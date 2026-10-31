using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Net;
using System.Threading;
using Lidgren.Network;
using NUnit.Framework;
using Robust.Shared;
using Robust.Shared.Configuration;
using Robust.Shared.Network;
using Robust.Shared.Network.Transport;
using Robust.Shared.Serialization;

namespace Robust.UnitTesting.Shared.Networking;

internal sealed class SgTransportTest : RobustIntegrationTest
{
    [Test]
    public async Task NetBackendDefaultsToLidgren()
    {
        var server = StartServer();
        await server.WaitIdleAsync();
        var cfg = server.ResolveDependency<IConfigurationManager>();
        Assert.That(cfg.GetCVar(CVars.NetBackend), Is.EqualTo("lidgren"));
    }

    [Test]
    public async Task LoopbackPairCarriesEnginePayloads()
    {
        var (serverTransport, clientTransport, serverHost, clientHost, clientChannel) = await StartPair();
        using var serverSide = serverTransport;
        using var clientSide = clientTransport;

        Assert.That(PumpUntil(() => clientHost.Channel != null && serverHost.Channel != null, serverTransport, clientTransport), Is.True);
        Assert.That(clientChannel.IsConnected, Is.True);
        Assert.That(serverHost.Channel!.IsConnected, Is.True);
        Assert.That(serverHost.Channel!.IsHandshakeComplete, Is.True);
        Assert.That(serverHost.Channel!.RemoteEndPoint.Address, Is.EqualTo(IPAddress.Loopback));
        Assert.That(serverHost.Channel!.RemoteEndPoint.Port, Is.EqualTo(clientTransport.LocalPort));

        for (var i = 1; i <= 3; i++)
        {
            clientChannel.SendMessage(new TestChatMessage { Text = $"ordered {i}" });
        }

        clientChannel.SendMessage(new TestEntityMessage { Number = 777 });
        clientChannel.SendMessage(new TestChatMessage { Text = "after unreliable" });

        Assert.That(PumpUntil(() => serverHost.Chat.Count >= 4 && serverHost.EntityNumber != null, serverTransport, clientTransport), Is.True);
        Assert.That(serverHost.Chat, Is.EqualTo(new[] { "ordered 1", "ordered 2", "ordered 3", "after unreliable" }));
        Assert.That(serverHost.EntityNumber, Is.EqualTo(777));

        serverHost.Channel!.SendMessage(new TestChatMessage { Text = "hello back" });
        Assert.That(PumpUntil(() => clientHost.Chat.Count >= 1, serverTransport, clientTransport), Is.True);
        Assert.That(clientHost.Chat[0], Is.EqualTo("hello back"));

        clientChannel.Disconnect("done");
        Assert.That(PumpUntil(() => serverHost.Disconnections == 1 && clientHost.Disconnections == 1, serverTransport, clientTransport), Is.True);
        Assert.That(serverHost.Channel!.IsConnected, Is.False);
        Assert.That(serverTransport.Channels.Count, Is.EqualTo(0));
    }

    [Test]
    public async Task SilenceTimesOutOnThePeer()
    {
        var (serverTransport, clientTransport, serverHost, clientHost, _) = await StartPair();
        using var serverSide = serverTransport;
        using var clientSide = clientTransport;

        Assert.That(PumpUntil(() => clientHost.Channel != null && serverHost.Channel != null, serverTransport, clientTransport), Is.True);

        serverTransport.SetTimings(TimeSpan.FromMilliseconds(100), TimeSpan.FromMilliseconds(300), TimeSpan.FromMilliseconds(50));

        var watch = Stopwatch.StartNew();
        while (watch.Elapsed < TimeSpan.FromSeconds(5) && serverHost.Disconnections == 0)
        {
            serverTransport.Pump();
            Thread.Sleep(10);
        }

        Assert.That(serverHost.Disconnections, Is.EqualTo(1));
        Assert.That(serverTransport.Channels.Count, Is.EqualTo(0));
    }

