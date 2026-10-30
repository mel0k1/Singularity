using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Numerics;
using System.Runtime.InteropServices;
using System.Runtime.CompilerServices;
using Robust.Shared.GameObjects;
using Robust.Shared.Log;
using Robust.Shared.Map;
using Robust.Shared.Maths;
using Robust.Shared.Physics.Collision.Shapes;
using Robust.Shared.Physics.Components;
using Robust.Shared.Physics.Dynamics;
using Robust.Shared.Physics.Events;
using Robust.Shared.Physics.Systems;
using Robust.Shared.Utility;

namespace Robust.Shared.Physics.Backend;

public static class RapierPhysicsMetrics
{
    public static long ApplyCalls;
    public static double ApplyMicros;
    public static long StepCalls;
    public static double StepMicros;
    public static long StatesCalls;
    public static double StatesMicros;
    public static long EventsCalls;
    public static double EventsMicros;
    public static long CommandsApplied;
    public static long StatesCopied;
    public static long EventsCopied;
    public static readonly long[] OpCounts = new long[16];

    public static void Reset()
    {
        ApplyCalls = 0;
        ApplyMicros = 0.0;
        StepCalls = 0;
        StepMicros = 0.0;
        StatesCalls = 0;
        StatesMicros = 0.0;
        EventsCalls = 0;
        EventsMicros = 0.0;
        CommandsApplied = 0;
        StatesCopied = 0;
        EventsCopied = 0;
        Array.Clear(OpCounts);
    }
}

internal sealed unsafe class RapierPhysicsBackend : IPhysicsBackend
{
    private const float PoseEpsilon = 1e-5f;
    private const float VelocityEpsilon = 1e-6f;

    public string Name => "rapier";

    private readonly SharedPhysicsSystem _physics;
    private readonly IEntityManager _entityManager;
    private readonly SharedTransformSystem _transform;
    private readonly ISawmill _sawmill;
    private readonly EntityQuery<PhysicsComponent> _physicsQuery;
    private readonly EntityQuery<FixturesComponent> _fixturesQuery;
    private readonly EntityQuery<TransformComponent> _xformQuery;

    private readonly Dictionary<MapId, IntPtr> _worlds = new();
    private readonly Dictionary<MapId, HashSet<ulong>> _mapBodies = new();
    private readonly Dictionary<MapId, Vector2> _mirrorGravity = new();
    private readonly Dictionary<ulong, MapId> _bodyMap = new();
    private readonly Dictionary<ulong, EntityUid> _bodyUids = new();
    private readonly Dictionary<ulong, MirrorState> _mirror = new();
    private readonly Dictionary<EntityUid, Dictionary<string, MirrorFixture>> _mirrorFixtures = new();
    private readonly Dictionary<EntityUid, long> _fixtureStamps = new();
    private readonly Dictionary<ulong, FixtureRef> _colliderIds = new();
    private readonly Dictionary<MapId, List<BodyInput>> _pending = new();
    private readonly List<MapId> _activeMaps = new();
    private readonly HashSet<ulong> _seenIds = new();
    private readonly List<ulong> _staleBodies = new();
    private readonly List<string> _staleFixtures = new();

    private RapierNative.SgCommand[] _commands = GC.AllocateArray<RapierNative.SgCommand>(8192, pinned: true);
    private float[] _vertices = GC.AllocateArray<float>(65536, pinned: true);
    private RapierNative.SgBodyState[] _states = GC.AllocateArray<RapierNative.SgBodyState>(8192, pinned: true);
    private RapierNative.SgContactEvent[] _events = GC.AllocateArray<RapierNative.SgContactEvent>(8192, pinned: true);

    private int _commandCount;
    private int _vertexCount;
    private bool _verticesDirty;
    private ulong _nextColliderId = 1;
    private bool _warnedUnsupportedShape;

