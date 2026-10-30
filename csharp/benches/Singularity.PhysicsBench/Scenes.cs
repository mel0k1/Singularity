using System.Diagnostics;
using System.Numerics;
using Robust.Shared.GameObjects;
using Robust.Shared.Map;
using Robust.Shared.Physics;
using Robust.Shared.Physics.Backend;
using Robust.Shared.Physics.Collision.Shapes;
using Robust.Shared.Physics.Components;
using Robust.Shared.Physics.Dynamics.Joints;
using Robust.Shared.Physics.Events;
using Robust.Shared.Physics.Systems;
using Robust.UnitTesting.Server;

namespace Singularity.PhysicsBench;

public enum SceneKind
{
    Idle,
    Floor,
    Rain,
    Mover,
    Chain,
    Welds,
    Prevent,
}

public sealed record FfiStats(
    double ApplyMicros,
    double StepMicros,
    double StatesMicros,
    double EventsMicros,
    double OrchestrationMicros,
    long ApplyCalls,
    long StepCalls,
    long Commands,
    long StatesCopied,
    long EventsCopied,
    long PairFilterCalls,
    long JointsSeen,
    long JointCommands,
    long JointBreaks,
    long DirtySyncs,
    long AuditTicks,
    long AuditMisses,
    long FullSyncTicks,
    long[] OpCounts);

public sealed record Outcome(
    string Scene,
    string Backend,
    int Ticks,
    int Bodies,
    int Dynamics,
    int Asleep,
    long Contacts,
    ulong StateHash,
    float MinY,
    float CargoDx,
    double[] TickUs,
    long AllocBytes,
    int Gen0,
    int Gen1,
    int Gen2,
    int Joints,
    float WeldDrift,
    long PreventCalls,
    FfiStats? Ffi);

public static class Scenes
{
    public const float Dt = 1f / 60f;
    public const int RainSpawns = 300;
    public const float MoverSpeed = 1.5f;
    public const int ChainLinks = 12;
    public const int RopeBeads = 6;
    public const int WeldPairs = 16;
    public const int PreventDiscs = 40;

    public static string Name(SceneKind scene)
    {
        return scene switch
        {
            SceneKind.Idle => "idle",
            SceneKind.Floor => "floor",
            SceneKind.Rain => "rain",
            SceneKind.Mover => "mover",
            SceneKind.Chain => "chain",
            SceneKind.Welds => "welds",
            SceneKind.Prevent => "prevent",
            _ => throw new ArgumentOutOfRangeException(nameof(scene)),
        };
    }

    public static bool TryParse(string name, out SceneKind scene)
    {
        switch (name)
        {
            case "idle": scene = SceneKind.Idle; return true;
            case "floor": scene = SceneKind.Floor; return true;
            case "rain": scene = SceneKind.Rain; return true;
            case "mover": scene = SceneKind.Mover; return true;
            case "chain": scene = SceneKind.Chain; return true;
            case "welds": scene = SceneKind.Welds; return true;
            case "prevent": scene = SceneKind.Prevent; return true;
            default: scene = SceneKind.Idle; return false;
        }
    }

    private static int Seed(SceneKind scene)
    {
        return scene switch
        {
            SceneKind.Rain => 2022,
            SceneKind.Mover => 3033,
            SceneKind.Chain => 4044,
            SceneKind.Welds => 5055,
            SceneKind.Prevent => 6066,
            _ => 1011,
        };
    }

