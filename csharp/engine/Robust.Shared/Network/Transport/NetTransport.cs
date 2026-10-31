using System;
using System.Collections.Generic;
using System.Net;
using Lidgren.Network;

namespace Robust.Shared.Network.Transport;

internal enum NetTransportEventKind
{
    Connected,
    Disconnected,
    Data,
}

internal readonly struct NetTransportEvent
{
    public readonly NetTransportEventKind Kind;
    public readonly INetTransportConnection Connection;
    public readonly byte[] Payload;
    public readonly string Reason;

    public NetTransportEvent(
        NetTransportEventKind kind,
        INetTransportConnection connection,
        byte[]? payload = null,
        string? reason = null)
    {
        Kind = kind;
        Connection = connection;
        Payload = payload ?? Array.Empty<byte>();
        Reason = reason ?? string.Empty;
    }
}

internal struct NetTransportStatistics
{
    public long SentBytes;
    public long ReceivedBytes;
    public long SentPackets;
    public long ReceivedPackets;
    public long SentMessages;
    public long ReceivedMessages;
    public long ResentDueToDelay;
    public long ResentDueToHole;
    public long DroppedMessages;
}

internal struct NetTransportSettings
{
    public string AppIdentifier;
    public int Mtu;
    public bool MtuExpand;
    public float MtuExpandFrequency;
    public int MtuExpandFailAttempts;
    public float PingInterval;
    public float ConnectionTimeout;
    public int MaxIpConnections;
    public int MaxRapidConnections;
    public bool SendConnectionRejectionReasons;
    public double RapidConnectionWindow;
    public int RapidConnectionDecay;
    public int MaxFragmentReassemblyBytesPerConnection;
    public float FragmentGroupTimeout;
    public float SimulatedLoss;
    public float SimulatedMinimumLatency;
    public float SimulatedRandomLatency;
    public float SimulatedDuplicatesChance;
    public bool VerboseLogging;
    public bool WarningLogging;
    public bool ErrorLogging;
    public bool LogRateLimitEnabled;
    public int LogRateLimitTargets;
    public int LogRateLimitBurst;
    public float LogRateLimitWindow;
}

internal interface INetTransportConnection
{
    IPEndPoint RemoteEndPoint { get; }

    long RemoteUniqueIdentifier { get; }

    float AverageRoundtripTime { get; }

    float RemoteTimeOffset { get; }

    int CurrentMtu { get; }

    NetConnectionStatus Status { get; }

    bool CanSendImmediately(NetDeliveryMethod method, int sequenceChannel);

    bool Send(NetOutgoingMessage message, NetDeliveryMethod method, int sequenceChannel);

    void Disconnect(string reason, bool sendBye = true);

    NetOutgoingMessage CreateMessage(int initialCapacity = 0);
}

internal interface INetTransportPeer
{
    void Start();

    void Shutdown(string reason);

    bool IsShutdownPending { get; }

    int ConnectionsCount { get; }

    NetTransportStatistics Statistics { get; }

    INetTransportConnection? Connect(IPEndPoint endPoint);

    void ApplySettings(NetTransportSettings settings);
}

internal sealed class LidgrenTransportPeer : INetTransportPeer
{
    public readonly NetPeer Peer;

    public LidgrenTransportPeer(NetPeer peer)
    {
        Peer = peer;
    }

    public void Start()
    {
        Peer.Start();
    }

    public void Shutdown(string reason)
    {
        Peer.Shutdown(reason);
    }

    public bool IsShutdownPending => Peer.Status == NetPeerStatus.ShutdownRequested;

    public int ConnectionsCount => Peer.ConnectionsCount;

    public NetTransportStatistics Statistics
    {
        get
        {
            var stats = Peer.Statistics;
            return new NetTransportStatistics
            {
                SentBytes = stats.SentBytes,
                ReceivedBytes = stats.ReceivedBytes,
                SentPackets = stats.SentPackets,
                ReceivedPackets = stats.ReceivedPackets,
                SentMessages = stats.SentMessages,
                ReceivedMessages = stats.ReceivedMessages,
                ResentDueToDelay = stats.ResentMessagesDueToDelay,
                ResentDueToHole = stats.ResentMessagesDueToHole,
                DroppedMessages = stats.DroppedMessages,
            };
        }
    }