    public RapierPhysicsBackend(SharedPhysicsSystem physics, IEntityManager entityManager)
    {
        _physics = physics;
        _entityManager = entityManager;
        _transform = _entityManager.System<SharedTransformSystem>();
        _physicsQuery = _entityManager.GetEntityQuery<PhysicsComponent>();
        _fixturesQuery = _entityManager.GetEntityQuery<FixturesComponent>();
        _xformQuery = _entityManager.GetEntityQuery<TransformComponent>();
        _sawmill = Logger.GetSawmill("physics.rapier");
        RapierNative.EnsureLoaded();
        var abi = RapierNative.sg_abi_version();
        if (abi != RapierNative.AbiVersion)
            throw new InvalidOperationException($"physics ffi abi {abi} != {RapierNative.AbiVersion}");
    }

    public void Step(float frameTime)
    {
        CollectBodies();

        _activeMaps.Clear();
        foreach (var map in _worlds.Keys)
        {
            _activeMaps.Add(map);
        }

        foreach (var map in _pending.Keys)
        {
            if (!_worlds.ContainsKey(map) && _pending[map].Count > 0)
                _activeMaps.Add(map);
        }

        foreach (var map in _activeMaps)
        {
            if (!_pending.TryGetValue(map, out var bodies) || bodies.Count == 0)
            {
                DestroyWorld(map);
                continue;
            }

            StepMap(map, bodies, frameTime);
        }
    }

    public void Shutdown()
    {
        foreach (var world in _worlds.Values)
        {
            RapierNative.sg_world_destroy(world);
        }

        _worlds.Clear();
        foreach (var map in _mapBodies.Keys.ToList())
        {
            foreach (var bodyId in _mapBodies[map])
            {
                ForgetBody(bodyId, map);
            }
        }

        _mapBodies.Clear();
        _mirrorGravity.Clear();
        _bodyMap.Clear();
        _bodyUids.Clear();
        _mirror.Clear();
        _mirrorFixtures.Clear();
        _colliderIds.Clear();
        foreach (var list in _pending.Values)
        {
            list.Clear();
        }

        _pending.Clear();
        _activeMaps.Clear();
        _seenIds.Clear();
        _staleBodies.Clear();
        _staleFixtures.Clear();
    }

    private void CollectBodies()
    {
        foreach (var list in _pending.Values)
        {
            list.Clear();
        }

        var enumerator = _entityManager.AllEntityQueryEnumerator<PhysicsComponent, FixturesComponent, TransformComponent>();
        while (enumerator.MoveNext(out var uid, out var body, out var fixtures, out var xform))
        {
            if (!body.CanCollide)
                continue;

            if (xform.MapID == MapId.Nullspace)
                continue;

            if (!_pending.TryGetValue(xform.MapID, out var list))
            {
                list = new List<BodyInput>();
                _pending[xform.MapID] = list;
            }

            list.Add(new BodyInput(uid, body, fixtures, xform));
        }

        enumerator.Dispose();
    }

    private void DestroyWorld(MapId map)
    {
        if (!_worlds.TryGetValue(map, out var world))
            return;

        RapierNative.sg_world_destroy(world);
        _worlds.Remove(map);
        _mirrorGravity.Remove(map);
        if (_mapBodies.TryGetValue(map, out var ids))
        {
            foreach (var bodyId in ids)
            {
                ForgetBody(bodyId, map);
            }

            _mapBodies.Remove(map);
        }
    }

