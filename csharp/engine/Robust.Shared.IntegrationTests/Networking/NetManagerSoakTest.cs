using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Threading.Tasks;
using NUnit.Framework;
using Robust.Shared;
using Robust.Shared.Enums;
using Robust.Shared.GameObjects;
using Robust.Shared.IoC;
using Robust.Shared.Map;
using Robust.Shared.Network;
using sIPlayerManager = Robust.Server.Player.IPlayerManager;

namespace Robust.UnitTesting.Shared.Networking;

internal sealed class NetManagerSoakTest : RobustIntegrationTest
{
    private const int EntityCount = 128;
    private const int EntityBatchCount = 4;
    private const int MovesPerTick = 16;
    private const int ProtocolMaxMtu = 8190;
    private const int LoginSettleTicks = 45;

    [TestCase("lidgren")]
    [TestCase("singularity")]
    public async Task RealContentSoakHoldsTheLine(string backend)
    {
        var seconds = LoadSoakSeconds();
        if (seconds <= 0)
        {
            Assert.Ignore("SG_SOAK_SECONDS=0 disables the soak");
            return;
        }

        var run = await RunSoak(backend, seconds, LoadSoakExpand());

        Assert.That(run.ConnectedAtEnd, Is.True, $"connection dropped mid-soak on {backend}");
        Assert.That(run.ClientSawAllEntities, Is.True, $"client never saw every spawned entity on {backend}");
        Assert.That(run.FinalCoordinatesMatch, Is.True, $"tracked entity coordinates diverged on {backend}");
        if (run.Expand)
        {
            Assert.That(run.ClientMtu, Is.EqualTo(ProtocolMaxMtu), $"client mtu never expanded on {backend}");
            Assert.That(run.ServerMtu, Is.EqualTo(ProtocolMaxMtu), $"server mtu never expanded on {backend}");
        }
        else
        {
            Assert.That(run.ClientMtu, Is.EqualTo(700), $"client mtu moved without expansion on {backend}");
            Assert.That(run.ServerMtu, Is.EqualTo(700), $"server mtu moved without expansion on {backend}");
        }
        Assert.That(run.RecvBytes, Is.GreaterThan(0), $"no traffic reached the client on {backend}");
        Assert.That(run.SentBytes, Is.GreaterThan(0), $"server sent nothing on {backend}");
        Assert.That(run.RttMillis, Is.LessThan(2000), $"rtt exploded to {run.RttMillis} ms on {backend}");

        TestContext.Out.WriteLine(
            $"soak[{backend}] seconds={run.Seconds:F1} serverTicks={run.ServerTicks} " +
            $"sentBytes={run.SentBytes} recvBytes={run.RecvBytes} " +
            $"sentPackets={run.SentPackets} recvPackets={run.RecvPackets} " +
            $"recvRate={(long) (run.RecvBytes / Math.Max(0.001, run.Seconds))} B/s " +
            $"packetRate={(long) (run.RecvPackets / Math.Max(0.001, run.Seconds))} pkt/s " +
            $"rttMs={run.RttMillis:F1} clientMtu={run.ClientMtu} serverMtu={run.ServerMtu}");
    }

    [Test]
    public async Task SingularityKeepsUpWithLidgrenOnRealContent()
    {
        var seconds = LoadSoakSeconds();
        if (seconds <= 0)
        {
            Assert.Ignore("SG_SOAK_SECONDS=0 disables the soak");
            return;
        }

        var lidgren = await RunSoak("lidgren", seconds, LoadSoakExpand());
        var singularity = await RunSoak("singularity", seconds, LoadSoakExpand());

        var lidgrenRate = lidgren.RecvBytes / Math.Max(0.001, lidgren.Seconds);
        var singularityRate = singularity.RecvBytes / Math.Max(0.001, singularity.Seconds);
        var lidgrenPackets = lidgren.RecvPackets / Math.Max(0.001, lidgren.Seconds);
        var singularityPackets = singularity.RecvPackets / Math.Max(0.001, singularity.Seconds);

        TestContext.Out.WriteLine(
            $"soak-compare lidgren: {lidgrenRate:F0} B/s {lidgrenPackets:F0} pkt/s ticks={lidgren.ServerTicks}");
        TestContext.Out.WriteLine(
            $"soak-compare singularity: {singularityRate:F0} B/s {singularityPackets:F0} pkt/s ticks={singularity.ServerTicks}");

        Assert.That(
            singularityRate,
            Is.GreaterThan(lidgrenRate * 0.75),
            $"singularity delivered {singularityRate:F0} B/s against lidgren's {lidgrenRate:F0} B/s on identical content");
    }

