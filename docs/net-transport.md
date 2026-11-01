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

### The NetManager funnels (round 4)

The seam now sits inside `NetManager` itself — `net.backend` (`"lidgren"` default, `"singularity"` opt-in) selects the transport for the real client and server, not just the standalone test classes:

- `Transport/NetTransport.cs` — the internal seam: `INetTransportPeer`/`INetTransportConnection` plus `LidgrenTransportPeer`/`LidgrenTransportConnection` adapters over the unmodified Lidgren types. The adapter is cached in `NetConnection.Tag`, so the Lidgren hot path gains one type check per message and one interface dispatch per send — nanoseconds against the serializer's microseconds; the ReadMessage/Recycle pump loop is untouched.
- `Transport/SgNetTransportPeer.cs` — the funnel peer over the `sg_transport_*` FFI: binds, connects, pumps events, maps `SgNetStats` into the engine's `NetworkStats`, and applies cvar settings (`net.mtu`, `net.lidgren_app_identifier`, `net.connection_timeout`, ping interval) through the same `NetTransportSettings` both backends share.
- `NetManager` was re-keyed from raw `NetConnection` to `INetTransportConnection` everywhere (`_channels`, `_awaitingData`/`_awaitingStatusChange`, `_assignedUserIds`, `NetPeerData`) — the dictionaries, the dispatch core, the status-changed core and the login handshake are backend-agnostic; `ProcessPackets` branches per peer type (Lidgren keeps its exact `ReadMessage` loop, the singularity peer pumps FFI events).
- Engine-level message encryption (`NetEncryption` on the channel) sits above the seam and works unchanged over both transports, exactly like it does over Lidgren.
- `NetManagerRealPairTest` runs a real headless client and server over loopback UDP through the full engine funnel — `ClientConnect` (happy eyeballs), guest login (`MsgLoginStart`/`MsgLoginSuccess`), the message string table, serializer and transfer handshakes, then PVS game states carrying a spawned entity to the client with correct coordinates, then a disconnect propagating both ways. It runs for **both** `lidgren` and `singularity` — the native path is the regression control for the refactor.

Two bugs surfaced only once the real funnel ran, both fixed:

- The vendored test harness registered `MsgMapStrServerHandshake` without `NetMessageAccept.Handshake`, so the real `NetManager`'s pre-handshake message gate (which `IntegrationNetManager` never had) killed the connection on the serializer handshake. Latent harness bug, invisible until a test used the real funnel.
- The Rust transport cleared its disconnect-reason table at the start of every `poll()`, so a reason recorded by an out-of-band disconnect (the engine dropping a channel mid-handler) was wiped before C# could read it. Disconnect reasons now ride inside the `Disconnected` event itself.

Honest caps: Lidgren's `ConnectionApproval` path (`HandleApproval`, auto-approve) stays Lidgren-only — the singularity transport accepts connections in Rust and `IServerNetManager.HandleApprovalCallback` is not consulted (nothing in the engine sets it); UPnP port forwarding warns and skips on the singularity backend; IPv6 dual-stack sockets are not wired (`net.dual_stack` binds per-family, v6 sockets are v6-only like Lidgren's non-dual-stack default); `NetChannel.Connection` (raw `NetConnection`) is gone from the public surface — nothing in the engine used it.

### MTU expansion and the real-content soak (round 5)

