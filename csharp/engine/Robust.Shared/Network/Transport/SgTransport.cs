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

    public IReadOnlyCollection<SgTransportChannel> Channels => _channels.Values;

    internal ISgTransportHost Host => _host;

    internal IRobustSerializer Serializer => _serializer;

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

        channel.MarkDisconnected();
        _host.Disconnected(channel, "connection closed");
    }

    private unsafe void OnReceived(ulong connection, SgTransportNative.SgNetEvent source)
    {
        if (!_channels.TryGetValue(connection, out var channel))
            return;

        var length = (int) source.Length;
        if (length < 0 || length > SgTransportNative.MaxDatagram)
            return;

        var payload = new Span<byte>(source.Data, length).ToArray();
        var message = new NetIncomingMessage();
        message.Data = payload;
        message.LengthBytes = length;
        message.Position = 0;
        _host.Received(channel, message);
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

    internal void CloseChannel(ulong connection)
    {
        if (_handle == IntPtr.Zero)
            return;

        SgTransportNative.sg_transport_close(_handle, connection);
    }
}