    private void StepMap(MapId map, List<BodyInput> bodies, float frameTime)
    {
        if (!_worlds.TryGetValue(map, out var world))
        {
            world = RapierNative.sg_world_create();
            _worlds[map] = world;
            _mapBodies[map] = new HashSet<ulong>();
        }

        var mapSet = _mapBodies[map];

        _commandCount = 0;
        _vertexCount = 0;
        _verticesDirty = false;
        _seenIds.Clear();
        foreach (var input in bodies)
        {
            _seenIds.Add((ulong)input.Uid.Id);
        }

        _staleBodies.Clear();
        foreach (var bodyId in mapSet)
        {
            if (!_seenIds.Contains(bodyId))
                _staleBodies.Add(bodyId);
        }

        foreach (var bodyId in _staleBodies)
        {
            Push(RapierNative.OpRemoveBody, bodyId, 0, 0);
            mapSet.Remove(bodyId);
            ForgetBody(bodyId, map);
        }

        var gravity = _physics.Gravity;
        if (!_mirrorGravity.TryGetValue(map, out var mirrored) || mirrored != gravity)
        {
            Push(RapierNative.OpSetGravity, 0, 0, 0, gravity.X, gravity.Y);
            _mirrorGravity[map] = gravity;
        }

        foreach (var input in bodies)
        {
            SyncBody(map, input);
        }

        fixed (float* vertexPtr = _vertices)
        {
            if (_verticesDirty)
            {
                var t0 = Stopwatch.GetTimestamp();
                RapierNative.sg_world_set_vertices(world, vertexPtr, (UIntPtr)_vertexCount);
                RapierPhysicsMetrics.ApplyMicros += ElapsedMicros(t0);
                RapierPhysicsMetrics.ApplyCalls++;
            }

            fixed (RapierNative.SgCommand* commandPtr = _commands)
            {
                var t1 = Stopwatch.GetTimestamp();
                RapierNative.sg_world_apply(world, commandPtr, (UIntPtr)_commandCount);
                RapierPhysicsMetrics.ApplyMicros += ElapsedMicros(t1);
                RapierPhysicsMetrics.ApplyCalls++;
                RapierPhysicsMetrics.CommandsApplied += _commandCount;
            }
        }

        var t2 = Stopwatch.GetTimestamp();
        RapierNative.sg_world_step(world, frameTime);
        RapierPhysicsMetrics.StepMicros += ElapsedMicros(t2);
        RapierPhysicsMetrics.StepCalls++;

        Writeback(world);
        RaiseEvents(world);
    }

    private void SyncBody(MapId map, in BodyInput input)
    {
        var uid = input.Uid;
        var body = input.Body;
        var fixtures = input.Fixtures;
        var xform = input.Xform;
        var bodyId = (ulong)uid.Id;
        var kind = KindOf(body.BodyType);
        var mapSet = _mapBodies[map];
        var known = _bodyUids.TryGetValue(bodyId, out var knownUid) && knownUid == uid && mapSet.Contains(bodyId);
        if (!known)
        {
            if (knownUid != default && knownUid != uid && mapSet.Contains(bodyId))
            {
                Push(RapierNative.OpRemoveBody, bodyId, 0, 0);
                mapSet.Remove(bodyId);
                ForgetBody(bodyId, map);
            }

            var spawnPosition = _transform.GetWorldPosition(xform);
            var spawnRotation = (float)_transform.GetWorldRotation(xform).Theta;
            Push(RapierNative.OpAddBody, bodyId, kind, 0, spawnPosition.X, spawnPosition.Y, spawnRotation);
            _bodyUids[bodyId] = uid;
            _bodyMap[bodyId] = map;
            _mirrorFixtures[uid] = new Dictionary<string, MirrorFixture>();
            mapSet.Add(bodyId);
            _mirror[bodyId] = MirrorState.Fresh();
        }

        _mirror.TryGetValue(bodyId, out var mirror);

        var position = _transform.GetWorldPosition(xform);
        var rotation = (float)_transform.GetWorldRotation(xform).Theta;

        if (mirror.Kind != kind)
        {
            Push(RapierNative.OpSetBodyKind, bodyId, kind, 0);
            mirror.Kind = kind;
        }

        if (MathF.Abs(position.X - mirror.X) > PoseEpsilon
            || MathF.Abs(position.Y - mirror.Y) > PoseEpsilon
            || MathF.Abs(rotation - mirror.Angle) > PoseEpsilon)
        {
            var wake = kind != RapierNative.KindStatic && !mirror.Awake;
            Push(
                RapierNative.OpSetTransform,
                bodyId,
                0,
                0,
                position.X,
                position.Y,
                rotation,
                wake ? 1.0f : 0.0f);
            mirror.X = position.X;
            mirror.Y = position.Y;
            mirror.Angle = rotation;
        }

        if (body.FixedRotation != mirror.FixedRotation)
        {
            Push(RapierNative.OpSetFixedRotation, bodyId, (ulong)(body.FixedRotation ? 1 : 0), 0);
            mirror.FixedRotation = body.FixedRotation;
        }

        if (kind != RapierNative.KindStatic)
        {
            if (body.Awake
                && (MathF.Abs(body.LinearVelocity.X - mirror.Vx) > VelocityEpsilon
                    || MathF.Abs(body.LinearVelocity.Y - mirror.Vy) > VelocityEpsilon
                    || MathF.Abs(body.AngularVelocity - mirror.Omega) > VelocityEpsilon))
            {
                Push(
                    RapierNative.OpSetVelocity,
                    bodyId,
                    0,
                    0,
                    body.LinearVelocity.X,
                    body.LinearVelocity.Y,
                    body.AngularVelocity);
                mirror.Vx = body.LinearVelocity.X;
                mirror.Vy = body.LinearVelocity.Y;
                mirror.Omega = body.AngularVelocity;
            }

            if (body.Force.X != mirror.Fx || body.Force.Y != mirror.Fy || body.Torque != mirror.Torque)
            {
                Push(
                    RapierNative.OpSetForce,
                    bodyId,
                    0,
                    0,
                    body.Force.X,
                    body.Force.Y,
                    body.Torque);
                mirror.Fx = body.Force.X;
                mirror.Fy = body.Force.Y;
                mirror.Torque = body.Torque;
            }

            if (body.LinearDamping != mirror.LinDamp || body.AngularDamping != mirror.AngDamp)
            {
                Push(RapierNative.OpSetDamping, bodyId, 0, 0, body.LinearDamping, body.AngularDamping);
                mirror.LinDamp = body.LinearDamping;
                mirror.AngDamp = body.AngularDamping;
            }

            if (body.Awake != mirror.Awake)
            {
                Push(RapierNative.OpSetAwake, bodyId, (ulong)(body.Awake ? 1 : 0), 0);
                mirror.Awake = body.Awake;
            }
            else if (body.Awake && !body.SleepingAllowed)
            {
                Push(RapierNative.OpSetAwake, bodyId, 1, 0);
            }
        }
        else
        {
            mirror.Awake = true;
        }

        SyncFixtures(uid, bodyId, fixtures);
        _mirror[bodyId] = mirror;
    }