    public static Outcome Run(SceneKind scene, int ticks, string backend = "farseer")
    {
        var sim = RobustServerSimulation.NewSimulation()
            .ChangeCVar(cfg =>
            {
                cfg.SetCVar("physics.target_minimum_tickrate", 30);
                cfg.SetCVar("physics.backend", backend);
            })
            .RegisterEntitySystems(factory =>
            {
                factory.LoadExtraSystemType<ContactCounterSystem>();
                factory.LoadExtraSystemType<PreventCollideCancellerSystem>();
            })
            .InitializeInstance();
        var ent = sim.Resolve<IEntityManager>();
        var physics = ent.System<SharedPhysicsSystem>();
        var activeBackend = physics.PhysicsBackendName;
        if (backend != "farseer" && activeBackend != backend)
            throw new InvalidOperationException($"physics backend '{backend}' did not activate (native library missing?)");
        var fixtures = ent.System<FixtureSystem>();
        var joints = ent.System<SharedJointSystem>();
        var transform = ent.System<SharedTransformSystem>();
        var mapSys = ent.System<SharedMapSystem>();
        mapSys.CreateMap(out var mapId);
        physics.SetGravity(new Vector2(0f, -9.81f));
        ContactCounterSystem.Starts = 0;
        PreventCollideCancellerSystem.Calls = 0;
        PreventCollideCancellerSystem.Enabled = scene == SceneKind.Prevent;
        RapierPhysicsMetrics.Reset();

        var rng = new Rng((ulong)Seed(scene));
        var ids = new List<EntityUid>();
        var dyn = new List<EntityUid>();
        var cargo = new List<(EntityUid Uid, float X0)>();
        var chainBodies = new List<EntityUid>();
        var weldPairs = new List<(EntityUid A, EntityUid B)>();
        EntityUid platform = default;

        if (scene is SceneKind.Floor or SceneKind.Idle)
        {
            platform = AddPlatform(ent, physics, fixtures, mapId, 64, 8, 0f, BodyType.Static);
            ids.Add(platform);
            ids.Add(AddWall(ent, physics, fixtures, mapId, -32.75f));
            ids.Add(AddWall(ent, physics, fixtures, mapId, 32.75f));
            if (scene == SceneKind.Floor)
            {
                for (var row = 0; row < 12; row++)
                {
                    var count = 12 - row;
                    for (var i = 0; i < count; i++)
                    {
                        var x = (i - (count - 1) * 0.5f) * 0.92f;
                        var y = 0.45f + row * 0.9f;
                        var uid = AddBox(ent, physics, fixtures, mapId, new Vector2(x, y), 0.45f, 0.8f);
                        ids.Add(uid);
                        dyn.Add(uid);
                    }
                }
                for (var d = 0; d < 60; d++)
                {
                    var x = rng.NextF32(-30f, 30f);
                    var y = rng.NextF32(4f, 30f);
                    var uid = AddDisc(ent, physics, fixtures, mapId, new Vector2(x, y), 0.35f, 0.6f);
                    ids.Add(uid);
                    dyn.Add(uid);
                }
            }
        }
        else if (scene == SceneKind.Rain)
        {
            platform = AddPlatform(ent, physics, fixtures, mapId, 16, 8, 0f, BodyType.Static);
            ids.Add(platform);
            ids.Add(AddWall(ent, physics, fixtures, mapId, -8.75f));
            ids.Add(AddWall(ent, physics, fixtures, mapId, 8.75f));
        }
        else if (scene == SceneKind.Chain)
        {
            var anchor = AddBox(ent, physics, fixtures, mapId, new Vector2(0f, 20f), 0.5f, 0.9f);
            physics.SetBodyType(anchor, BodyType.Static, body: ent.GetComponent<PhysicsComponent>(anchor));
            ids.Add(anchor);
            EntityUid previous = anchor;
            for (var i = 0; i < ChainLinks; i++)
            {
                var uid = AddBox(ent, physics, fixtures, mapId, new Vector2(0f, 19f - i * 0.95f), 0.45f, 0.8f);
                ids.Add(uid);
                dyn.Add(uid);
                chainBodies.Add(uid);
                var joint = joints.CreateRevoluteJoint(previous, uid, id: $"chain{i}");
                joint.LocalAnchorA = new Vector2(0f, i == 0 ? -0.5f : -0.475f);
                joint.LocalAnchorB = new Vector2(0f, 0.475f);
                joint.CollideConnected = false;
                previous = uid;
            }

            for (var i = 0; i < RopeBeads; i++)
            {
                var x = 3f;
                var y = 19.5f - i * 0.6f;
                var uid = AddDisc(ent, physics, fixtures, mapId, new Vector2(x, y), 0.28f, 0.6f);
                ids.Add(uid);
                dyn.Add(uid);
                chainBodies.Add(uid);
                if (i == 0)
                {
                    var rope = joints.CreateDistanceJoint(anchor, uid, id: $"rope{i}");
                    rope.CollideConnected = false;
                }
                else
                {
                    var prev = ids[^2];
                    var rope = joints.CreateDistanceJoint(prev, uid, id: $"rope{i}");
                    rope.CollideConnected = false;
                }
            }
        }
        else if (scene == SceneKind.Welds)
        {
            platform = AddPlatform(ent, physics, fixtures, mapId, 40, 4, 0f, BodyType.Static);
            ids.Add(platform);
            for (var p = 0; p < WeldPairs; p++)
            {
                var x = (p - WeldPairs * 0.5f + 0.5f) * 2.2f;
                var y = 1.6f + (p % 4) * 1.3f;
                var a = AddBox(ent, physics, fixtures, mapId, new Vector2(x, y), 0.45f, 0.8f);
                var b = AddBox(ent, physics, fixtures, mapId, new Vector2(x + 0.95f, y), 0.45f, 0.8f);
                transform.SetWorldRotation(b, MathF.PI / 2f);
                ids.Add(a);
                ids.Add(b);
                dyn.Add(a);
                dyn.Add(b);
                var weld = joints.CreateWeldJoint(a, b, id: $"weld{p}");
                weld.LocalAnchorA = new Vector2(0.475f, 0f);
                weld.LocalAnchorB = new Vector2(0f, 0.475f);
                weld.ReferenceAngle = MathF.PI / 2f;
                weld.CollideConnected = false;
                weldPairs.Add((a, b));
            }
        }
        else if (scene == SceneKind.Prevent)
        {
            platform = AddPlatform(ent, physics, fixtures, mapId, 16, 4, 0f, BodyType.Static);
            ids.Add(platform);
            for (var d = 0; d < PreventDiscs; d++)
            {
                var x = rng.NextF32(-6f, 6f);
                var y = 3f + d * 0.9f;
                var uid = AddDisc(ent, physics, fixtures, mapId, new Vector2(x, y), 0.35f, 0.6f);
                ids.Add(uid);
                dyn.Add(uid);
            }
        }
        else
        {
            platform = AddPlatform(ent, physics, fixtures, mapId, 16, 2, 0f, BodyType.KinematicController);
            ids.Add(platform);
            for (var row = 0; row < 2; row++)
            {
                for (var i = 0; i < 16; i++)
                {
                    var x = (i - 7.5f) * 0.92f;
                    var y = 0.45f + row * 0.9f;
                    var uid = AddBox(ent, physics, fixtures, mapId, new Vector2(x, y), 0.45f, 0.9f);
                    ids.Add(uid);
                    dyn.Add(uid);
                    cargo.Add((uid, x));
                }
            }
        }

        GC.Collect();
        GC.WaitForPendingFinalizers();
        GC.Collect();
        var allocBefore = GC.GetAllocatedBytesForCurrentThread();
        var gen0Before = GC.CollectionCount(0);
        var gen1Before = GC.CollectionCount(1);
        var gen2Before = GC.CollectionCount(2);

        var tickUs = new double[ticks];
        var watch = Stopwatch.StartNew();
        for (var t = 0; t < ticks; t++)
        {
            if (scene == SceneKind.Rain && t < RainSpawns)
            {
                var x = rng.NextF32(-6f, 6f);
                var y = 25f + rng.NextF32(0f, 4f);
                var uid = (t & 1) == 0
                    ? AddDisc(ent, physics, fixtures, mapId, new Vector2(x, y), 0.4f, 0.6f)
                    : AddBox(ent, physics, fixtures, mapId, new Vector2(x, y), 0.4f, 0.6f);
                ids.Add(uid);
                dyn.Add(uid);
            }
            if (scene == SceneKind.Mover && t == 0)
            {
                physics.SetLinearVelocity(platform, new Vector2(MoverSpeed, 0f));
            }
            watch.Restart();
            ent.TickUpdate(Dt, false);
            watch.Stop();
            tickUs[t] = watch.Elapsed.TotalMicroseconds;
        }

        var allocBytes = GC.GetAllocatedBytesForCurrentThread() - allocBefore;
        var gen0 = GC.CollectionCount(0) - gen0Before;
        var gen1 = GC.CollectionCount(1) - gen1Before;
        var gen2 = GC.CollectionCount(2) - gen2Before;

        var hash = Fnv.Basis;
        foreach (var id in ids)
        {
            var body = ent.GetComponent<PhysicsComponent>(id);
            var xform = ent.GetComponent<TransformComponent>(id);
            var pos = xform.WorldPosition;
            hash = Fnv.Fold(hash, (uint)id.Id);
            hash = Fnv.Fold(hash, (uint)BitConverter.SingleToInt32Bits(pos.X));
            hash = Fnv.Fold(hash, (uint)BitConverter.SingleToInt32Bits(pos.Y));
            hash = Fnv.Fold(hash, (uint)BitConverter.SingleToInt32Bits((float)xform.WorldRotation.Theta));
            hash = Fnv.Fold(hash, (uint)BitConverter.SingleToInt32Bits(body.LinearVelocity.X));
            hash = Fnv.Fold(hash, (uint)BitConverter.SingleToInt32Bits(body.LinearVelocity.Y));
            hash = Fnv.Fold(hash, (uint)BitConverter.SingleToInt32Bits(body.AngularVelocity));
            hash = Fnv.Fold(hash, (uint)(body.Awake ? 0 : 1));
        }

        var asleep = 0;
        var minY = float.MaxValue;
        foreach (var id in dyn)
        {
            var body = ent.GetComponent<PhysicsComponent>(id);
            var y = ent.GetComponent<TransformComponent>(id).WorldPosition.Y;
            if (!body.Awake) asleep++;
            if (y < minY) minY = y;
        }
        if (dyn.Count == 0) minY = 0f;

        if (Environment.GetEnvironmentVariable("SINGULARITY_BENCH_DUMP") == "1")
        {
            foreach (var id in dyn)
            {
                var pos = ent.GetComponent<TransformComponent>(id).WorldPosition;
                if (pos.Y < -1f)
                {
                    var dumpedJoints = ent.TryGetComponent<JointComponent>(id, out var jc) ? jc.JointCount : 0;
                    Console.WriteLine($"  dump {id} at {pos.X:F2},{pos.Y:F2} joints {dumpedJoints}");
                }
            }
        }

        var cargoDx = 0f;
        if (cargo.Count > 0)
        {
            var sum = 0f;
            foreach (var (uid, x0) in cargo)
            {
                sum += ent.GetComponent<TransformComponent>(uid).WorldPosition.X - x0;
            }
            cargoDx = sum / cargo.Count;
        }

        var jointCount = 0;
        foreach (var id in ids)
        {
            if (ent.TryGetComponent<JointComponent>(id, out var jointComponent))
                jointCount += jointComponent.JointCount;
        }
        jointCount /= 2;

        var weldDrift = 0f;
        if (weldPairs.Count > 0)
        {
            var sum = 0f;
            foreach (var (a, b) in weldPairs)
            {
                var angleA = ent.GetComponent<TransformComponent>(a).WorldRotation.Theta;
                var angleB = ent.GetComponent<TransformComponent>(b).WorldRotation.Theta;
                var relative = (float)(angleB - angleA);
                sum += MathF.Abs(Math.Abs(relative) - MathF.PI / 2f);
            }
            weldDrift = sum / weldPairs.Count;
        }

        FfiStats? ffi = null;
        if (backend == "rapier")
        {
            ffi = new FfiStats(
                RapierPhysicsMetrics.ApplyMicros,
                RapierPhysicsMetrics.StepMicros,
                RapierPhysicsMetrics.StatesMicros,
                RapierPhysicsMetrics.EventsMicros,
                RapierPhysicsMetrics.OrchestrationMicros,
                RapierPhysicsMetrics.ApplyCalls,
                RapierPhysicsMetrics.StepCalls,
                RapierPhysicsMetrics.CommandsApplied,
                RapierPhysicsMetrics.StatesCopied,
                RapierPhysicsMetrics.EventsCopied,
                RapierPhysicsMetrics.PairFilterCalls,
                RapierPhysicsMetrics.JointsSeen,
                RapierPhysicsMetrics.JointCommands,
                RapierPhysicsMetrics.JointBreaks,
                RapierPhysicsMetrics.DirtySyncs,
                RapierPhysicsMetrics.AuditTicks,
                RapierPhysicsMetrics.AuditMisses,
                RapierPhysicsMetrics.FullSyncTicks,
                (long[])RapierPhysicsMetrics.OpCounts.Clone());
        }

        return new Outcome(
            Name(scene),
            backend,
            ticks,
            ids.Count,
            dyn.Count,
            asleep,
            ContactCounterSystem.Starts / 2,
            hash,
            minY,
            cargoDx,
            tickUs,
            allocBytes,
            gen0,
            gen1,
            gen2,
            jointCount,
            weldDrift,
            PreventCollideCancellerSystem.Calls,
            ffi);
    }

