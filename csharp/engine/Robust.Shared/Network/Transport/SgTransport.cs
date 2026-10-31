using System;
using System.Collections.Generic;
using System.Net;
using System.Text;
using Lidgren.Network;
using Robust.Shared.Network;
using Robust.Shared.Serialization;

namespace Robust.Shared.Network.Transport;

public sealed class SgTransport : IDisposable
{
    private const int PollCapacity = 256;
    private const int EndpointBytes = 64;

    private readonly ISgTransportHost _host;
    private readonly IRobustSerializer _serializer;
    private readonly Dictionary<ulong, SgTransportChannel> _channels = new();
    private readonly SgTransportNative.SgNetEvent[] _events = new SgTransportNative.SgNetEvent[PollCapacity];
    private readonly byte[] _scratch = new byte[4096];
    private IntPtr _handle;

    private SgTransport(ISgTransportHost host, IRobustSerializer serializer, IntPtr handle)
    {
        SgTransportNative.EnsureLoaded();
        var abi = SgTransportNative.sg_transport_abi_version();
        if (abi != SgTransportNative.AbiVersion)
            throw new InvalidOperationException($"transport ffi abi {abi} != {SgTransportNative.AbiVersion}");
        _host = host;
        _serializer = serializer;
        _handle = handle;
    }

    public static SgTransport Bind(ISgTransportHost host, IRobustSerializer serializer, IPEndPoint localEndPoint)
    {
        SgTransportNative.EnsureLoaded();
        var handle = SgTransportNative.sg_transport_bind(localEndPoint.ToString());
        if (handle == IntPtr.Zero)
            throw new InvalidOperationException($"sg_transport_bind failed for {localEndPoint}");
        return new SgTransport(host, serializer, handle);
    }

    public SgTransportChannel Connect(IPEndPoint endPoint)
    {
        var connection = SgTransportNative.sg_transport_connect(_handle, endPoint.ToString());
        if (connection == 0)
            throw new InvalidOperationException($"sg_transport_connect failed for {endPoint}");
        var channel = new SgTransportChannel(this, connection, endPoint);
        _channels.Add(connection, channel);
        return channel;
    }

    public ushort LocalPort => _handle == IntPtr.Zero ? (ushort) 0 : SgTransportNative.sg_transport_local_port(_handle);

    public int Mtu => _handle == IntPtr.Zero ? 0 : checked((int) SgTransportNative.sg_transport_mtu(_handle));

    public int ConnectionMtu(ulong connection)
    {
        return _handle == IntPtr.Zero
            ? 0
            : checked((int) SgTransportNative.sg_transport_connection_mtu(_handle, connection));
    }

    public IReadOnlyCollection<SgTransportChannel> Channels => _channels.Values;

    internal ISgTransportHost Host => _host;

    internal IRobustSerializer Serializer => _serializer;

    public void SetAppIdentifier(string appIdentifier)
    {
        SgTransportNative.sg_transport_set_app_identifier(_handle, appIdentifier);
    }

    public void SetMtu(int mtu)
    {
        SgTransportNative.sg_transport_set_mtu(_handle, checked((uint) mtu));
    }

    public void SetMtuExpand(bool enabled, TimeSpan frequency, int failAttempts)
    {
        SgTransportNative.sg_transport_set_mtu_expand(
            _handle,
            enabled ? 1u : 0u,
            Math.Max(1, (uint) frequency.TotalMilliseconds),
            checked((uint) Math.Max(1, failAttempts)));
    }

    public void SetTimings(TimeSpan pingInterval, TimeSpan timeout, TimeSpan resendDelay)
    {
        SgTransportNative.sg_transport_set_timings(
            _handle,
            Math.Max(1, (uint) pingInterval.TotalMilliseconds),
            Math.Max(1, (uint) timeout.TotalMilliseconds),
            (uint) resendDelay.TotalMilliseconds);
    }

    public bool SetEncryptionKey(ulong connection, byte[] key)
    {
        if (key.Length != 32)
            throw new ArgumentException("transport keys must be 32 bytes");

        unsafe
        {
            fixed (byte* material = key)
            {
                return SgTransportNative.sg_transport_set_key(_handle, connection, material, (nuint) key.Length) != 0;
            }
        }
    }

    public float Rtt(ulong connection)
    {
        return SgTransportNative.sg_transport_rtt(_handle, connection);
    }