    private void SyncFixtures(EntityUid uid, ulong bodyId, FixturesComponent fixtures)
    {
        var stamp = FixtureStamp(fixtures);
        if (_fixtureStamps.TryGetValue(uid, out var knownStamp) && knownStamp == stamp)
            return;

        _fixtureStamps[uid] = stamp;
        var mirror = _mirrorFixtures[uid];
        _staleFixtures.Clear();

        foreach (var (fixtureId, fixture) in fixtures.Fixtures)
        {
            var fingerprint = Fingerprint(fixture);
            if (mirror.TryGetValue(fixtureId, out var existing) && existing.Fingerprint == fingerprint)
                continue;

            if (existing.Fingerprint != 0)
            {
                Push(RapierNative.OpRemoveCollider, existing.ColliderId, 0, 0);
                _colliderIds.Remove(existing.ColliderId);
            }

            if (!TryPushCollider(bodyId, fixture, fingerprint, out var colliderId))
                continue;

            mirror[fixtureId] = new MirrorFixture(colliderId, fingerprint);
            _colliderIds[colliderId] = new FixtureRef(uid, fixtureId);
        }

        foreach (var (fixtureId, _) in mirror)
        {
            if (!fixtures.Fixtures.ContainsKey(fixtureId))
                _staleFixtures.Add(fixtureId);
        }

        foreach (var fixtureId in _staleFixtures)
        {
            var existing = mirror[fixtureId];
            if (existing.Fingerprint != 0)
            {
                Push(RapierNative.OpRemoveCollider, existing.ColliderId, 0, 0);
                _colliderIds.Remove(existing.ColliderId);
            }

            mirror.Remove(fixtureId);
        }
    }