    public static List<string> Sanity(Outcome o)
    {
        var problems = new List<string>();
        if (o.Scene == "prevent")
        {
            if (o.Contacts != 0) problems.Add($"prevent filter leaked {o.Contacts} contacts");
            if (o.PreventCalls == 0) problems.Add("PreventCollideEvent never raised");
            if (o.MinY > -5f) problems.Add($"bodies did not fall through, min_y = {o.MinY}");
            return problems;
        }

        if (o.Scene == "chain")
        {
            if (o.Joints != ChainLinks + RopeBeads) problems.Add($"joint count {o.Joints} != {ChainLinks + RopeBeads}");
            if (o.MinY < 0f) problems.Add($"chain fell apart, min_y = {o.MinY}");
            return problems;
        }

        if (o.Scene == "welds")
        {
            if (o.Joints != WeldPairs) problems.Add($"joint count {o.Joints} != {WeldPairs}");
            if (o.WeldDrift > 0.2f) problems.Add($"welds drifted {o.WeldDrift} rad");
            if (o.MinY < -0.5f) problems.Add($"welded pairs fell through, min_y = {o.MinY}");
            return problems;
        }

        if (o.Contacts == 0 && o.Dynamics > 0) problems.Add("no collision events observed");
        if (o.MinY < -0.5f) problems.Add($"body fell through the floor, min_y = {o.MinY}");
        if (o.Scene == "mover" && o.CargoDx < 1.0f) problems.Add($"cargo not carried, mean dx = {o.CargoDx}");
        return problems;
    }

