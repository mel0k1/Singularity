using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Net;
using System.Text;
using System.Threading;
using Lidgren.Network;
using NUnit.Framework;
using Robust.Shared.Network;
using Robust.Shared.Network.Transport;
using Robust.Shared.Serialization;

namespace Robust.UnitTesting.Shared.Networking;

internal sealed class SgTransportWireCompatTest : RobustIntegrationTest
{
    private static readonly byte[] Key = FillKey();

    [Test]
    public async Task RustClientHandshakesExchangesAndDisconnectsWithNetServer()
    {
        var server = StartServer();
        await server.WaitIdleAsync();
        var serializer = server.ResolveDependency<IRobustSerializer>();

        var netServer = new NetServer(BuildConfig());
        netServer.Start();
        try
        {
            var state = new ServerState();
            var host = new RawHost();
            using var clientTransport = SgTransport.Bind(host, serializer, new IPEndPoint(IPAddress.Loopback, 0));
            clientTransport.SetTimings(TimeSpan.FromMilliseconds(100), TimeSpan.FromSeconds(20), TimeSpan.Zero);

            var channel = clientTransport.Connect(new IPEndPoint(IPAddress.Loopback, netServer.Port));
            var connectionId = (ulong) channel.ConnectionId;
            Assert.That(clientTransport.SetEncryptionKey(connectionId, Key), Is.True);

            Assert.That(PumpUntil(() => channel.IsConnected && state.Connected, clientTransport, netServer, state), Is.True);
            Assert.That(state.Approvals, Is.EqualTo(1));
            Assert.That(state.Connection, Is.Not.Null);

            state.Encryption = new NetEncryption(Key, isServer: true);

            var hello = Encoding.UTF8.GetBytes("hello from singularity");
            Assert.That(clientTransport.Send(connectionId, (uint) NetDeliveryMethod.ReliableOrdered, hello), Is.True);

            var bigToServer = new byte[8192];
            for (var i = 0; i < bigToServer.Length; i++)
                bigToServer[i] = (byte) (i % 253);
            Assert.That(clientTransport.Send(connectionId, (uint) NetDeliveryMethod.ReliableOrdered, bigToServer), Is.True);

            Assert.That(PumpUntil(() => state.Plaintexts.Count >= 2, clientTransport, netServer, state), Is.True);
            Assert.That(state.DecryptFailures, Is.EqualTo(0));
            Assert.That(state.Plaintexts[0], Is.EqualTo(hello));
            Assert.That(state.Plaintexts[1], Is.EqualTo(bigToServer));

            var small = netServer.CreateMessage();
            small.Write(Encoding.UTF8.GetBytes("pong from lidgren"));
            state.Encryption.Encrypt(small);
            netServer.SendMessage(small, state.Connection!, NetDeliveryMethod.Unreliable);

            var bigToClient = new byte[8192];
            for (var i = 0; i < bigToClient.Length; i++)
                bigToClient[i] = (byte) ((i * 7 + 3) % 251);
            var big = netServer.CreateMessage(bigToClient.Length);
            big.Write(bigToClient);
            state.Encryption.Encrypt(big);
            netServer.SendMessage(big, state.Connection!, NetDeliveryMethod.ReliableOrdered);

            Assert.That(PumpUntil(() => host.Payloads.Count >= 2, clientTransport, netServer, state), Is.True);
            Assert.That(host.Payloads, Does.Contain(Encoding.UTF8.GetBytes("pong from lidgren")));
            Assert.That(host.Payloads, Does.Contain(bigToClient));

            Assert.That(PumpUntil(() =>
                state.Connection!.AverageRoundtripTime > 0 && clientTransport.Rtt(connectionId) >= 0,
                clientTransport, netServer, state), Is.True);
            Assert.That(state.Connection!.AverageRoundtripTime, Is.GreaterThan(0));
            Assert.That(float.IsFinite(clientTransport.RemoteTimeOffsetSeconds(connectionId)), Is.True);

            channel.Disconnect("wire-compat bye");
            Assert.That(PumpUntil(() => state.Disconnected, clientTransport, netServer, state), Is.True);
            Assert.That(state.DisconnectReason, Is.EqualTo("wire-compat bye"));
            Assert.That(channel.IsConnected, Is.False);
        }
        finally
        {
            netServer.Shutdown("test over");
        }
    }