    private async Task<(SgTransport Server, SgTransport Client, Host ServerHost, Host ClientHost, SgTransportChannel ClientChannel)> StartPair()
    {
        var server = StartServer();
        await server.WaitIdleAsync();
        var cfg = server.ResolveDependency<IConfigurationManager>();
        var serializer = server.ResolveDependency<IRobustSerializer>();
        cfg.SetCVar(CVars.NetBackend, "singularity");

        var key = new byte[32];
        for (var i = 0; i < key.Length; i++)
        {
            key[i] = (byte) (i * 7 + 1);
        }

        var serverHost = new Host(serializer, key, isServer: true);
        var clientHost = new Host(serializer, key, isServer: false);
        var serverTransport = SgTransport.Bind(serverHost, serializer, new IPEndPoint(IPAddress.Loopback, 0));
        var clientTransport = SgTransport.Bind(clientHost, serializer, new IPEndPoint(IPAddress.Loopback, 0));
        var clientChannel = clientTransport.Connect(new IPEndPoint(IPAddress.Loopback, serverTransport.LocalPort));
        return (serverTransport, clientTransport, serverHost, clientHost, clientChannel);
    }

    private static bool PumpUntil(Func<bool> done, params SgTransport[] transports)
    {
        var watch = Stopwatch.StartNew();
        while (watch.Elapsed < TimeSpan.FromSeconds(5))
        {
            foreach (var transport in transports)
            {
                transport.Pump();
            }

            if (done())
            {
                return true;
            }

            Thread.Sleep(2);
        }

        return done();
    }

    private sealed class Host : ISgTransportHost
    {
        private static readonly Dictionary<string, int> Ids = new()
        {
            [nameof(TestChatMessage)] = 1,
            [nameof(TestEntityMessage)] = 2,
        };

        private readonly IRobustSerializer _serializer;
        private readonly byte[] _key;
        private readonly bool _isServer;

        public SgTransportChannel? Channel;
        public readonly List<string> Chat = new();
        public int? EntityNumber;
        public int Disconnections;

        public Host(IRobustSerializer serializer, byte[] key, bool isServer)
        {
            _serializer = serializer;
            _key = key;
            _isServer = isServer;
        }

        public INetManager? NetPeer => null;

        public int GetMessageId(NetMessage message)
        {
            if (!Ids.TryGetValue(message.MsgName, out var id))
            {
                throw new NetManagerException($"no id registered for {message.MsgName}");
            }

            return id;
        }

        public void Connected(SgTransportChannel channel)
        {
            Channel = channel;
            channel.EnableEncryption(_key, _isServer);
            if (_isServer)
            {
                channel.UserData = new NetUserData(new NetUserId(Guid.NewGuid()), "tester");
                channel.AuthType = LoginType.Guest;
            }

            channel.IsHandshakeComplete = true;
        }

        public void Disconnected(SgTransportChannel channel, string reason)
        {
            Disconnections += 1;
        }

        public void Received(SgTransportChannel channel, NetIncomingMessage message)
        {
            if (!channel.TryDecrypt(message))
            {
                return;
            }

            switch (message.ReadByte())
            {
                case 1:
                    var chat = new TestChatMessage { MsgChannel = channel };
                    chat.ReadFromBuffer(message, _serializer);
                    Chat.Add(chat.Text);
                    break;
                case 2:
                    var entity = new TestEntityMessage { MsgChannel = channel };
                    entity.ReadFromBuffer(message, _serializer);
                    EntityNumber = entity.Number;
                    break;
            }
        }
    }

    private sealed class TestChatMessage : NetMessage
    {
        public string Text = string.Empty;

        public override MsgGroups MsgGroup => MsgGroups.String;

        public override int EstimateBufferSize() => 16 + Text.Length;

        public override void WriteToBuffer(NetOutgoingMessage buffer, IRobustSerializer serializer)
        {
            buffer.Write(Text);
        }

        public override void ReadFromBuffer(NetIncomingMessage buffer, IRobustSerializer serializer)
        {
            Text = buffer.ReadString();
        }
    }

    private sealed class TestEntityMessage : NetMessage
    {
        public int Number;

        public override MsgGroups MsgGroup => MsgGroups.Entity;

        public override int EstimateBufferSize() => 8;

        public override void WriteToBuffer(NetOutgoingMessage buffer, IRobustSerializer serializer)
        {
            buffer.Write(Number);
        }

        public override void ReadFromBuffer(NetIncomingMessage buffer, IRobustSerializer serializer)
        {
            Number = buffer.ReadInt32();
        }
    }
}