    private static EntityUid AddPlatform(IEntityManager ent, SharedPhysicsSystem physics, FixtureSystem fixtures, MapId mapId, int w, int h, float top, BodyType kind)
    {
        var uid = ent.Spawn(null, new MapCoordinates(new Vector2(0f, top - h * 0.5f), mapId));
        var body = ent.AddComponent<PhysicsComponent>(uid);
        if (kind != BodyType.Static) physics.SetBodyType(uid, kind, body: body);
        for (var i = 0; i < w; i++)
        {
            for (var j = 0; j < h; j++)
            {
                var shape = new PolygonShape();
                shape.SetAsBox(0.5f, 0.5f, new Vector2(i + 0.5f - w * 0.5f, j + 0.5f - h * 0.5f), 0f);
                fixtures.TryCreateFixture(uid, shape, $"p{i}x{j}", density: 1f, hard: true, collisionLayer: 2, collisionMask: 2, friction: 0.9f, restitution: 0f);
            }
        }
        physics.WakeBody(uid, body: body);
        return uid;
    }

    private static EntityUid AddWall(IEntityManager ent, SharedPhysicsSystem physics, FixtureSystem fixtures, MapId mapId, float x)
    {
        var uid = ent.Spawn(null, new MapCoordinates(new Vector2(x, 28f), mapId));
        var body = ent.AddComponent<PhysicsComponent>(uid);
        for (var j = 0; j < 60; j++)
        {
            var shape = new PolygonShape();
            shape.SetAsBox(0.75f, 0.5f, new Vector2(0f, j + 0.5f - 30f), 0f);
            fixtures.TryCreateFixture(uid, shape, $"w{j}", density: 1f, hard: true, collisionLayer: 2, collisionMask: 2, friction: 0.9f, restitution: 0f);
        }
        physics.WakeBody(uid, body: body);
        return uid;
    }

