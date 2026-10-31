# Network transport study

Roadmap item 3. The seam: everything above `INetChannel` stays C# — `NetManager`, `IntegrationNetManager`, channels, string tables, GameStates, the encryption handshake. What moves to Rust is the thing that owns the socket. This spike builds that layer as `singularity-net-transport` and proves the vocabulary, the event model and the FFI boundary before anyone touches the Lidgren wire format.

## What landed

- `UdpTransport` — one non-blocking socket, a connection table keyed by endpoint, hello/hello-ack handshake, heartbeats, silence timeout, per-connection stats. Event-driven: the host polls, the transport never calls back (no reverse FFI at all — the deliberate opposite of the physics pair filter).
- Delivery methods: `Unreliable` (fire and forget, per-stream sequence stamps) and `ReliableOrdered` over a stop-and-wait ARQ with a 60 ms RTO — honest spike-grade reliability, chosen because it is trivially provable under loss, not because it is the destination. Lidgren's windowed ARQ is the production target; the seam does not change when it lands.
- The spike wire format is 9 bytes — `[kind][seq u64 LE][payload]` with six kinds (payload×2, ack, hello, hello-ack, disconnect, heartbeat). **It is not Lidgren's framing.** The framer is one function on each side by design; swapping it for the Lidgren fork's header (sequence/ack/fragment fields) is the next milestone and the reason this crate does not touch a single byte of the real protocol yet.
- FFI surface (`sg_transport_*`): bind, connect, send, poll-events-with-inline-payload, close, stats, destroy. Events carry up to 1200 B inline so C# pins exactly one array per poll — same pinned-buffer philosophy as the physics command stream.
- `singularity-crypto` already speaks the engine's frame encryption (`[u64 counter][ciphertext+tag]`, XChaCha20-Poly1305, byte-tested against the C# `NetEncryption`); when the Lidgren framer lands in Rust, encryption slots in at the frame boundary with no new seam.

## Method

Loopback on the 2-core host, one process, release build. `cargo run --release -p singularity-net-transport --bin bench`. Loss injection is built into the send path (xorshift dice, 0.3 both directions for the loss test) so reliability is tested without harness machinery.

## Results

| payload | delivery | msgs/s | delivered | note |
| --- | --- | --- | --- | --- |
| 64 B | unreliable | 220,222 | 6400/6400 | loopback socket-bound |
| 512 B | unreliable | 202,275 | 6400/6400 | 103.6 MB/s |
| 1200 B | unreliable | 221,419 | 6400/6400 | 265.7 MB/s |
| 64 B | reliable ordered | 9,214 | 400/400 | stop-and-wait ceiling |
| 512 B | reliable ordered | 8,451 | 400/400 | |
| 1200 B | reliable ordered | 9,208 | 400/400 | |

- `send()` path cost: ~850 ns per call including the `send_to` syscall.
- Loss test: 40 reliable messages through 30% loss in both directions — all delivered, strictly in order, retransmit counter > 0.
- 8 tests: handshake both sides, unreliable echo, reliable-under-loss, close notification, silence timeout, payload bounds (1200 B passes, 1201 B rejected), 8 clients echo, and the full FFI surface end-to-end through the C ABI.

One design bug caught by the bench before it could ship: unreliable and reliable streams initially shared a sequence counter, so a reliable message after any unreliable traffic looked out-of-order to the receiver and was dropped — the bench showed 0/400 delivered while the (reliable-only) test stayed green. Separate sequence spaces fixed it; the bench stays as the regression gate for exactly that class.

## Next steps

1. Study the Lidgren fork framing in the vendored engine: header layout, SRN sequence/ack bookkeeping, fragmentation, channel types — then port the framer behind the same `send_wire`/`handle_datagram` pair.
2. Wire-compat test: a Rust transport endpoint talking byte-level Lidgren protocol against the real C# `NetPeer` over loopback, `singularity-crypto` on the frame boundary.
3. The C# seam — done. `net.backend` (`"lidgren"` default, `"singularity"` opt-in) selects the channel layer in `csharp/engine/Robust.Shared/Network/Transport/`: `SgTransportNative` (the `sg_transport_*` bindings, `SINGULARITY_NET_TRANSPORT_FFI` override, ABI 1), `SgTransport` (socket owner, event pump, channel registry) and `SgTransportChannel : INetChannel` (engine payloads `[msg id][message]`, payload encryption through the engine's own `NetEncryption`, delivery mapped Reliable*→stop-and-wait and everything else to Unreliable). The FFI grew one call, `sg_transport_remote_endpoint`, because the event model carries ids, not addresses. `Robust.Shared.IntegrationTests/Networking/SgTransportTest.cs` runs a loopback pair over real UDP through the whole seam — handshake, encrypted reliable-ordered and unreliable payloads, ordered delivery across interleaved streams, reply path, close notification and silence timeout — and CI runs it in the csharp job. Honest caps until the framer lands: payloads over 1200 B are rejected (no fragmentation yet), no ping/RTT or clock sync (`Ping` 0), disconnect reasons do not cross the wire, and `NetManager`'s own funnels still run Lidgren — the seam plugs into whoever owns the channels, the engine wiring arrives with the framer milestone.
4. Windowed ARQ replacing stop-and-wait when the framer work forces the decision.