    private bool TryPushCollider(ulong bodyId, Fixture fixture, uint fingerprint, out ulong colliderId)
    {
        colliderId = 0;
        var groups = ((ulong)(uint)fixture.CollisionLayer << 32) | (uint)fixture.CollisionMask;

        switch (fixture.Shape)
        {
            case PolygonShape polygon:
            {
                var vertices = polygon.Vertices;
                if (vertices.Length < 3)
                    return false;

                if (vertices.Length == 4 && TryAxisAlignedBox(vertices, out var center, out var half))
                {
                    colliderId = _nextColliderId++;
                    Push(
                        RapierNative.OpAddColliderBox,
                        colliderId,
                        bodyId,
                        groups,
                        fixture.Friction,
                        fixture.Restitution,
                        fixture.Density,
                        fixture.Hard ? 0.0f : 1.0f,
                        half.X,
                        half.Y,
                        center.X,
                        center.Y);
                    return true;
                }

                var offset = PushVertexStart();
                foreach (var vertex in vertices)
                {
                    PushVertex(vertex);
                }

                colliderId = _nextColliderId++;
                Push(
                    RapierNative.OpAddColliderPoly,
                    colliderId,
                    bodyId,
                    groups,
                    fixture.Friction,
                    fixture.Restitution,
                    fixture.Density,
                    fixture.Hard ? 0.0f : 1.0f,
                    offset,
                    vertices.Length);
                _verticesDirty = true;
                return true;
            }
            case PhysShapeCircle circle:
            {
                colliderId = _nextColliderId++;
                Push(
                    RapierNative.OpAddColliderCircle,
                    colliderId,
                    bodyId,
                    groups,
                    fixture.Friction,
                    fixture.Restitution,
                    fixture.Density,
                    fixture.Hard ? 0.0f : 1.0f,
                    circle.Radius,
                    circle.Position.X,
                    circle.Position.Y);
                return true;
            }
            case PhysShapeAabb aabb:
            {
                var bounds = aabb.LocalBounds;
                colliderId = _nextColliderId++;
                Push(
                    RapierNative.OpAddColliderBox,
                    colliderId,
                    bodyId,
                    groups,
                    fixture.Friction,
                    fixture.Restitution,
                    fixture.Density,
                    fixture.Hard ? 0.0f : 1.0f,
                    bounds.Width * 0.5f + aabb.Radius,
                    bounds.Height * 0.5f + aabb.Radius,
                    bounds.Center.X,
                    bounds.Center.Y);
                return true;
            }
            default:
                if (!_warnedUnsupportedShape)
                {
                    _warnedUnsupportedShape = true;
                    _sawmill.Warning($"unsupported fixture shape {fixture.Shape?.GetType().Name ?? "null"} skipped");
                }

                return false;
        }
    }

    private void Writeback(IntPtr world)
    {
        var t0 = Stopwatch.GetTimestamp();
        var count = (int)RapierNative.sg_world_state_count(world);
        RapierPhysicsMetrics.StatesMicros += ElapsedMicros(t0);
        RapierPhysicsMetrics.StatesCalls++;
        if (count == 0)
            return;

        EnsureStates(count);
        fixed (RapierNative.SgBodyState* statePtr = _states)
        {
            var t1 = Stopwatch.GetTimestamp();
            count = (int)RapierNative.sg_world_copy_states(world, statePtr, (UIntPtr)count);
            RapierPhysicsMetrics.StatesMicros += ElapsedMicros(t1);
            RapierPhysicsMetrics.StatesCalls++;
        }

        RapierPhysicsMetrics.StatesCopied += count;

        for (var i = 0; i < count; i++)
        {
            ref var state = ref _states[i];
            if (state.Kind == (uint)RapierNative.KindStatic)
                continue;

            if (!_bodyUids.TryGetValue(state.Id, out var uid) || !_physicsQuery.TryGetComponent(uid, out var body))
                continue;

            _mirror.TryGetValue(state.Id, out var mirror);
            var asleep = (state.Flags & RapierNative.FlagAsleep) != 0;
            mirror.Awake = !asleep;
            mirror.X = state.X;
            mirror.Y = state.Y;
            mirror.Angle = state.Angle;
            mirror.Vx = state.Vx;
            mirror.Vy = state.Vy;
            mirror.Omega = state.Omega;

            if (asleep == body.Awake && body.SleepingAllowed)
                _physics.SetAwake(uid, body, !asleep);

            if (asleep)
            {
                mirror.Vx = 0.0f;
                mirror.Vy = 0.0f;
                mirror.Omega = 0.0f;
            }
            else if (!float.IsNaN(state.Vx) && !float.IsNaN(state.Vy) && !float.IsNaN(state.Omega))
            {
                _physics.BackendWriteVelocities(uid, body, new Vector2(state.Vx, state.Vy), state.Omega);
            }

            if (!float.IsNaN(state.X)
                && !float.IsNaN(state.Y)
                && _xformQuery.TryGetComponent(uid, out var xform))
            {
                var current = _transform.GetWorldPosition(xform);
                var currentRotation = (float)_transform.GetWorldRotation(xform).Theta;
                if (MathF.Abs(state.X - current.X) > PoseEpsilon
                    || MathF.Abs(state.Y - current.Y) > PoseEpsilon
                    || MathF.Abs(state.Angle - currentRotation) > PoseEpsilon)
                {
                    _transform.SetLocalPositionRotation(
                        uid,
                        xform.LocalPosition + new Vector2(state.X - current.X, state.Y - current.Y),
                        xform.LocalRotation + (state.Angle - currentRotation),
                        xform);
                }
            }

            _mirror[state.Id] = mirror;
        }
    }

