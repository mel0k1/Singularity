using System;
using System.Collections.Immutable;
using System.Net;
using System.Threading.Channels;
using System.Threading.Tasks;
using Lidgren.Network;
using Robust.Shared.Network.Transport;
using Robust.Shared.ViewVariables;

namespace Robust.Shared.Network
{
    public partial class NetManager
    {
        private sealed class NetChannel : INetChannel
        {
            private readonly NetManager _manager;
            internal readonly INetTransportConnection Connection;

            /// <inheritdoc />
            [ViewVariables]
            public long ConnectionId => Connection.RemoteUniqueIdentifier;

            /// <inheritdoc />
            [ViewVariables]
            public INetManager NetPeer => _manager;

            [ViewVariables] public string UserName => UserData.UserName;
            [ViewVariables] public LoginType AuthType { get; }
            [ViewVariables] public TimeSpan RemoteTimeOffset => TimeSpan.FromSeconds(Connection.RemoteTimeOffset);
            [ViewVariables] public TimeSpan RemoteTime => _manager._timing.RealTime + RemoteTimeOffset;

            /// <inheritdoc />
            [ViewVariables]
            public short Ping => (short) Math.Round(Connection.AverageRoundtripTime * 1000);

            /// <inheritdoc />
            [ViewVariables]
            public bool IsConnected => Connection.Status == NetConnectionStatus.Connected;

            /// <inheritdoc />
            public IPEndPoint RemoteEndPoint => Connection.RemoteEndPoint;

            [ViewVariables] public NetUserId UserId => UserData.UserId;
            [ViewVariables] public NetUserData UserData { get; }

            public bool IsHandshakeComplete { get; set; }

            // Only used on server, contains the encryption to use for this channel.
            public NetEncryption? Encryption { get; set; }

            [ViewVariables] public int CurrentMtu => Connection.CurrentMtu;

            public ChannelWriter<EncryptChannelItem>? EncryptionChannel;
            public Task? EncryptionChannelTask;

            /// <summary>
            ///     Creates a new instance of a NetChannel.
            /// </summary>
            /// <param name="manager">The server this channel belongs to.</param>
            /// <param name="connection">The transport connection to the remote peer.</param>
            internal NetChannel(NetManager manager, INetTransportConnection connection, NetUserData userData,
                LoginType loginType)
            {
                _manager = manager;
                Connection = connection;
                AuthType = loginType;
                UserData = userData;
            }

            /// <inheritdoc />
            public T CreateNetMessage<T>()
                where T : NetMessage, new()
            {
                return _manager.CreateNetMessage<T>();
            }

            /// <inheritdoc />
            public void SendMessage(NetMessage message)
            {
                if (_manager.IsClient)
                {
                    _manager.ClientSendMessage(message);
                    return;
                }

                _manager.ServerSendMessage(message, this);
            }

            /// <inheritdoc />
            public void Disconnect(string reason)
            {
                Disconnect(reason, true);
            }

            public void Disconnect(string reason, bool sendBye)
            {
                if (Connection.Status == NetConnectionStatus.Connected)
                    Connection.Disconnect(reason, sendBye);
            }

            public bool CanSendImmediately(NetDeliveryMethod method, int sequenceChannel)
            {
                return Connection.CanSendImmediately(method, sequenceChannel);
            }

            public override string ToString()
            {
                return $"{ConnectionId}/{UserId}";
            }
        }
    }
}