    private static EntityUid AddBox(IEntityManager ent, SharedPhysicsSystem physics, FixtureSystem fixtures, MapId mapId, Vector2 pos, float half, float friction)
    {
        var uid = ent.Spawn(null, new MapCoordinates(pos, mapId));
        var body = ent.AddComponent<PhysicsComponent>(uid);
        physics.SetBodyType(uid, BodyType.Dynamic, body: body);
        physics.SetFixedRotation(uid, false, body: body);
        var shape = new PolygonShape();
        shape.SetAsBox(half, half);
        fixtures.TryCreateFixture(uid, shape, "f0", density: 1f, hard: true, collisionLayer: 2, collisionMask: 2, friction: friction, restitution: 0f);
        physics.WakeBody(uid, body: body);
        return uid;
    }

    private static EntityUid AddDisc(IEntityManager ent, SharedPhysicsSystem physics, FixtureSystem fixtures, MapId mapId, Vector2 pos, float radius, float friction)
    {
        var uid = ent.Spawn(null, new MapCoordinates(pos, mapId));
        var body = ent.AddComponent<PhysicsComponent>(uid);
        physics.SetBodyType(uid, BodyType.Dynamic, body: body);
        physics.SetFixedRotation(uid, false, body: body);
        var shape = new PhysShapeCircle(radius);
        fixtures.TryCreateFixture(uid, shape, "f0", density: 1f, hard: true, collisionLayer: 2, collisionMask: 2, friction: friction, restitution: 0f);
        physics.WakeBody(uid, body: body);
        return uid;
    }
}