    private void RaiseEvents(IntPtr world)
    {
        var t0 = Stopwatch.GetTimestamp();
        var count = (int)RapierNative.sg_world_event_count(world);
        RapierPhysicsMetrics.EventsMicros += ElapsedMicros(t0);
        RapierPhysicsMetrics.EventsCalls++;
        if (count == 0)
            return;

        EnsureEvents(count);
        fixed (RapierNative.SgContactEvent* eventPtr = _events)
        {
            var t1 = Stopwatch.GetTimestamp();
            count = (int)RapierNative.sg_world_copy_events(world, eventPtr, (UIntPtr)count);
            RapierPhysicsMetrics.EventsMicros += ElapsedMicros(t1);
            RapierPhysicsMetrics.EventsCalls++;
        }

        RapierPhysicsMetrics.EventsCopied += count;

        for (var i = 0; i < count; i++)
        {
            ref var e = ref _events[i];
            if (!_colliderIds.TryGetValue(e.A, out var a) || !_colliderIds.TryGetValue(e.B, out var b))
                continue;

            if (!_physicsQuery.TryGetComponent(a.Uid, out var bodyA)
                || !_physicsQuery.TryGetComponent(b.Uid, out var bodyB)
                || !_fixturesQuery.TryGetComponent(a.Uid, out var fixturesA)
                || !_fixturesQuery.TryGetComponent(b.Uid, out var fixturesB)
                || !fixturesA.Fixtures.TryGetValue(a.FixtureId, out var fixtureA)
                || !fixturesB.Fixtures.TryGetValue(b.FixtureId, out var fixtureB))
                continue;

            if (e.Started != 0)
            {
                var points = new FixedArray2<Vector2>(
                    new Vector2(e.P0x, e.P0y),
                    new Vector2(e.P1x, e.P1y));
                var normal = new Vector2(e.Nx, e.Ny);
                var pointCount = (int)e.PointCount;
                var evA = new StartCollideEvent(
                    a.Uid,
                    b.Uid,
                    a.FixtureId,
                    b.FixtureId,
                    fixtureA,
                    fixtureB,
                    bodyA,
                    bodyB,
                    points,
                    pointCount,
                    normal);
                var evB = new StartCollideEvent(
                    b.Uid,
                    a.Uid,
                    b.FixtureId,
                    a.FixtureId,
                    fixtureB,
                    fixtureA,
                    bodyB,
                    bodyA,
                    points,
                    pointCount,
                    normal);
                _physics.BackendRaiseLocal(a.Uid, ref evA, true);
                _physics.BackendRaiseLocal(b.Uid, ref evB, true);
            }
            else
            {
                var evA = new EndCollideEvent(a.Uid, b.Uid, a.FixtureId, b.FixtureId, fixtureA, fixtureB, bodyA, bodyB);
                var evB = new EndCollideEvent(b.Uid, a.Uid, b.FixtureId, a.FixtureId, fixtureB, fixtureA, bodyB, bodyA);
                _physics.BackendRaiseLocal(a.Uid, ref evA);
                _physics.BackendRaiseLocal(b.Uid, ref evB);
            }
        }
    }

