using System.Diagnostics;
using System.Numerics;
using Robust.Shared.GameObjects;
using Robust.Shared.Map;
using Robust.Shared.Physics;
using Robust.Shared.Physics.Backend;
using Robust.Shared.Physics.Collision.Shapes;
using Robust.Shared.Physics.Components;
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
}

public sealed record FfiStats(
    double ApplyMicros,
    double StepMicros,
    double StatesMicros,
    double EventsMicros,
    long ApplyCalls,
    long StepCalls,
    long Commands,
    long StatesCopied,
    long EventsCopied,
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
    FfiStats? Ffi);

public static class Scenes
{
    public const float Dt = 1f / 60f;
    public const int RainSpawns = 300;
    public const float MoverSpeed = 1.5f;

    public static string Name(SceneKind scene)
    {
        return scene switch
        {
            SceneKind.Idle => "idle",
            SceneKind.Floor => "floor",
            SceneKind.Rain => "rain",
            SceneKind.Mover => "mover",
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
            default: scene = SceneKind.Idle; return false;
        }
    }

    private static int Seed(SceneKind scene)
    {
        return scene switch
        {
            SceneKind.Rain => 2022,
            SceneKind.Mover => 3033,
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
            .RegisterEntitySystems(factory => factory.LoadExtraSystemType<ContactCounterSystem>())
            .InitializeInstance();
        var ent = sim.Resolve<IEntityManager>();
        var physics = ent.System<SharedPhysicsSystem>();
        var activeBackend = physics.PhysicsBackendName;
        if (backend != "farseer" && activeBackend != backend)
            throw new InvalidOperationException($"physics backend '{backend}' did not activate (native library missing?)");
        var fixtures = ent.System<FixtureSystem>();
        var mapSys = ent.System<SharedMapSystem>();
        mapSys.CreateMap(out var mapId);
        physics.SetGravity(new Vector2(0f, -9.81f));
        ContactCounterSystem.Starts = 0;
        RapierPhysicsMetrics.Reset();

        var rng = new Rng((ulong)Seed(scene));
        var ids = new List<EntityUid>();
        var dyn = new List<EntityUid>();
        var cargo = new List<(EntityUid Uid, float X0)>();
        EntityUid platform;

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

        FfiStats? ffi = null;
        if (backend == "rapier")
        {
            ffi = new FfiStats(
                RapierPhysicsMetrics.ApplyMicros,
                RapierPhysicsMetrics.StepMicros,
                RapierPhysicsMetrics.StatesMicros,
                RapierPhysicsMetrics.EventsMicros,
                RapierPhysicsMetrics.ApplyCalls,
                RapierPhysicsMetrics.StepCalls,
                RapierPhysicsMetrics.CommandsApplied,
                RapierPhysicsMetrics.StatesCopied,
                RapierPhysicsMetrics.EventsCopied,
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
            ffi);
    }

    public static List<string> Sanity(Outcome o)
    {
        var problems = new List<string>();
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