    private static double LoadSoakSeconds()
    {
        var raw = Environment.GetEnvironmentVariable("SG_SOAK_SECONDS");
        return double.TryParse(raw, out var seconds) ? seconds : 8;
    }

    private static bool LoadSoakExpand()
    {
        var raw = Environment.GetEnvironmentVariable("SG_SOAK_EXPAND");
        return !bool.TryParse(raw, out var expand) || expand;
    }

    private async Task<SoakRun> RunSoak(string backend, double seconds, bool expand)
    {
        var port = PickFreePort();

        var serverOptions = new ServerIntegrationOptions
        {
            InitIoC = () =>
            {
                IoCManager.Register<INetManager, NetManager>(true);
                IoCManager.Register<IServerNetManager, NetManager>(true);
            },
        };
        serverOptions.CVarOverrides["net.backend"] = backend;
        serverOptions.CVarOverrides["net.bindto"] = "127.0.0.1";
        serverOptions.CVarOverrides["net.port"] = port.ToString();
        serverOptions.CVarOverrides["auth.mode"] = "2";
        serverOptions.CVarOverrides["net.mtu_expand"] = expand.ToString().ToLowerInvariant();
        serverOptions.CVarOverrides["net.mtu_expand_frequency"] = "1";

        var clientOptions = new ClientIntegrationOptions
        {
            InitIoC = () =>
            {
                IoCManager.Register<INetManager, NetManager>(true);
                IoCManager.Register<IClientNetManager, NetManager>(true);
            },
        };
        clientOptions.CVarOverrides["net.backend"] = backend;
        clientOptions.CVarOverrides["net.mtu_expand"] = expand.ToString().ToLowerInvariant();
        clientOptions.CVarOverrides["net.mtu_expand_frequency"] = "1";

        var server = StartServer(serverOptions);
        var client = StartClient(clientOptions);

        await Task.WhenAll(client.WaitIdleAsync(), server.WaitIdleAsync());

        var sNetMan = server.ResolveDependency<IServerNetManager>();
        var cNetMan = client.ResolveDependency<IClientNetManager>();
        var sPlayerMan = server.ResolveDependency<sIPlayerManager>();

        await client.WaitPost(() => cNetMan.ClientConnect("127.0.0.1", port, "soak"));

        var connected = false;
        for (var i = 0; i < 600 && !connected; i++)
        {
            await server.WaitRunTicks(1);
            await client.WaitRunTicks(1);
            connected = cNetMan.IsConnected && sPlayerMan.Sessions.Any(s => s.Status == SessionStatus.Connected);
        }

        Assert.That(connected, Is.True, $"client never finished connecting over the {backend} transport");

        var session = sPlayerMan.Sessions.Single();
        var sEntities = server.ResolveDependency<IEntityManager>();
        var cEntities = client.ResolveDependency<IEntityManager>();

        MapId mapId = default;
        EntityUid mapEntity = default;
        await server.WaitPost(() =>
        {
            mapEntity = sEntities.System<SharedMapSystem>().CreateMap(out mapId);
        });

        await server.WaitPost(() =>
        {
            var actor = sEntities.SpawnAttachedTo(null, new EntityCoordinates(mapEntity, default));
            sPlayerMan.SetAttachedEntity(session, actor);
            sPlayerMan.JoinGame(session);
        });

        for (var i = 0; i < LoginSettleTicks; i++)
        {
            await server.WaitRunTicks(1);
            await client.WaitRunTicks(1);
        }

        var entities = new List<EntityUid>();
        var netEntities = new List<NetEntity>();
        for (var batch = 0; batch < EntityBatchCount; batch++)
        {
            await server.WaitPost(() =>
            {
                for (var i = 0; i < EntityCount / EntityBatchCount; i++)
                {
                    var index = entities.Count;
                    var x = (index % 12 - 5.5f) * 0.7f;
                    var y = (index / 12 - 5.5f) * 0.7f;
                    var entity = sEntities.SpawnAttachedTo(null, new EntityCoordinates(mapEntity, x, y));
                    entities.Add(entity);
                    netEntities.Add(sEntities.GetNetEntity(entity));
                }
            });
            await server.WaitRunTicks(1);
            await client.WaitRunTicks(1);
        }

        var watch = Stopwatch.StartNew();
        var tick = 0;
        var stayedConnected = true;
        while (watch.Elapsed < TimeSpan.FromSeconds(seconds))
        {
            await server.WaitRunTicks(1);
            var moves = new (EntityUid entity, float x, float y)[MovesPerTick];
            for (var m = 0; m < MovesPerTick; m++)
            {
                var index = (tick * MovesPerTick + m) % entities.Count;
                var x = ((tick + index * 3 + m * 7) % 17 - 8) * 0.2f;
                var y = ((tick * 2 + index * 5 + m) % 17 - 8) * 0.2f;
                moves[m] = (entities[index], x, y);
            }

            await server.WaitPost(() =>
            {
                var xform = sEntities.System<SharedTransformSystem>();
                foreach (var (entity, x, y) in moves)
                {
                    xform.SetCoordinates(entity, new EntityCoordinates(mapEntity, x, y));
                }
            });
            tick++;

            if (!cNetMan.IsConnected || session.Status != SessionStatus.InGame)
            {
                stayedConnected = false;
                break;
            }

            await client.WaitRunTicks(1);
        }

        var allPresent = false;
        for (var i = 0; i < 600 && !allPresent; i++)
        {
            await server.WaitRunTicks(1);
            await client.WaitRunTicks(1);
            allPresent = netEntities.All(n => cEntities.EntityExists(cEntities.GetEntity(n)));
        }

        var coordsMatch = false;
        for (var i = 0; i < 600 && !coordsMatch; i++)
        {
            await server.WaitRunTicks(1);
            await client.WaitRunTicks(1);
            EntityCoordinates serverCoords = default;
            await server.WaitPost(() =>
            {
                serverCoords = sEntities.GetComponent<TransformComponent>(entities[0]).Coordinates;
            });
            var clientCoords = cEntities.GetComponent<TransformComponent>(cEntities.GetEntity(netEntities[0])).Coordinates;
            coordsMatch = Math.Abs(serverCoords.X - clientCoords.X) < 0.01f
                           && Math.Abs(serverCoords.Y - clientCoords.Y) < 0.01f;
        }

        var serverStats = ((NetManager) sNetMan).Statistics;
        var clientStats = ((NetManager) cNetMan).Statistics;
        var run = new SoakRun
        {
            Expand = expand,
            Seconds = watch.Elapsed.TotalSeconds,
            ServerTicks = tick,
            ConnectedAtEnd = stayedConnected && cNetMan.IsConnected,
            ClientSawAllEntities = allPresent,
            FinalCoordinatesMatch = coordsMatch,
            SentBytes = serverStats.SentBytes,
            RecvBytes = clientStats.ReceivedBytes,
            SentPackets = serverStats.SentPackets,
            RecvPackets = clientStats.ReceivedPackets,
            ClientMtu = cNetMan.ServerChannel!.CurrentMtu,
            ServerMtu = session.Channel.CurrentMtu,
            RttMillis = cNetMan.ServerChannel.Ping,
        };

        session.Channel.Disconnect("soak over");
        var disconnected = false;
        for (var i = 0; i < 600 && !disconnected; i++)
        {
            await server.WaitRunTicks(1);
            await client.WaitRunTicks(1);
            disconnected = !cNetMan.IsConnected && !sPlayerMan.Sessions.Any();
        }

        Assert.That(disconnected, Is.True, $"disconnect never propagated over the {backend} transport");
        return run;
    }

    private static int PickFreePort()
    {
        using var socket = new Socket(AddressFamily.InterNetwork, SocketType.Dgram, ProtocolType.Udp);
        socket.Bind(new IPEndPoint(IPAddress.Loopback, 0));
        return ((IPEndPoint) socket.LocalEndPoint!).Port;
    }

    private sealed class SoakRun
    {
        public bool Expand;
        public double Seconds;
        public int ServerTicks;
        public bool ConnectedAtEnd;
        public bool ClientSawAllEntities;
        public bool FinalCoordinatesMatch;
        public long SentBytes;
        public long RecvBytes;
        public long SentPackets;
        public long RecvPackets;
        public int ClientMtu;
        public int ServerMtu;
        public double RttMillis;
    }
}