    private void ForgetBody(ulong bodyId, MapId map)
    {
        if (_bodyMap.TryGetValue(bodyId, out var owner) && owner != map)
            return;

        _bodyMap.Remove(bodyId);
        if (_bodyUids.TryGetValue(bodyId, out var uid))
        {
            if (_mirrorFixtures.TryGetValue(uid, out var fixtures))
            {
                foreach (var fixture in fixtures.Values)
                {
                    _colliderIds.Remove(fixture.ColliderId);
                }

                _mirrorFixtures.Remove(uid);
            }

            _fixtureStamps.Remove(uid);
            _bodyUids.Remove(bodyId);
        }
        _mirror.Remove(bodyId);
    }

    private int PushVertexStart()
    {
        return _vertexCount / 2;
    }

    private void PushVertex(Vector2 vertex)
    {
        if (_vertexCount + 2 > _vertices.Length)
        {
            var grown = GC.AllocateArray<float>(_vertices.Length * 2, pinned: true);
            Array.Copy(_vertices, grown, _vertices.Length);
            _vertices = grown;
        }

        _vertices[_vertexCount++] = vertex.X;
        _vertices[_vertexCount++] = vertex.Y;
    }

    private void Push(
        uint op,
        ulong id,
        ulong id2,
        ulong id3,
        float p0 = 0.0f,
        float p1 = 0.0f,
        float p2 = 0.0f,
        float p3 = 0.0f,
        float p4 = 0.0f,
        float p5 = 0.0f,
        float p6 = 0.0f,
        float p7 = 0.0f)
    {
        if (_commandCount == _commands.Length)
        {
            var grown = GC.AllocateArray<RapierNative.SgCommand>(_commands.Length * 2, pinned: true);
            Array.Copy(_commands, grown, _commands.Length);
            _commands = grown;
        }

        _commands[_commandCount++] = new RapierNative.SgCommand
        {
            Op = op,
            Reserved = 0,
            Id = id,
            Id2 = id2,
            Id3 = id3,
            P0 = p0,
            P1 = p1,
            P2 = p2,
            P3 = p3,
            P4 = p4,
            P5 = p5,
            P6 = p6,
            P7 = p7,
        };
        RapierPhysicsMetrics.OpCounts[op & 15]++;
    }

    private void EnsureStates(int count)
    {
        if (count > _states.Length)
        {
            var grown = GC.AllocateArray<RapierNative.SgBodyState>(Math.Max(count, _states.Length * 2), pinned: true);
            _states = grown;
        }
    }

    private void EnsureEvents(int count)
    {
        if (count > _events.Length)
        {
            var grown = GC.AllocateArray<RapierNative.SgContactEvent>(Math.Max(count, _events.Length * 2), pinned: true);
            _events = grown;
        }
    }

    private static double ElapsedMicros(long start)
    {
        return (Stopwatch.GetTimestamp() - start) * 1_000_000.0 / Stopwatch.Frequency;
    }

    private static ulong KindOf(BodyType type)
    {
        return type switch
        {
            BodyType.Static => RapierNative.KindStatic,
            BodyType.Dynamic => RapierNative.KindDynamic,
            _ => RapierNative.KindKinematic,
        };
    }

    private static bool TryAxisAlignedBox(Vector2[] vertices, out Vector2 center, out Vector2 half)
    {
        center = Vector2.Zero;
        half = Vector2.Zero;
        var minX = float.MaxValue;
        var maxX = float.MinValue;
        var minY = float.MaxValue;
        var maxY = float.MinValue;
        foreach (var vertex in vertices)
        {
            minX = MathF.Min(minX, vertex.X);
            maxX = MathF.Max(maxX, vertex.X);
            minY = MathF.Min(minY, vertex.Y);
            maxY = MathF.Max(maxY, vertex.Y);
        }

        foreach (var vertex in vertices)
        {
            var onX = MathF.Abs(vertex.X - minX) < 1e-6f || MathF.Abs(vertex.X - maxX) < 1e-6f;
            var onY = MathF.Abs(vertex.Y - minY) < 1e-6f || MathF.Abs(vertex.Y - maxY) < 1e-6f;
            if (!onX || !onY)
                return false;
        }

        half = new Vector2((maxX - minX) * 0.5f, (maxY - minY) * 0.5f);
        if (half.X <= 0.0f || half.Y <= 0.0f)
            return false;

        center = new Vector2((minX + maxX) * 0.5f, (minY + maxY) * 0.5f);
        return true;
    }