- `protocol.rs` grew the initiator half of the fork's MTU discovery (`NetConnection.MTU.cs` port-matched): the first probe goes out on the first tick after connect, sizing climbs ×1.25 while nothing has failed, falls back to binary search between `smallest_failed` and `largest_successful` once a probe times out, finalizes after `expand_mtu_fail_attempts` consecutive timeouts (or when the search converges), and the probe payload rides a standalone `ExpandMTURequest` datagram — the same `[kind 140][seq 0][payload bits:16]` frame the responder path already answered. The protocol cap is the fork's `c_protocolMaxMTU` = **8190** (`(ushort.MaxValue / 8) - 1`, one less than `NetConstants.MaximumFragmentChunkSize`), because a Lidgren responder silently ignores probe payloads above its own cap — the wire-compat test caught that one-byte difference immediately. MTU is now per-connection state (`current_mtu` drives `best_chunk_size` and the `FrameOut` coalescing limit), while `net.mtu` stays the floor; a runtime `set_mtu` still mirrors the floor into connections that haven't discovered anything, but a connection that probed keeps its measured value like Lidgren does.
- The cvars flow through the same `NetTransportSettings` both backends share: `net.mtu_expand`, `net.mtu_expand_frequency` (seconds, default 2) and `net.mtu_expand_fail_attempts` (default 5) are subscribed in `NetManager`, so runtime changes re-apply to both peers. The Lidgren peer sets `AutoExpandMTU` only while the peer isn't running (the fork locks that setter once started) and clamps the frequency/attempts the same way the C# setters demand; the singularity peer forwards everything through the new `sg_transport_set_mtu_expand` FFI call, and `CurrentMtu` on both funnel adapters is now the per-connection value (`sg_transport_connection_mtu`), matching `NetConnection.CurrentMTU` semantics. FFI moved to ABI 4.
- `SgTransportWireCompatTest.MtuExpansionInteropsWithNetServer` puts a Rust endpoint against a real Lidgren `NetServer` with `AutoExpandMTU = true`: both sides climb the ladder independently (each side probes, each side echoes) and both converge to 8190 — the probe/echo bytes are compatible in both directions, not just the framing. `NetManagerRealPairTest.MtuExpandRaisesChannelMtuOnBothEnds` does the same through the full engine funnel on both backends.
- `NetManagerSoakTest` is the real-content soak the roadmap promised: a real client/server pair per backend, guest login, a map, a joined player, 128 spawned entities (in batches, after the login settles) and continuous per-tick coordinate churn driving PVS game states at full tilt, with `net.mtu_expand` on. The per-backend case gates stability (connection held, every entity present client-side, tracked coordinates equal, MTU at 8190 on both ends, RTT bounded) and the comparison case runs lidgren then singularity on identical content and gates `singularity_recv_bytes_per_sec > 0.75 × lidgren_recv_bytes_per_sec` — the standing rule that a slower singularity path does not ship. `SG_SOAK_SECONDS` (default 8, 0 skips) and `SG_SOAK_EXPAND` tune the run for CI.

### Pacing (round 6)

Roadmap item 2, closed. The per-pump flush sent one datagram per small message because the engine produces roughly one game state per tick, so the soak's unhinged tick rate (~2,860 server ticks/s in the window) put 2,858 pkt/s on the wire where Lidgren's heartbeat-batched send buffer put 660. The fix is a per-connection tail in `UdpTransport`: each pump can leave its last partial datagram held (`Connection.tail`), the next pump adopts it into the fresh `FrameOut` so new frames append to the held bytes, and the tail is released on the wire when it fills to MTU, when its deadline (`net.pacing` ms after the first byte, never re-anchored while adopted) passes, or when holding is not paying. The hold only engages while the measured pump cadence is at least twice per window (`pacing >= 2 × poll_ema`, an EMA of `poll()` intervals) — at standard tick rates the engine pumps once per tick, the rule fails, and every pump still flushes, so the hold adds zero latency where it cannot pack. Standalone frames (handshake, ping, probes, bye) are never held — `FrameOut` tracks whether the last datagram is a standalone — and `disconnect_with` pushes the tail out before the bye.

