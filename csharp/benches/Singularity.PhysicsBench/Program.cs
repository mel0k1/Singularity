using System.Reflection;
using System.Text.Json;

namespace Singularity.PhysicsBench;

internal static class Program
{
    private const int DefaultTicks = 600;

    private static readonly SceneKind[] AllScenes =
    {
        SceneKind.Idle, SceneKind.Floor, SceneKind.Rain, SceneKind.Mover,
        SceneKind.Chain, SceneKind.Welds, SceneKind.Prevent,
    };

    private static int Main(string[] args)
    {
        var scenes = new List<SceneKind>(AllScenes);
        var ticks = DefaultTicks;
        var json = false;
        var validate = false;
        var strict = false;
        var backend = "farseer";
        var verifySync = false;
        foreach (var arg in args)
        {
            if (arg == "--json")
            {
                json = true;
            }
            else if (arg == "--validate")
            {
                validate = true;
            }
            else if (arg == "--strict")
            {
                strict = true;
            }
            else if (arg == "--verify-sync")
            {
                verifySync = true;
            }
            else if (arg == "--help")
            {
                Console.WriteLine("usage: Singularity.PhysicsBench [--scene=idle|floor|rain|mover|chain|welds|prevent|all] [--ticks=N] [--backend=farseer|rapier] [--sleep=mapped|native] [--json] [--validate] [--strict] [--verify-sync]");
                return 0;
            }
            else if (arg.StartsWith("--ticks=") && int.TryParse(arg[8..], out var parsed) && parsed > 0)
            {
                ticks = parsed;
            }
            else if (arg.StartsWith("--backend="))
            {
                backend = arg[10..];
                if (backend != "farseer" && backend != "rapier")
                {
                    Console.Error.WriteLine($"unknown backend: {backend}");
                    return 2;
                }
            }
            else if (arg.StartsWith("--sleep="))
            {
                var mode = arg[8..];
                if (mode != "mapped" && mode != "native")
                {
                    Console.Error.WriteLine($"unknown sleep mode: {mode}");
                    return 2;
                }

                Environment.SetEnvironmentVariable("SINGULARITY_RAPIER_SLEEP", mode);
            }
            else if (arg.StartsWith("--scene="))
            {
                var rest = arg[8..];
                if (rest == "all")
                {
                    scenes = new List<SceneKind>(AllScenes);
                }
                else if (Scenes.TryParse(rest, out var scene))
                {
                    scenes = new List<SceneKind> { scene };
                }
                else
                {
                    Console.Error.WriteLine($"unknown scene: {rest}");
                    return 2;
                }
            }
            else
            {
                Console.Error.WriteLine($"unknown flag: {arg}");
                return 2;
            }
        }

        var config = typeof(Program).Assembly.GetCustomAttribute<AssemblyConfigurationAttribute>()?.Configuration ?? "?";
        Console.WriteLine($"singularity physics bench: {backend} in-situ (robust toolbox fdb576c)");
        Console.WriteLine($".net {Environment.Version} | procs {Environment.ProcessorCount} | {config} | ticks {ticks} | dt {Scenes.Dt:0.#####}");

        var failed = false;
        if (verifySync)
        {
            foreach (var scene in scenes)
            {
                Environment.SetEnvironmentVariable("SINGULARITY_RAPIER_SYNC", "full");
                var full = Scenes.Run(scene, ticks, "rapier");
                Environment.SetEnvironmentVariable("SINGULARITY_RAPIER_SYNC", null);
                var dirty = Scenes.Run(scene, ticks, "rapier");
                var verdict = full.StateHash == dirty.StateHash ? "identical" : "diverged";
                Console.WriteLine($"{Scenes.Name(scene),-7} sync full {full.StateHash:x16} dirty {dirty.StateHash:x16} {verdict}");
                if (json)
                {
                    Console.WriteLine(JsonSerializer.Serialize(new
                    {
                        scene = Scenes.Name(scene),
                        full_hash = full.StateHash.ToString("x16"),
                        dirty_hash = dirty.StateHash.ToString("x16"),
                        verdict,
                    }));
                }

                if (verdict != "identical")
                    failed = true;
            }

            return failed ? 2 : 0;
        }

        foreach (var scene in scenes)
        {
            if (validate)
            {
                var first = Scenes.Run(scene, ticks, backend);
                var second = Scenes.Run(scene, ticks, backend);
                var determinism = first.StateHash == second.StateHash ? "identical" : "diverged";
                var problems = Scenes.Sanity(first);
                var verdict = problems.Count == 0 ? "ok" : string.Join("; ", problems);
                Console.WriteLine($"{Scenes.Name(scene),-7} {determinism} {verdict}");
                if (json)
                {
                    Console.WriteLine(JsonSerializer.Serialize(ToJson(first)) + " | " + JsonSerializer.Serialize(new { determinism, problems }));
                }

                if (determinism == "diverged" || (strict && problems.Count > 0))
                    failed = true;
            }
            else
            {
                var outcome = Scenes.Run(scene, ticks, backend);
                Report(outcome);
                if (json)
                {
                    Console.WriteLine(JsonSerializer.Serialize(ToJson(outcome)));
                }
            }
        }

        return failed ? 2 : 0;
    }