    private static long FixtureStamp(FixturesComponent fixtures)
    {
        var stamp = (long)fixtures.Fixtures.Count;
        foreach (var pair in fixtures.Fixtures)
        {
            stamp = stamp * 31 + RuntimeHelpers.GetHashCode(pair.Value);
            stamp = stamp * 31 + RuntimeHelpers.GetHashCode(pair.Value.Shape);
        }

        return stamp;
    }

    private static uint Fingerprint(Fixture fixture)
    {
        var hash = 2166136261u;
        FoldU(fixture.Shape == null ? uint.MaxValue : (uint)fixture.Shape.ShapeType);
        FoldF(fixture.Friction);
        FoldF(fixture.Restitution);
        FoldF(fixture.Density);
        FoldU(fixture.Hard ? 1u : 0u);
        FoldU((uint)fixture.CollisionLayer);
        FoldU((uint)fixture.CollisionMask);

        switch (fixture.Shape)
        {
            case PolygonShape polygon:
                FoldU((uint)polygon.Vertices.Length);
                foreach (var vertex in polygon.Vertices)
                {
                    FoldF(vertex.X);
                    FoldF(vertex.Y);
                }

                break;
            case PhysShapeCircle circle:
                FoldF(circle.Radius);
                FoldF(circle.Position.X);
                FoldF(circle.Position.Y);
                break;
            case PhysShapeAabb aabb:
                FoldF(aabb.LocalBounds.Left);
                FoldF(aabb.LocalBounds.Right);
                FoldF(aabb.LocalBounds.Top);
                FoldF(aabb.LocalBounds.Bottom);
                FoldF(aabb.Radius);
                break;
        }

        return hash;

        void FoldF(float value)
        {
            hash = (hash ^ BitConverter.SingleToUInt32Bits(value)) * 16777619u;
        }

        void FoldU(uint value)
        {
            hash = (hash ^ value) * 16777619u;
        }
    }

    private struct MirrorState
    {
        public float X;
        public float Y;
        public float Angle;
        public float Vx;
        public float Vy;
        public float Omega;
        public float Fx;
        public float Fy;
        public float Torque;
        public float LinDamp;
        public float AngDamp;
        public ulong Kind;
        public bool FixedRotation;
        public bool Awake;

        public static MirrorState Fresh()
        {
            return new MirrorState
            {
                X = float.NegativeInfinity,
                Y = float.NegativeInfinity,
                Angle = float.NegativeInfinity,
                Vx = float.NegativeInfinity,
                Vy = float.NegativeInfinity,
                Omega = float.NegativeInfinity,
                Fx = float.NegativeInfinity,
                Fy = float.NegativeInfinity,
                Torque = float.NegativeInfinity,
                LinDamp = float.NegativeInfinity,
                AngDamp = float.NegativeInfinity,
                Kind = ulong.MaxValue,
                Awake = true,
            };
        }
    }

    private readonly struct FixtureRef
    {
        public readonly EntityUid Uid;
        public readonly string FixtureId;

        public FixtureRef(EntityUid uid, string fixtureId)
        {
            Uid = uid;
            FixtureId = fixtureId;
        }
    }

    private readonly struct MirrorFixture
    {
        public readonly ulong ColliderId;
        public readonly uint Fingerprint;

        public MirrorFixture(ulong colliderId, uint fingerprint)
        {
            ColliderId = colliderId;
            Fingerprint = fingerprint;
        }
    }

    private readonly struct BodyInput
    {
        public readonly EntityUid Uid;
        public readonly PhysicsComponent Body;
        public readonly FixturesComponent Fixtures;
        public readonly TransformComponent Xform;

        public BodyInput(EntityUid uid, PhysicsComponent body, FixturesComponent fixtures, TransformComponent xform)
        {
            Uid = uid;
            Body = body;
            Fixtures = fixtures;
            Xform = xform;
        }
    }
}