    public float RemoteTimeOffsetSeconds(ulong connection)
    {
        return SgTransportNative.sg_transport_time_offset(_handle, connection);
    }

    public string? DisconnectReason(ulong connection)
    {
        unsafe
        {
            fixed (byte* buffer = _scratch)
            {
                var written = SgTransportNative.sg_transport_disconnect_reason(
                    _handle, connection, buffer, (nuint) _scratch.Length);
                if (written == 0)
                    return null;

                var end = 0;
                while (end < _scratch.Length && _scratch[end] != 0)
                    end++;

                return Encoding.UTF8.GetString(_scratch, 0, end);
            }
        }
    }

    public void Pump()
    {
        if (_handle == IntPtr.Zero)
            return;

        int count;
        unsafe
        {
            fixed (SgTransportNative.SgNetEvent* events = _events)
            {
                count = checked((int) SgTransportNative.sg_transport_poll(_handle, events, (nuint) PollCapacity));
            }
        }

        for (var i = 0; i < count; i++)
        {
            var source = _events[i];
            switch (source.Kind)
            {
                case SgTransportNative.EventConnected:
                    OnConnected(source.Connection);
                    break;
                case SgTransportNative.EventDisconnected:
                    OnDisconnected(source.Connection);
                    break;
                case SgTransportNative.EventReceived:
                    OnReceived(source.Connection, source);
                    break;
            }
        }
    }

    public void Dispose()
    {
        if (_handle == IntPtr.Zero)
            return;

        foreach (var connection in _channels.Keys)
        {
            SgTransportNative.sg_transport_close(_handle, connection);
        }

        _channels.Clear();
        SgTransportNative.sg_transport_destroy(_handle);
        _handle = IntPtr.Zero;
    }

    private void OnConnected(ulong connection)
    {
        if (_channels.TryGetValue(connection, out var channel))
        {
            channel.MarkEstablished();
            _host.Connected(channel);
            return;
        }

        var endPoint = QueryRemoteEndPoint(connection);
        if (endPoint == null)
            return;

        channel = new SgTransportChannel(this, connection, endPoint);
        channel.MarkEstablished();
        _channels.Add(connection, channel);
        _host.Connected(channel);
    }

    private void OnDisconnected(ulong connection)
    {
        if (!_channels.Remove(connection, out var channel))
            return;

        var reason = DisconnectReason(connection) ?? "connection closed";
        channel.MarkDisconnected();
        _host.Disconnected(channel, reason);
    }

    private unsafe void OnReceived(ulong connection, SgTransportNative.SgNetEvent source)
    {
        if (!_channels.TryGetValue(connection, out var channel))
            return;

        var length = (int) source.Length;
        if (length <= 0)
            return;

        byte[] payload;
        if (source.Reserved == SgTransportNative.PayloadOverflow || length > SgTransportNative.MaxEventPayload)
        {
            payload = FetchOverflow(length);
        }
        else
        {
            payload = new Span<byte>(source.Data, length).ToArray();
        }

        var message = new NetIncomingMessage();
        message.Data = payload;
        message.LengthBytes = length;
        message.Position = 0;
        _host.Received(channel, message);
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

    private IPEndPoint? QueryRemoteEndPoint(ulong connection)
    {
        var buffer = new byte[EndpointBytes];
        if (SgTransportNative.sg_transport_remote_endpoint(_handle, connection, buffer, (nuint) EndpointBytes) != 1)
            return null;

        var end = Array.IndexOf(buffer, (byte) 0);
        if (end <= 0)
            return null;

        return IPEndPoint.Parse(Encoding.ASCII.GetString(buffer, 0, end));
    }

    internal bool Send(ulong connection, uint delivery, ReadOnlySpan<byte> payload)
    {
        if (_handle == IntPtr.Zero || payload.IsEmpty)
            return false;

        unsafe
        {
            fixed (byte* data = payload)
            {
                return SgTransportNative.sg_transport_send(_handle, connection, delivery, data, (nuint) payload.Length) != 0;
            }
        }
    }

    internal void CloseChannel(ulong connection, string reason, bool sendBye = true)
    {
        if (_handle == IntPtr.Zero)
            return;

        SgTransportNative.sg_transport_disconnect(_handle, connection, reason, sendBye ? 1u : 0u);
    }
}