    public INetTransportConnection? Connect(IPEndPoint endPoint)
    {
        return GetConnection(Peer.Connect(endPoint));
    }

    public void ApplySettings(NetTransportSettings settings)
    {
        var config = Peer.Configuration;
        if (Peer.Status == NetPeerStatus.NotRunning)
        {
            config.MaximumTransmissionUnit = Math.Clamp(
                settings.Mtu,
                1,
                (ushort.MaxValue + 1) / 8 - 1);
            config.AutoExpandMTU = settings.MtuExpand;
        }

        config.ExpandMTUFrequency = Math.Max(0.01f, settings.MtuExpandFrequency);
        config.ExpandMTUFailAttempts = Math.Max(1, settings.MtuExpandFailAttempts);
        config.PingInterval = settings.PingInterval;
        config.ConnectionTimeout = settings.ConnectionTimeout;
        config.MaximumIpConnections = settings.MaxIpConnections;
        config.MaximumRapidConnections = settings.MaxRapidConnections;
        config.SendConnectionRejectionReasons = settings.SendConnectionRejectionReasons;
        config.RapidConnectionWindow = settings.RapidConnectionWindow;
        config.RapidConnectionDecay = settings.RapidConnectionDecay;
        config.MaximumFragmentReassemblyBytesPerConnection =
            settings.MaxFragmentReassemblyBytesPerConnection;
        config.FragmentGroupTimeout = settings.FragmentGroupTimeout;
        config.SimulatedLoss = settings.SimulatedLoss;
        config.SimulatedMinimumLatency = settings.SimulatedMinimumLatency;
        config.SimulatedRandomLatency = settings.SimulatedRandomLatency;
        config.SimulatedDuplicatesChance = settings.SimulatedDuplicatesChance;
        config.SetMessageTypeEnabled(NetIncomingMessageType.VerboseDebugMessage, settings.VerboseLogging);
        config.SetMessageTypeEnabled(NetIncomingMessageType.WarningMessage, settings.WarningLogging);
        config.SetMessageTypeEnabled(NetIncomingMessageType.ErrorMessage, settings.ErrorLogging);
        config.LogRateLimiterEnabled = settings.LogRateLimitEnabled;
        config.LogRateLimitTargets = (NetLogRateLimitTarget) settings.LogRateLimitTargets;
        config.LogRateLimitBurst = settings.LogRateLimitBurst;
        config.LogRateLimitWindow = settings.LogRateLimitWindow;
    }

    public LidgrenTransportConnection GetConnection(NetConnection connection)
    {
        if (connection.Tag is LidgrenTransportConnection adapter)
            return adapter;

        var created = new LidgrenTransportConnection(connection);
        connection.Tag = created;
        return created;
    }
}

internal sealed class LidgrenTransportConnection : INetTransportConnection
{
    private readonly NetConnection _connection;

    public LidgrenTransportConnection(NetConnection connection)
    {
        _connection = connection;
    }

    public IPEndPoint RemoteEndPoint => _connection.RemoteEndPoint;

    public long RemoteUniqueIdentifier => _connection.RemoteUniqueIdentifier;

    public float AverageRoundtripTime => _connection.AverageRoundtripTime;

    public float RemoteTimeOffset => _connection.RemoteTimeOffset;

    public int CurrentMtu => _connection.CurrentMTU;

    public NetConnectionStatus Status => _connection.Status;

    public bool CanSendImmediately(NetDeliveryMethod method, int sequenceChannel)
    {
        return _connection.CanSendImmediately(method, sequenceChannel);
    }

    public bool Send(NetOutgoingMessage message, NetDeliveryMethod method, int sequenceChannel)
    {
        return _connection.SendMessage(message, method, sequenceChannel)
            is NetSendResult.Sent or NetSendResult.Queued;
    }

    public void Disconnect(string reason, bool sendBye = true)
    {
        _connection.Disconnect(reason, sendBye);
    }

    public NetOutgoingMessage CreateMessage(int initialCapacity = 0)
    {
        return initialCapacity > 0
            ? _connection.Peer.CreateMessage(initialCapacity)
            : _connection.Peer.CreateMessage();
    }
}
