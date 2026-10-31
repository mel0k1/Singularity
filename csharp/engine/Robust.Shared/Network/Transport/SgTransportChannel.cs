using System;
using System.Net;
using Lidgren.Network;
using Robust.Shared.Network;
using Robust.Shared.Serialization;

namespace Robust.Shared.Network.Transport;

public sealed class SgTransportChannel : INetChannel
{
    private readonly SgTransport _transport;
    private readonly ulong _connection;
    private NetEncryption? _encryption;
    private bool _established;
    private bool _closed;

    internal SgTransportChannel(SgTransport transport, ulong connection, IPEndPoint remoteEndPoint)
    {
        _transport = transport;
        _connection = connection;
        RemoteEndPoint = remoteEndPoint;
    }

    public INetManager NetPeer => _transport.Host.NetPeer!;

    public long ConnectionId => (long) _connection;

    public IPEndPoint RemoteEndPoint { get; }

    public NetUserData UserData { get; set; } = new(new NetUserId(Guid.Empty), string.Empty);

    public NetUserId UserId => UserData.UserId;

    public string UserName => UserData.UserName;

    public LoginType AuthType { get; set; }

    public TimeSpan RemoteTimeOffset => TimeSpan.Zero;

    public TimeSpan RemoteTime => TimeSpan.Zero;

    public short Ping => 0;

    public bool IsConnected => _established && !_closed;

    public bool IsHandshakeComplete { get; set; }

    public int CurrentMtu => SgTransportNative.MaxDatagram;

    internal void MarkEstablished()
    {
        _established = true;
    }

    internal void MarkDisconnected()
    {
        _established = false;
        _closed = true;
    }

    public void EnableEncryption(byte[] key, bool isServer)
    {
        _encryption = new NetEncryption(key, isServer);
    }

    public bool TryDecrypt(NetIncomingMessage message)
    {
        return _encryption == null || _encryption.TryDecrypt(message);
    }

    public T CreateNetMessage<T>()
        where T : NetMessage, new()
    {
        return new T();
    }

    public void SendMessage(NetMessage message)
    {
        var id = _transport.Host.GetMessageId(message);
        var overhead = _encryption != null ? NetEncryption.EncryptionOverhead : 0;
        var packet = new NetOutgoingMessage();
        packet.Data = new byte[message.EstimateBufferSize() + overhead];
        packet.Write((byte) id);
        message.WriteToBuffer(packet, _transport.Serializer);
        _encryption?.Encrypt(packet);

        if (!_transport.Send(_connection, MapDelivery(message.DeliveryMethod), packet.Data.AsSpan(0, packet.LengthBytes)))
            throw new NetManagerException($"sg_transport rejected a {packet.LengthBytes} byte payload");
    }

    public void Disconnect(string reason)
    {
        Disconnect(reason, true);
    }

    public void Disconnect(string reason, bool sendBye)
    {
        if (IsConnected)
            _transport.CloseChannel(_connection);
    }

    public bool CanSendImmediately(NetDeliveryMethod method, int sequenceChannel)
    {
        return IsConnected;
    }

    public override string ToString()
    {
        return $"{ConnectionId}/{UserId}";
    }

    private static uint MapDelivery(NetDeliveryMethod method)
    {
        return method switch
        {
            NetDeliveryMethod.ReliableUnordered
                or NetDeliveryMethod.ReliableSequenced
                or NetDeliveryMethod.ReliableOrdered => SgTransportNative.DeliveryReliableOrdered,
            _ => SgTransportNative.DeliveryUnreliable,
        };
    }
}
