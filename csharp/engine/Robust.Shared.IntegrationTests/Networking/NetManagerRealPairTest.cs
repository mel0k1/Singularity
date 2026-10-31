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

internal sealed class NetManagerRealPairTest : RobustIntegrationTest
{
    [TestCase("lidgren")]
    [TestCase("singularity")]
    public async Task RealUdpPairCarriesLoginStringTablesAndGameStates(string backend)
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

        var clientOptions = new ClientIntegrationOptions
        {
            InitIoC = () =>
            {
                IoCManager.Register<INetManager, NetManager>(true);
                IoCManager.Register<IClientNetManager, NetManager>(true);
            },
        };
        clientOptions.CVarOverrides["net.backend"] = backend;

        var server = StartServer(serverOptions);
        var client = StartClient(clientOptions);

        await Task.WhenAll(client.WaitIdleAsync(), server.WaitIdleAsync());

        var sNetMan = server.ResolveDependency<IServerNetManager>();
        var cNetMan = client.ResolveDependency<IClientNetManager>();
        var sPlayerMan = server.ResolveDependency<sIPlayerManager>();

        Assert.That(sNetMan.IsRunning, Is.True, "server net manager did not start");

        await client.WaitPost(() => cNetMan.ClientConnect("127.0.0.1", port, "wiretest"));

        var connected = false;
        for (var i = 0; i < 600 && !connected; i++)
        {
            await server.WaitRunTicks(1);
            await client.WaitRunTicks(1);
            connected = cNetMan.IsConnected && sPlayerMan.Sessions.Any(s => s.Status == SessionStatus.Connected);
        }

        Assert.That(connected, Is.True, $"client never finished connecting over the {backend} transport");

        var session = sPlayerMan.Sessions.Single();
        Assert.That(session.Name, Is.EqualTo("wiretest"));
        Assert.That(cNetMan.ServerChannel, Is.Not.Null);

        var sEntities = server.ResolveDependency<IEntityManager>();
        var cEntities = client.ResolveDependency<IEntityManager>();

        MapId mapId = default;
        EntityUid mapEntity = default;
        await server.WaitPost(() =>
        {
            mapEntity = sEntities.System<SharedMapSystem>().CreateMap(out mapId);
        });

        EntityUid entity = default;
        NetEntity netEntity = default!;
        await server.WaitPost(() =>
        {
            var actor = sEntities.SpawnAttachedTo(null, new EntityCoordinates(mapEntity, default));
            sPlayerMan.SetAttachedEntity(session, actor);
            sPlayerMan.JoinGame(session);

            entity = sEntities.SpawnAttachedTo(null, new EntityCoordinates(mapEntity, default));
            netEntity = sEntities.GetNetEntity(entity);
        });

        EntityUid clientEntity = default;
        var seen = false;
        for (var i = 0; i < 600 && !seen; i++)
        {
            await server.WaitRunTicks(1);
            await client.WaitRunTicks(1);
            clientEntity = cEntities.GetEntity(netEntity);
            seen = cEntities.EntityExists(clientEntity);
        }

        Assert.That(seen, Is.True, $"game states never delivered the spawned entity over the {backend} transport");
        Assert.That(
            cEntities.GetComponent<TransformComponent>(clientEntity).Coordinates,
            Is.EqualTo(new EntityCoordinates(mapEntity, 0, 0)),
            "the entity arrived at the wrong position on the client");

        session.Channel.Disconnect("server says bye");

        var disconnected = false;
        for (var i = 0; i < 600 && !disconnected; i++)
        {
            await server.WaitRunTicks(1);
            await client.WaitRunTicks(1);
            disconnected = !cNetMan.IsConnected && !sPlayerMan.Sessions.Any();
        }

        Assert.That(disconnected, Is.True, "disconnect never propagated to both sides");
    }

    [TestCase("lidgren")]
    [TestCase("singularity")]
    public async Task MtuExpandRaisesChannelMtuOnBothEnds(string backend)
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
        serverOptions.CVarOverrides["net.mtu_expand"] = "true";
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
        clientOptions.CVarOverrides["net.mtu_expand"] = "true";
        clientOptions.CVarOverrides["net.mtu_expand_frequency"] = "1";

        var server = StartServer(serverOptions);
        var client = StartClient(clientOptions);

        await Task.WhenAll(client.WaitIdleAsync(), server.WaitIdleAsync());

        var sNetMan = server.ResolveDependency<IServerNetManager>();
        var cNetMan = client.ResolveDependency<IClientNetManager>();
        var sPlayerMan = server.ResolveDependency<sIPlayerManager>();

        await client.WaitPost(() => cNetMan.ClientConnect("127.0.0.1", port, "mtutest"));

        var connected = false;
        for (var i = 0; i < 600 && !connected; i++)
        {
            await server.WaitRunTicks(1);
            await client.WaitRunTicks(1);
            connected = cNetMan.IsConnected && sPlayerMan.Sessions.Any(s => s.Status == SessionStatus.Connected);
        }

        Assert.That(connected, Is.True, $"client never finished connecting over the {backend} transport");

        var session = sPlayerMan.Sessions.Single();
        var expanded = false;
        for (var i = 0; i < 900 && !expanded; i++)
        {
            await server.WaitRunTicks(1);
            await client.WaitRunTicks(1);
            expanded = session.Channel.CurrentMtu >= 8190 && cNetMan.ServerChannel!.CurrentMtu >= 8190;
        }

        Assert.That(
            expanded,
            Is.True,
            $"mtu never reached the protocol max over the {backend} transport " +
            $"(server {session.Channel.CurrentMtu}, client {cNetMan.ServerChannel!.CurrentMtu})");
        Assert.That(session.Channel.CurrentMtu, Is.EqualTo(8190), $"server channel mtu wrong on {backend}");
        Assert.That(cNetMan.ServerChannel!.CurrentMtu, Is.EqualTo(8190), $"client channel mtu wrong on {backend}");
    }

    private static int PickFreePort()
    {
        using var socket = new Socket(AddressFamily.InterNetwork, SocketType.Dgram, ProtocolType.Udp);
        socket.Bind(new IPEndPoint(IPAddress.Loopback, 0));
        return ((IPEndPoint) socket.LocalEndPoint!).Port;
    }
}