public struct Rng
{
    private ulong _state;

    public Rng(ulong seed)
    {
        _state = seed | 1ul;
    }

    public float NextF32(float lo, float hi)
    {
        _state ^= _state << 13;
        _state ^= _state >> 7;
        _state ^= _state << 17;
        var unit = (float)(_state >> 40) / 16777216f;
        return lo + unit * (hi - lo);
    }
}

internal static class Fnv
{
    public const ulong Basis = 14695981039346656037ul;

    public const ulong Prime = 1099511628211ul;

    public static ulong Fold(ulong hash, uint value)
    {
        hash ^= value;
        hash *= Prime;
        return hash;
    }
}

public sealed class ContactCounterSystem : EntitySystem
{
    public static long Starts;

    public override void Initialize()
    {
        base.Initialize();
        SubscribeLocalEvent<PhysicsComponent, StartCollideEvent>(OnStartCollide);
    }

    private static void OnStartCollide(Entity<PhysicsComponent> ent, ref StartCollideEvent args)
    {
        Starts++;
    }
}

public sealed class PreventCollideCancellerSystem : EntitySystem
{
    public static long Calls;
    public static bool Enabled;

    public override void Initialize()
    {
        base.Initialize();
        SubscribeLocalEvent<PhysicsComponent, PreventCollideEvent>(OnPreventCollide);
    }

    private static void OnPreventCollide(Entity<PhysicsComponent> ent, ref PreventCollideEvent args)
    {
        if (!Enabled)
            return;

        Calls++;
        if (args.OtherBody.BodyType == BodyType.Dynamic)
            args.Cancelled = true;
    }
}