    [Test]
    public async Task NetServerDisconnectReasonReachesRustSide()
    {
        var server = StartServer();
        await server.WaitIdleAsync();
        var serializer = server.ResolveDependency<IRobustSerializer>();

        var netServer = new NetServer(BuildConfig());
        netServer.Start();
        try
        {
            var state = new ServerState();
            var host = new RawHost();
            using var clientTransport = SgTransport.Bind(host, serializer, new IPEndPoint(IPAddress.Loopback, 0));
            clientTransport.SetTimings(TimeSpan.FromMilliseconds(100), TimeSpan.FromSeconds(20), TimeSpan.Zero);

            var channel = clientTransport.Connect(new IPEndPoint(IPAddress.Loopback, netServer.Port));
            Assert.That(PumpUntil(() => channel.IsConnected && state.Connected, clientTransport, netServer, state), Is.True);

            state.Connection!.Disconnect("server says bye");
            Assert.That(PumpUntil(() => host.DisconnectedReason != null, clientTransport, netServer, state), Is.True);
            Assert.That(host.DisconnectedReason, Is.EqualTo("server says bye"));
            Assert.That(channel.IsConnected, Is.False);
            Assert.That(clientTransport.Channels.Count, Is.EqualTo(0));
        }
        finally
        {
            netServer.Shutdown("test over");
        }
    }

    private static NetPeerConfiguration BuildConfig()
    {
        var config = new NetPeerConfiguration("RobustToolbox")
        {
            AcceptIncomingConnections = true,
            MaximumConnections = 8,
            PingInterval = 0.25f,
            ConnectionTimeout = 20f,
            MaximumTransmissionUnit = 700,
        };
        config.EnableMessageType(NetIncomingMessageType.ConnectionApproval);
        return config;
    }

    private static bool PumpUntil(Func<bool> done, SgTransport client, NetServer server, ServerState state)
    {
        var watch = Stopwatch.StartNew();
        while (watch.Elapsed < TimeSpan.FromSeconds(10))
        {
            client.Pump();
            DrainServer(server, state);
            if (done())
                return true;

            Thread.Sleep(2);
        }

        client.Pump();
        DrainServer(server, state);
        return done();
    }

    private static void DrainServer(NetServer server, ServerState state)
    {
        NetIncomingMessage? message;
        while ((message = server.ReadMessage()) != null)
        {
            switch (message.MessageType)
            {
                case NetIncomingMessageType.ConnectionApproval:
                    state.Approvals++;
                    message.SenderConnection?.Approve();
                    break;
                case NetIncomingMessageType.StatusChanged:
                    var status = (NetConnectionStatus) message.ReadByte();
                    var reason = message.ReadString();
                    if (status == NetConnectionStatus.Connected)
                    {
                        state.Connected = true;
                        state.Connection = message.SenderConnection;
                    }

                    if (status == NetConnectionStatus.Disconnected)
                    {
                        state.Disconnected = true;
                        state.DisconnectReason = reason;
                    }

                    break;
                case NetIncomingMessageType.Data:
                    if (state.Encryption == null)
                        break;

                    if (!state.Encryption.TryDecrypt(message))
                    {
                        state.DecryptFailures++;
                        break;
                    }

                    state.Incoming.Add(CopyPayload(message));
                    break;
            }

            server.Recycle(message);
        }
    }

    private static byte[] CopyPayload(NetIncomingMessage message)
    {
        var payload = new byte[message.LengthBytes];
        Array.Copy(message.Data, payload, message.LengthBytes);
        return payload;
    }

    private static byte[] FillKey()
    {
        var key = new byte[32];
        for (var i = 0; i < key.Length; i++)
            key[i] = (byte) (i * 11 + 5);
        return key;
    }

    private sealed class ServerState
    {
        public int Approvals;
        public bool Connected;
        public bool Disconnected;
        public string DisconnectReason = string.Empty;
        public NetConnection? Connection;
        public NetEncryption? Encryption;
        public int DecryptFailures;
        public readonly List<byte[]> Incoming = new();

        public List<byte[]> Plaintexts => new(Incoming);
    }

    private sealed class RawHost : ISgTransportHost
    {
        public readonly List<byte[]> Payloads = new();
        public string? DisconnectedReason;
        public SgTransportChannel? Channel;

        public INetManager? NetPeer => null;

        public int GetMessageId(NetMessage message)
        {
            throw new NotSupportedException("the wire-compat host exchanges raw payloads");
        }

        public void Connected(SgTransportChannel channel)
        {
            Channel = channel;
        }

        public void Disconnected(SgTransportChannel channel, string reason)
        {
            DisconnectedReason = reason;
        }

        public void Received(SgTransportChannel channel, NetIncomingMessage message)
        {
            lock (Payloads)
            {
                Payloads.Add(CopyPayload(message));
            }
        }
    }
}
