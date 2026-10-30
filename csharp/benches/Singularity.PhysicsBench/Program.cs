using System.Reflection;
using System.Text.Json;

namespace Singularity.PhysicsBench;

internal static class Program
{
    private const int DefaultTicks = 600;

    private static int Main(string[] args)
    {
        var scenes = new List<SceneKind> { SceneKind.Idle, SceneKind.Floor, SceneKind.Rain, SceneKind.Mover };
        var ticks = DefaultTicks;
        var json = false;
        var validate = false;
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
            else if (arg == "--help")
            {
                Console.WriteLine("usage: Singularity.PhysicsBench [--scene=idle|floor|rain|mover|all] [--ticks=N] [--json] [--validate]");
                return 0;
            }
            else if (arg.StartsWith("--ticks=") && int.TryParse(arg[8..], out var parsed) && parsed > 0)
            {
                ticks = parsed;
            }
            else if (arg.StartsWith("--scene="))
            {
                var rest = arg[8..];
                if (rest == "all")
                {
                    scenes = new List<SceneKind> { SceneKind.Idle, SceneKind.Floor, SceneKind.Rain, SceneKind.Mover };
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
        Console.WriteLine("singularity physics bench: farseer in-situ (robust toolbox fdb576c)");
        Console.WriteLine($".net {Environment.Version} | procs {Environment.ProcessorCount} | {config} | ticks {ticks} | dt {Scenes.Dt:0.#####}");

        foreach (var scene in scenes)
        {
            if (validate)
            {
                var first = Scenes.Run(scene, ticks);
                var second = Scenes.Run(scene, ticks);
                var determinism = first.StateHash == second.StateHash ? "identical" : "diverged";
                var problems = Scenes.Sanity(first);
                var verdict = problems.Count == 0 ? "ok" : string.Join("; ", problems);
                Console.WriteLine($"{Scenes.Name(scene),-7} {determinism} {verdict}");
                if (json)
                {
                    Console.WriteLine(JsonSerializer.Serialize(new { scene = Scenes.Name(scene), backend = "farseer", determinism, problems }));
                }
            }
            else
            {
                var outcome = Scenes.Run(scene, ticks);
                Report(outcome);
                if (json)
                {
                    Console.WriteLine(JsonSerializer.Serialize(ToJson(outcome)));
                }
            }
        }
        return 0;
    }

    private static void Report(Outcome o)
    {
        var us = o.TickUs;
        Console.WriteLine(
            $"{o.Scene,-7} bodies {o.Bodies,4} asleep {o.Asleep,4} contacts {o.Contacts,6} " +
            $"p50 {Stats.P50(us),8:F0}us p95 {Stats.P95(us),8:F0}us max {Stats.Max(us),8:F0}us mean {Stats.Mean(us),8:F0}us " +
            $"min_y {o.MinY,8:F3} cargo_dx {o.CargoDx,7:F3} hash {o.StateHash:x16}");
    }

    private static object ToJson(Outcome o)
    {
        var us = o.TickUs;
        return new
        {
            scene = o.Scene,
            backend = "farseer",
            ticks = o.Ticks,
            bodies = o.Bodies,
            dynamics = o.Dynamics,
            asleep = o.Asleep,
            contact_starts = o.Contacts,
            p50_us = Math.Round(Stats.P50(us), 3),
            p95_us = Math.Round(Stats.P95(us), 3),
            max_us = Math.Round(Stats.Max(us), 3),
            mean_us = Math.Round(Stats.Mean(us), 3),
            min_y = o.MinY,
            cargo_dx = o.CargoDx,
            state_hash = o.StateHash.ToString("x16"),
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
