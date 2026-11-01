using System;
using System.Collections.Generic;
using System.Net;
using System.Text;
using Lidgren.Network;

namespace Robust.Shared.Network.Transport;

internal sealed class SgNetTransportPeer : INetTransportPeer
{
    private const int PollCapacity = 256;
    private const int EndpointBytes = 64;

    private readonly Dictionary<ulong, SgNetTransportConnection> _connections = new();
    private readonly SgTransportNative.SgNetEvent[] _events = new SgTransportNative.SgNetEvent[PollCapacity];
    private IntPtr _handle;
    private NetTransportSettings _settings;

    private SgNetTransportPeer(IntPtr handle, NetTransportSettings settings)
    {
        var abi = SgTransportNative.sg_transport_abi_version();
        if (abi != SgTransportNative.AbiVersion)
            throw new InvalidOperationException($"transport ffi abi {abi} != {SgTransportNative.AbiVersion}");

        _handle = handle;
        _settings = settings;
    }

    public static SgNetTransportPeer? Bind(IPEndPoint localEndPoint, NetTransportSettings settings)
    {
        SgTransportNative.EnsureLoaded();
        var handle = SgTransportNative.sg_transport_bind(localEndPoint.ToString());
        if (handle == IntPtr.Zero)
            return null;

        var peer = new SgNetTransportPeer(handle, settings);
        peer.ApplySettings(settings);
        return peer;
    }

    public int LocalPort => _handle == IntPtr.Zero ? 0 : SgTransportNative.sg_transport_local_port(_handle);

    public void Start()
    {
    }

    public void Shutdown(string reason)
    {
        if (_handle == IntPtr.Zero)
            return;

        SgTransportNative.sg_transport_destroy(_handle);
        _handle = IntPtr.Zero;
        _connections.Clear();
    }

    public bool IsShutdownPending => false;

    public int ConnectionsCount => _connections.Count;

    public NetTransportStatistics Statistics
    {
        get
        {
            if (_handle == IntPtr.Zero)
                return default;

            unsafe
            {
                var stats = new SgTransportNative.SgNetStats();
                SgTransportNative.sg_transport_stats(_handle, &stats);
                return new NetTransportStatistics
                {
                    SentBytes = (long) stats.BytesSent,
                    ReceivedBytes = (long) stats.BytesReceived,
                    SentPackets = (long) stats.DatagramsSent,
                    ReceivedPackets = (long) stats.DatagramsReceived,
                    SentMessages = (long) stats.DatagramsSent,
                    ReceivedMessages = (long) stats.DatagramsReceived,
                    ResentDueToDelay = (long) stats.Retransmits,
                };
            }
        }
    }

    public INetTransportConnection? Connect(IPEndPoint endPoint)
    {
        if (_handle == IntPtr.Zero)
            return null;

        var id = SgTransportNative.sg_transport_connect(_handle, endPoint.ToString());
        if (id == 0)
            return null;

        var connection = new SgNetTransportConnection(this, id, endPoint, NetConnectionStatus.InitiatedConnect);
        _connections.Add(id, connection);
        return connection;
    }

    public void ApplySettings(NetTransportSettings settings)
    {
        if (_handle == IntPtr.Zero)
            return;

        _settings = settings;
        SgTransportNative.sg_transport_set_app_identifier(_handle, settings.AppIdentifier);
        SgTransportNative.sg_transport_set_mtu(_handle, checked((uint) Math.Max(1, settings.Mtu)));
        var expandFrequencyMs = Math.Max(
            1,
            (uint) TimeSpan.FromSeconds(Math.Max(0.001f, settings.MtuExpandFrequency)).TotalMilliseconds);
        SgTransportNative.sg_transport_set_mtu_expand(
            _handle,
            settings.MtuExpand ? 1u : 0u,
            expandFrequencyMs,
            checked((uint) Math.Max(1, settings.MtuExpandFailAttempts)));
        var pingMs = Math.Max(1, (uint) TimeSpan.FromSeconds(settings.PingInterval).TotalMilliseconds);
        var timeoutMs = Math.Max(1, (uint) TimeSpan.FromSeconds(settings.ConnectionTimeout).TotalMilliseconds);
        SgTransportNative.sg_transport_set_timings(_handle, pingMs, timeoutMs, 0);
        SgTransportNative.sg_transport_set_pacing(
            _handle,
            (uint) Math.Max(0, settings.PacingMilliseconds));
    }

