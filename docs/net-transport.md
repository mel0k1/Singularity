# Network transport study

Roadmap item 3. The seam: everything above `INetChannel` stays C# — `NetManager`, `IntegrationNetManager`, channels, string tables, GameStates, the encryption handshake. What moves to Rust is the thing that owns the socket. This crate is that layer, and as of this round it speaks the vendored engine fork's Lidgren wire format byte for byte — the spike vocabulary (poll-driven events, connection table, `sg_transport_*` FFI) stayed, the 9-byte toy framing is gone.

## What landed

### The framer (round 3)

- `wire.rs` — the byte format of the vendored `Lidgren.Network` fork: the 5-byte header `[kind][seq:15 | frag:1][payload bits:16]`, varuint fragment headers (`group, totalBits, chunkSize, chunkNumber`), handshake payloads (app identifier string, i64 unique identifier, f32 local time, hail), ack triples, ping/pong payloads. Every layout decision was read off the C# source (`NetOutgoingMessage.Encode`, `NetFragmentationHelper`, `NetConnection.Handshake`), not inferred from packets.
- `channels.rs` — Lidgren's selective-repeat ARQ per channel: separate 1024-entry sequence spaces, 99 channel slots (kind = delivery method + sequence channel, exactly the `NetMessageType` byte), reliable window 64 / unreliable window 128, early-ack tracking with the hole-in-sequence heuristic (resend once-sent messages when `now - last_sent >= 0.35 × resend delay`), and all five receiver behaviors (unreliable unordered, unreliable sequenced, reliable unordered, reliable ordered with withholding, reliable sequenced).
- `protocol.rs` — the connection state machine: Connect/ConnectResponse/ConnectionEstablished handshake with app-identifier validation (`RobustToolbox` by default, matching `net.lidgren_app_identifier`) and retransmit with attempt caps; acks queued per received message and packed `(kind, seq u16 LE)`-per-3-bytes into `Acknowledge` messages (seq 0) exactly like `NetConnection.Heartbeat`; ping/pong with the C# RTT EMA (0.7/0.3) and remote-time-offset averaging, resend delay 25 ms + 2.1×RTT adaptive; fragmentation send (group ids from 1 wrapping at 65534, `GetBestChunkSize` search ported verbatim, the header length field includes the fragment header bits) and receive (per-group chunk masks, 30 s group expiry, 64 concurrent groups, 32 MiB reassembly cap per connection); `ExpandMTURequest` answered with `ExpandMTUSuccess` (responder only — the engine doesn't expand by default either).
- `lib.rs` — the transport: datagrams coalesce multiple messages up to MTU 700 (mirroring `net.mtu`), oversized unreliable messages ride single datagrams (the fork's `IgnoreMTU` default), reliable messages above MTU fragment up to 16 MiB groups, timeouts recompute from last activity each tick so `SetTimings` applies to live connections, and frame-boundary crypto through `singularity-crypto` — one `Session` per connection, initiator uses client nonces, accepted side uses server nonces, `[u64 counter][ciphertext+tag]` identical to the engine's `NetEncryption`.
- FFI moved to ABI 2: added `sg_transport_set_key`, `sg_transport_set_timings`, `sg_transport_set_app_identifier`, `sg_transport_set_mtu`/`sg_transport_mtu`, `sg_transport_disconnect(reason, send_bye)`, `sg_transport_disconnect_reason`, `sg_transport_rtt`, `sg_transport_time_offset` and `sg_transport_fetch`. Events still carry 1200 B inline; bigger payloads set the overflow flag and come out of a FIFO via `fetch`, so the pinned event array stays small while fragmented payloads of any size flow.

### The C# seam (round 2, still in)

`net.backend` (`"lidgren"` default, `"singularity"` opt-in) selects the channel layer in `csharp/engine/Robust.Shared/Network/Transport/`: `SgTransportNative` (ABI 2 bindings), `SgTransport` (socket owner, event pump, overflow fetch, reason/RTT queries) and `SgTransportChannel : INetChannel` — now with real `Ping` (from pong-measured RTT), real `RemoteTimeOffset`, transport `CurrentMtu`, the full delivery mapping (`kind = NetDeliveryMethod + SequenceChannel`, so reliable-unordered and reliable-sequenced keep their semantics instead of collapsing into ordered), and `Disconnect(reason)` that actually crosses the wire.

### The wire-compat test

`Robust.Shared.IntegrationTests/Networking/SgTransportWireCompatTest.cs` puts a Rust transport endpoint against a real, unmodified C# `NetServer` from the vendored fork over loopback UDP:

- the full handshake including `ConnectionApproval` → `Approve()`;
- encrypted payloads both directions — the Rust side encrypts/decrypts with `singularity-crypto`, the C# side with the engine's own `NetEncryption`, so the cross-language frame format is proven, not just asserted;
- reliable-ordered and unreliable exchange;
- an 8 KB message each way — Lidgren fragments it into ~13 datagrams at MTU 700, Rust reassembles (and vice versa), which also exercises the FFI overflow-fetch path on the C# side;
- RTT measured on both sides (`AverageRoundtripTime` on the C# connection, `sg_transport_rtt` on the Rust side);
- disconnect reasons surviving the wire in both directions.

Both tests pass in the CI csharp job alongside the seam's loopback pair test.

## Method

Loopback on the 2-core host, one process, release build. `cargo run --release -p singularity-net-transport --bin bench`. Loss injection stays in the send path (xorshift dice) so reliability is tested without harness machinery. The C# side runs under `dotnet test` with the freshly built `libsingularity_net_transport.so`.

## Results

| payload | delivery | msgs/s | delivered | note |
| --- | --- | --- | --- | --- |
| 64 B | unreliable | 141,367 | 6400/6400 | loopback socket-bound |
| 512 B | unreliable | 121,973 | 6400/6400 | 62.5 MB/s |
| 1200 B | unreliable | 118,145 | 6400/6400 | 141.8 MB/s |
| 64 B | reliable ordered | 360,172 | 400/400 | window 64 |
| 512 B | reliable ordered | 244,929 | 400/400 | window 64 |
| 1200 B | reliable ordered | 126,290 | 400/400 | fragmented into 2 chunks at MTU 700 |

- The windowed ARQ replaced stop-and-wait: reliable throughput went from ~9 k msgs/s (64 B, spike) to ~360 k msgs/s — 39× — because the pipe is no longer one-message-deep. The reliable numbers are loopback-RTT-bound, not window-bound.
- `send()` is now enqueue-only (65 ns/call, no syscall); datagrams leave on `poll()`, which is where the C# heartbeat sends them too. Unreliable bursts that outrun the 128-slot window are dropped at enqueue, exactly like `NetUnreliableSenderChannel`.
- The bench caught kernel reality when pushed past it: a burst of 8191 B unreliable datagrams (single-datagram sends per the fork's `IgnoreMTU` default) overruns the socket buffers and loses data — which is precisely why the engine caps MTU at 700 and fragments everything reliable. The bench measures 64/512/1200 B; the payload-bounds test still covers the 8191 B accept/reject edges.
- Rust tests: 19 unit (header/varuint/fragment-header/ack-packing roundtrips, coalescing, chunk-size search, window and receiver behaviors port-matched to Lidgren) plus 10 integration (handshake both sides, unreliable echo, reliable-under-loss at 30% both directions in order, 16 KB fragmented roundtrip, disconnect with reason, silence timeout, payload bounds including the 16 MiB group cap, RTT on both ends, 8-client echo, and the full FFI surface end-to-end including key install, overflow fetch, RTT and reason queries).
- One port bug caught against the C# source before it could ship: the draft's unreliable sender advanced its flow-control window *past* an early ack instead of *to* it (Lidgren's `while (windowStart != seqNr)` loop) — off by one slot per early ack. A second draft bug — a fixed `timeout_deadline` captured at connect — would have made runtime `SetTimings` invisible to live connections; timeouts now recompute from last activity every tick.

## Honest caps

- `NetManager`'s own funnels still run Lidgren — the seam plugs into whoever owns the channels; engine wiring is the next milestone, not this one.
- MTU expansion is responder-only (the engine default `net.mtu_expand=false` never initiates either).
- Unconnected messages, discovery, NAT introduction and the ConnectResponse port-change reroute are not implemented; the transport drops what it does not speak.
- `RemoteTime` on the seam is the offset until the channel can see the engine clock.
- Events carry 1200 B inline; larger payloads take the FIFO + `fetch` path, which costs one extra copy on the C# side.

## Next steps

1. Engine wiring: `NetManager` client/server funnels over `SgTransport` under `net.backend=singularity` — the approval callback, string tables and GameStates over the seam, then a real client↔server integration pair in CI on both backends.
2. MTU expansion initiation behind `net.mtu_expand` (binary search between largest-success/failure like the fork).
3. Pacing: the fork coalesces ack/sends across ~3 heartbeat frames (10 ms) while this transport flushes per poll — fine on loopback, worth measuring under real GameStates before calling it done.
4. Window tuning is untouched on purpose: the constants are Lidgren's, so behavior matches first and diverges measurably later.