    private static void Report(Outcome o)
    {
        var us = o.TickUs;
        Console.WriteLine(
            $"{o.Scene,-7} bodies {o.Bodies,4} asleep {o.Asleep,4} contacts {o.Contacts,6} joints {o.Joints,3} " +
            $"p50 {Stats.P50(us),8:F0}us p95 {Stats.P95(us),8:F0}us max {Stats.Max(us),8:F0}us mean {Stats.Mean(us),8:F0}us " +
            $"alloc {o.AllocBytes / 1024.0 / 1024.0,7:F2}MB gc {o.Gen0}/{o.Gen1}/{o.Gen2} " +
            $"min_y {o.MinY,8:F3} cargo_dx {o.CargoDx,7:F3} welds {o.WeldDrift,6:F3} prevent {o.PreventCalls,5} hash {o.StateHash:x16}");

        if (o.Ffi is { } ffi && o.Ticks > 0 && ffi.StepCalls > 0)
        {
            var perStep = ffi.StepCalls / (double)o.Ticks;
            Console.WriteLine(
                $"        ffi per tick: apply {ffi.ApplyMicros / o.Ticks,7:F1}us step {ffi.StepMicros / o.Ticks,7:F1}us " +
                $"states {ffi.StatesMicros / o.Ticks,6:F1}us events {ffi.EventsMicros / o.Ticks,6:F1}us orch {ffi.OrchestrationMicros / o.Ticks,7:F1}us " +
                $"roundtrips {perStep,3:F0} cmds {ffi.Commands / (double)o.Ticks,7:F1} " +
                $"states/tick {ffi.StatesCopied / (double)o.Ticks,6:F0} events/tick {ffi.EventsCopied / (double)o.Ticks,6:F1} " +
                $"filter {ffi.PairFilterCalls,6} joints {ffi.JointsSeen / (double)o.Ticks,5:F1}/t {ffi.JointCommands,4} breaks {ffi.JointBreaks,3} " +
                $"dirty {ffi.DirtySyncs / (double)o.Ticks,6:F1}/t audits {ffi.AuditTicks} misses {ffi.AuditMisses} full {ffi.FullSyncTicks} " +
                $"ops add+{ffi.OpCounts[1]} rm{ffi.OpCounts[2]} col+{ffi.OpCounts[3] + ffi.OpCounts[4] + ffi.OpCounts[5]} col-{ffi.OpCounts[6]} xf{ffi.OpCounts[7]} vel{ffi.OpCounts[8]} kind{ffi.OpCounts[9]} damp{ffi.OpCounts[10]} rot{ffi.OpCounts[11]} wake{ffi.OpCounts[12]} grav{ffi.OpCounts[13]} force{ffi.OpCounts[14]} jnt+{ffi.OpCounts[15]} jnt-{ffi.OpCounts[16]} sleep{ffi.OpCounts[17]} jp{ffi.OpCounts[18]}");
        }
    }

    private static object ToJson(Outcome o)
    {
        var us = o.TickUs;
        return new
        {
            scene = o.Scene,
            backend = o.Backend,
            ticks = o.Ticks,
            bodies = o.Bodies,
            dynamics = o.Dynamics,
            asleep = o.Asleep,
            contact_starts = o.Contacts,
            joints = o.Joints,
            weld_drift = o.WeldDrift,
            prevent_calls = o.PreventCalls,
            p50_us = Math.Round(Stats.P50(us), 3),
            p95_us = Math.Round(Stats.P95(us), 3),
            max_us = Math.Round(Stats.Max(us), 3),
            mean_us = Math.Round(Stats.Mean(us), 3),
            alloc_bytes = o.AllocBytes,
            gc = new[] { o.Gen0, o.Gen1, o.Gen2 },
            min_y = o.MinY,
            cargo_dx = o.CargoDx,
            state_hash = o.StateHash.ToString("x16"),
            ffi = o.Ffi is { } ffi && o.Ticks > 0
                ? new
                {
                    apply_us_per_tick = Math.Round(ffi.ApplyMicros / o.Ticks, 3),
                    step_us_per_tick = Math.Round(ffi.StepMicros / o.Ticks, 3),
                    states_us_per_tick = Math.Round(ffi.StatesMicros / o.Ticks, 3),
                    events_us_per_tick = Math.Round(ffi.EventsMicros / o.Ticks, 3),
                    orchestration_us_per_tick = Math.Round(ffi.OrchestrationMicros / o.Ticks, 3),
                    commands_per_tick = Math.Round(ffi.Commands / (double)o.Ticks, 3),
                    states_per_tick = Math.Round(ffi.StatesCopied / (double)o.Ticks, 3),
                    events_per_tick = Math.Round(ffi.EventsCopied / (double)o.Ticks, 3),
                    pair_filter_calls = ffi.PairFilterCalls,
                    joints_seen = ffi.JointsSeen,
                    joint_commands = ffi.JointCommands,
                    joint_breaks = ffi.JointBreaks,
                    dirty_syncs = ffi.DirtySyncs,
                    audit_ticks = ffi.AuditTicks,
                    audit_misses = ffi.AuditMisses,
                    full_sync_ticks = ffi.FullSyncTicks,
                }
                : null,
        };
    }
}

internal static class Stats
{
    public static double Mean(double[] v) => v.Average();

    public static double Max(double[] v) => v.Max();

    public static double P50(double[] v) => P(v, 0.50);

    public static double P95(double[] v) => P(v, 0.95);

    private static double P(double[] v, double p)
    {
        var sorted = (double[])v.Clone();
        Array.Sort(sorted);
        var k = Math.Clamp((int)Math.Ceiling(p * sorted.Length), 1, sorted.Length);
        return sorted[k - 1];
    }
}
