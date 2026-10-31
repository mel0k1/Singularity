using Lidgren.Network;
using Robust.Shared.Network;

namespace Robust.Shared.Network.Transport;

public interface ISgTransportHost
{
    public INetManager? NetPeer { get; }

    public int GetMessageId(NetMessage message);

    public void Connected(SgTransportChannel channel);

    public void Disconnected(SgTransportChannel channel, string reason);

    public void Received(SgTransportChannel channel, NetIncomingMessage message);
}