The default is 2 ms after measuring the cliff: the engine ack-gates its game state sends (`MsgStateAck` rides the client's tail), so the tail's window is also the floor on state round trips — at 10 ms the soak failed its own throughput gate. CI-shaped clean-build numbers (loopback, 8 s window, identical content):

| backend | client received | packets | server ticks in window |
| --- | --- | --- | --- |
| lidgren | 1,127 KB/s | 681 pkt/s | 23,060 |
| singularity (unpaced) | 1,296 KB/s | 2,858 pkt/s | 22,850 |
| singularity (2 ms) | **1,222 KB/s** | **230 pkt/s** | **24,969** |

2 ms packs ~12 states per datagram (2,858 → 230 pkt/s, 12.5× fewer datagrams than the per-pump flush) at the same throughput, and stays inert at real tick rates by construction. `net.pacing` = 0 restores the per-pump flush byte for byte; Lidgren ignores the cvar (its own network thread is its pacing).

Two measurement lessons worth keeping: an `eprintln`-per-pump instrumentation build halved the measured throughput (it added a syscall and stderr write to every pump — all early pacing numbers taken with it on disk were garbage), and the login-stall failures it coincided with were real: the soak occasionally (~1 in 8 locally, independent of the pacing window — it reproduces with pacing disabled) fails `client never finished connecting` — the client finishes all three handshakes, the server session never reaches `Connected`. It is on the known-issues list below.

Soak numbers from the round 5 CI-shaped run (loopback, 2 cores, 8 s window, identical deterministic move schedule):

| backend | client received | packets | server ticks in window |
| --- | --- | --- | --- |
| lidgren | 852 KB/s | 660 pkt/s | 17,143 |
| singularity | **1,296 KB/s** | 2,858 pkt/s | 22,850 |

The singularity funnel moved 52% more game-state bytes and completed 33% more server ticks in the same window — the whole pair is faster, not just the wire, because `ProcessPackets` hands a flat event array to the engine instead of recycling Lidgren messages per packet. Lidgren coalesces harder (660 pkt/s vs 2,858 — its ~10 ms heartbeat thread batches several states per datagram where the Rust transport flushes per pump), so it spends fewer bytes on headers per state; it still delivers less content per second. `net.backend` stays `lidgren` by default — the opt-in stays until the singularity path survives real content beyond loopback, but the throughput rule is no longer the blocker.

The soak also flushed out a backend-independent engine race worth recording: spawning entities while the login full-state/ack dance is still in flight can produce a delta state whose tick filter (`component.LastModifiedTick <= fromTick`) strips every component off a brand-new entity, and the client dies with `MissingMetadataException` (the engine's own comment in `ClientGameStateManager` describes the failure). It reproduces on lidgren too — the soak now settles for 45 ticks after `JoinGame` and spawns in batches, which is what real content looks like anyway; the underlying engine race is unchanged and out of scope here.

### The wire-compat test

`Robust.Shared.IntegrationTests/Networking/SgTransportWireCompatTest.cs` puts a Rust transport endpoint against a real, unmodified C# `NetServer` from the vendored fork over loopback UDP:

- the full handshake including `ConnectionApproval` → `Approve()`;
- encrypted payloads both directions — the Rust side encrypts/decrypts with `singularity-crypto`, the C# side with the engine's own `NetEncryption`, so the cross-language frame format is proven, not just asserted;
- reliable-ordered and unreliable exchange;
- an 8 KB message each way — Lidgren fragments it into ~13 datagrams at MTU 700, Rust reassembles (and vice versa), which also exercises the FFI overflow-fetch path on the C# side;
- RTT measured on both sides (`AverageRoundtripTime` on the C# connection, `sg_transport_rtt` on the Rust side);
- MTU discovery both directions against a real `AutoExpandMTU` server, converging on 8190 on both ends;
- disconnect reasons surviving the wire in both directions.

Both tests pass in the CI csharp job alongside the seam's loopback pair test, the funnel pair test and the soak.

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
- Rust tests: 26 unit (header/varuint/fragment-header/ack-packing roundtrips, coalescing, chunk-size search, window and receiver behaviors port-matched to Lidgren, the MTU probe ladder — ×1.25 growth, unexpected-success rejection, timeout → binary search, fail-attempt finalize, protocol-max climb, floor-keeping when disabled, request echo) plus 15 integration (handshake both sides, unreliable echo, reliable-under-loss at 30% both directions in order, 16 KB fragmented roundtrip, disconnect with reason, silence timeout, payload bounds including the 16 MiB group cap, RTT on both ends, 8-client echo, the full FFI surface end-to-end, MTU negotiation to 8190 on both ends over loopback, floor-keeping without expansion, and 16 KB shrinking to a handful of datagrams once expanded).
- One port bug caught against the C# source before it could ship: the draft's unreliable sender advanced its flow-control window *past* an early ack instead of *to* it (Lidgren's `while (windowStart != seqNr)` loop) — off by one slot per early ack. A second draft bug — a fixed `timeout_deadline` captured at connect — would have made runtime `SetTimings` invisible to live connections; timeouts now recompute from last activity every tick.

## Honest caps

- Lidgren's `ConnectionApproval` path (`HandleApproval`, auto-approve) stays Lidgren-only — the singularity transport accepts connections in Rust and `IServerNetManager.HandleApprovalCallback` is not consulted (nothing in the engine sets it); UPnP port forwarding warns and skips on the singularity backend; IPv6 dual-stack sockets are not wired (`net.dual_stack` binds per-family, v6 sockets are v6-only like Lidgren's non-dual-stack default); `NetChannel.Connection` (raw `NetConnection`) is gone from the public surface — nothing in the engine used it.
- MTU discovery is Lidgren-shaped end to end: probing starts on the first tick after connect, and once a connection finalizes (fail-attempts exhausted or search converged) it never re-probes — so a runtime `net.mtu_expand` flip only affects connections that haven't started, which is exactly what the fork's locked `AutoExpandMTU` setter allows. `net.mtu_ipv6` still only feeds the Lidgren path; the singularity floor is `net.mtu` regardless of address family.
- Unconnected messages, discovery, NAT introduction and the ConnectResponse port-change reroute are not implemented; the transport drops what it does not speak.
- `RemoteTime` on the seam is the offset until the channel can see the engine clock.
- Events carry 1200 B inline; larger payloads take the FIFO + `fetch` path, which costs one extra copy on the C# side.
- The transport only pumps when the engine calls `poll()` (once per tick), where Lidgren's network thread drains its socket continuously — under a login-sized burst the kernel receive buffer can drop datagrams the fork would have buffered in its managed ring. The reliable channels recover via retransmit, but MTU probes are expendable library datagrams: with the default 2 s frequency the ladder survives the burst and converges afterwards, and shrinking `net.mtu_expand_frequency` during heavy transfer can stall expansion early.
- Known issue, transport-level, pre-existing (reproduces with pacing disabled and at HEAD-era behavior): a large fragmented reliable message stalls mid-transfer. A 300 KB `ReliableOrdered` send in a scratch pair test froze after ~256 of ~442 chunks: the receiver acked only a handful of single-triple Acknowledge datagrams, the sender's window never refilled, and delivery stopped for the rest of the run. Nothing in the committed test suite exceeds the in-flight window (8/16 KB messages), so the gates stay green — real content that ships big serializer handshakes or large blobs will hit it. Next round's first fix.
- Known issue, flaky (~1 in 8 locally, pacing-independent, all three `net.pacing` values affected): the soak's login occasionally hangs — the client completes the string table, serializer and transfer handshakes, but the server session never logs `Connected`, and the 600-iteration connect loop exhausts in ~19 s. The three login legs are all `ReliableUnordered` (retransmit should recover drops), so the mechanism is not yet identified; it survived a rebuild-from-HEAD control run too rarely to attribute.
- The receiver queues acks for unreliable user frames as well (Lidgren only acks reliable ones); the extra triples are ignored by both peers, but they are wasted bytes on the wire.

## Next steps

1. Fix the large-fragment stall (the scratch reproduction above is the starting point): the sender window/refill path in `channels.rs` (`allowed_sends`, `pump`, `receive_ack`) around multi-window fragment groups is the suspect.
2. Chase the login flake to ground: instrument `collect_connection` event flow on both peers during the connect loop until one hang is captured mid-flight.
3. Content past the bare engine: the pair test and soak run the vanilla engine with no content pack — a real SS14 content server (prototypes, more component variety, heavier states) over the singularity backend is the next proving ground, with `net.backend` still defaulting to `lidgren` until it holds up.
4. The engine race the soak found is upstream material: a delta state built with `fromTick ≥ entity creation tick` for a never-seen entity strips all its components (`PvsSystem.GetStates` filters `LastModifiedTick <= fromTick` even for unseen entities) and kills the client with `MissingMetadataException`; the soak sidesteps it, the engine fix (force full component states for entities the session has never seen) is out of scope for the transport seam.
5. Window tuning is untouched on purpose: the constants are Lidgren's, so behavior matches first and diverges measurably later.