    public unsafe void Pump(List<NetTransportEvent> events)
    {
        if (_handle == IntPtr.Zero)
            return;

        int count;
        unsafe
        {
            fixed (SgTransportNative.SgNetEvent* events2 = _events)
            {
                count = checked((int) SgTransportNative.sg_transport_poll(_handle, events2, (nuint) PollCapacity));
            }
        }

        for (var i = 0; i < count; i++)
        {
            var source = _events[i];
            switch (source.Kind)
            {
                case SgTransportNative.EventConnected:
                {
                    var connection = ResolveConnection(source.Connection);
                    if (connection == null)
                        break;

                    connection.SetStatus(NetConnectionStatus.Connected);
                    events.Add(new NetTransportEvent(
                        NetTransportEventKind.Connected,
                        connection));
                    break;
                }

                case SgTransportNative.EventDisconnected:
                {
                    if (!_connections.Remove(source.Connection, out var connection))
                        break;

                    connection.SetStatus(NetConnectionStatus.Disconnected);
                    var reasonLength = (int) source.Length;
                    string reason;
                    if (reasonLength > 0 && reasonLength <= SgTransportNative.MaxEventPayload)
                    {
                        reason = Encoding.UTF8.GetString(new Span<byte>(source.Data, reasonLength));
                    }
                    else
                    {
                        reason = "connection closed";
                    }

                    events.Add(new NetTransportEvent(
                        NetTransportEventKind.Disconnected,
                        connection,
                        reason: reason));
                    break;
                }

                case SgTransportNative.EventReceived:
                {
                    if (!_connections.TryGetValue(source.Connection, out var connection))
                        break;

                    var length = (int) source.Length;
                    if (length <= 0)
                        break;

                    byte[] payload;
                    if (source.Reserved == SgTransportNative.PayloadOverflow
                        || length > SgTransportNative.MaxEventPayload)
                    {
                        payload = FetchOverflow(length);
                    }
                    else
                    {
                        payload = new byte[length];
                        unsafe
                        {
                            fixed (byte* dst = payload)
                            {
                                Buffer.MemoryCopy(source.Data, dst, length, length);
                            }
                        }
                    }

                    if (payload.Length != length)
                        break;

                    events.Add(new NetTransportEvent(
                        NetTransportEventKind.Data,
                        connection,
                        payload: payload));
                    break;
                }
            }
        }
    }

    private SgNetTransportConnection? ResolveConnection(ulong id)
    {
        if (_connections.TryGetValue(id, out var connection))
            return connection;

        var endPoint = QueryRemoteEndpoint(id);
        if (endPoint == null)
            return null;

        connection = new SgNetTransportConnection(this, id, endPoint, NetConnectionStatus.None);
        _connections.Add(id, connection);
        return connection;
    }

    private IPEndPoint? QueryRemoteEndpoint(ulong id)
    {
        var buffer = new byte[EndpointBytes];
        if (SgTransportNative.sg_transport_remote_endpoint(_handle, id, buffer, (nuint) EndpointBytes) != 1)
            return null;

        var end = Array.IndexOf(buffer, (byte) 0);
        if (end <= 0)
            return null;

        return IPEndPoint.Parse(Encoding.ASCII.GetString(buffer, 0, end));
    }

    private unsafe byte[] FetchOverflow(int length)
    {
        var buffer = new byte[length];
        fixed (byte* dst = buffer)
        {
            var fetched = (int) SgTransportNative.sg_transport_fetch(_handle, dst, (nuint) buffer.Length);
            return fetched == length ? buffer : Array.Empty<byte>();
        }
    }

    internal bool Send(ulong id, uint kind, ReadOnlySpan<byte> payload)
    {
        if (_handle == IntPtr.Zero || payload.IsEmpty)
            return false;

        unsafe
        {
            fixed (byte* data = payload)
            {
                return SgTransportNative.sg_transport_send(_handle, id, kind, data, (nuint) payload.Length) != 0;
            }
        }
    }

    internal void Close(ulong id, string reason, bool sendBye)
    {
        if (_handle == IntPtr.Zero)
            return;

        SgTransportNative.sg_transport_disconnect(_handle, id, reason, sendBye ? 1u : 0u);
    }

    internal IntPtr HandleForQueries => _handle;

    internal NetTransportSettings Settings => _settings;
}

internal sealed class SgNetTransportConnection : INetTransportConnection
{
    private readonly SgNetTransportPeer _peer;
    private readonly ulong _id;
    private volatile NetConnectionStatus _status;

    public SgNetTransportConnection(
        SgNetTransportPeer peer,
        ulong id,
        IPEndPoint remoteEndPoint,
        NetConnectionStatus initialStatus)
    {
        _peer = peer;
        _id = id;
        RemoteEndPoint = remoteEndPoint;
        _status = initialStatus;
    }

    public IPEndPoint RemoteEndPoint { get; }

    public long RemoteUniqueIdentifier => SgTransportNative.sg_transport_remote_identifier(_peer.HandleForQueries, _id);

    public float AverageRoundtripTime => SgTransportNative.sg_transport_rtt(_peer.HandleForQueries, _id);

    public float RemoteTimeOffset => SgTransportNative.sg_transport_time_offset(_peer.HandleForQueries, _id);

    public int CurrentMtu => checked((int) SgTransportNative.sg_transport_connection_mtu(_peer.HandleForQueries, _id));

    public NetConnectionStatus Status => _status;

    public bool CanSendImmediately(NetDeliveryMethod method, int sequenceChannel)
    {
        return _status == NetConnectionStatus.Connected;
    }

    public bool Send(NetOutgoingMessage message, NetDeliveryMethod method, int sequenceChannel)
    {
        return _peer.Send(_id, (uint) method + (uint) sequenceChannel, message.Data.AsSpan(0, message.LengthBytes));
    }

    public void Disconnect(string reason, bool sendBye = true)
    {
        _peer.Close(_id, reason, sendBye);
    }

    public NetOutgoingMessage CreateMessage(int initialCapacity = 0)
    {
        var message = new NetOutgoingMessage();
        message.Data = new byte[Math.Max(initialCapacity, 16)];
        return message;
    }

    internal void SetStatus(NetConnectionStatus status)
    {
        _status = status;
    }
}
