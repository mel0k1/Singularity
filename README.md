<div align="center">

# Singularity

**A hybrid client-server game engine: a C# orchestration core with Rust subsystems.**

[![CI](https://github.com/mel0k1/Singularity/actions/workflows/ci.yml/badge.svg)](https://github.com/mel0k1/Singularity/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-b7410e.svg)](https://www.rust-lang.org)

A reimagining of [Robust Toolbox](https://github.com/space-wizards/RobustToolbox), the engine behind Space Station 14.

[English](README.md) · [Русский](README.ru.md)

</div>

## Why hybrid

Rewriting Robust Toolbox in pure Rust would mean replacing ~367k lines of C#: `Robust.Shared` alone is 1073 files / ~165k lines. The content contract is welded to C#: reflection-based discovery plus four Roslyn source generators (`[DataField]`, `[AutoGenerateComponentState]`, `[SubscribeLocalEvent]`, `[Dependency]`) generate code directly into content assemblies. A full rewrite breaks every content project ever written against the engine.

Singularity splits the engine along the seam Robust Toolbox already uses in production:

- **C# stays the orchestrator** — ECS, IoC, YAML serialization, prototypes, GameStates/PVS orchestration, network messages. This is where the content contract lives, so it stays compatible.
- **Rust becomes the native provider** — Robust Toolbox already ships native code behind C# interfaces (zstd, libsodium, SDL3, OpenAL). Singularity generalizes the pattern: performance-critical subsystems are Rust crates behind thin C# interfaces.
- **One binary on both ends** — client and server share the same Rust artifact, so physics and crypto behave identically on both sides. Determinism first.

## Architecture

```text
C# layer — orchestration and content contract        Rust layer — crates behind C# interfaces
══════════════════════════════════════════            ══════════════════════════════════════════
GameObjects/ECS · IoC · entity event bus             singularity-crypto         XChaCha20-Poly1305 frames
YAML serialization · prototypes                     singularity-physics        Rapier/Avian study
GameStates · PVS orchestration                      singularity-net-transport  Lidgren wire study
network messages · StringTable · discovery          singularity-pvs            SoA per-session data
                        └──────────── FFI contract ──────────┘
```

The FFI contract: C# builds the registries — component types, network ids, serializer hashes — and hands them to Rust as tables. Rust never reflects; the registries always come from the C# host.

## Wire compatibility

`singularity-crypto` mirrors Robust Toolbox `NetEncryption` byte for byte:

- frame = `[u64 LE counter][XChaCha20-Poly1305 ciphertext + tag]`, 24 bytes of overhead
- the 24-byte XChaCha nonce is the counter in its first 8 bytes, zero elsewhere
- counters are pre-incremented by two: the server transmits 2, 4, 6…, the client 3, 5, 7…, so the two directions never collide
- authentication failure is a value, not an exception: `decrypt` returns `None`, like `TryDecrypt`; a corrupted frame cannot crash the peer

## Layout

| Path | Language | Role |
| --- | --- | --- |
| `rust/crates/singularity-crypto` | Rust | XChaCha20-Poly1305 frame codec, wire-compatible with `NetEncryption` |
| `rust/crates/singularity-physics` | Rust | physics backend study: Rapier and Avian behind a shared `PhysicsBackend` seam, benchmarked and parity-checked — see [docs/physics-bench.md](docs/physics-bench.md) |
| `rust/crates/singularity-physics-ffi` | Rust | the C ABI seam: per-map `SgWorld`, command-buffer apply, bulk state/event copy — the native side of `IPhysicsBackend` |
| `rust/crates/singularity-net-transport` · planned | Rust | Lidgren-compatible transport study |
| `rust/crates/singularity-net-transport` | Rust | the transport seam below `INetChannel`: UDP transport, handshake, stop-and-wait reliability, event polling, C ABI — see [docs/net-transport.md](docs/net-transport.md) |
| `rust/crates/singularity-pvs` · planned | Rust | SoA per-session visibility data |
| `csharp/engine` | C# | vendored RobustToolbox @ fdb576c — the starting point of the orchestration core; carries the `IPhysicsBackend` seam behind `SharedPhysicsSystem.SimulateWorld()` |
| `csharp/benches/Singularity.PhysicsBench` | C# | farseer and the rapier mirror measured in-situ on the real ECS — see [docs/physics-bench.md](docs/physics-bench.md) |

## Status

Research phase complete. The Robust Toolbox sources were studied subsystem by subsystem — ECS, IoC, serialization, prototypes, GameStates/PVS, network, physics, native interop. Three spikes are implemented, tested and benchmarked: `singularity-crypto`, `singularity-physics` and the in-engine mirror — `IPhysicsBackend` with a Rapier backend behind `SimulateWorld()`, switchable by the `physics.backend` cvar, default off. Round 2 closed the whole verdict list: distance/revolute/prismatic/weld joints with breaks and `CollideConnected`, `PreventCollideEvent` as a cached reverse FFI call through the engine's own `ShouldCollideSlow`, incremental body collection (bit-identical to the full scan in every scene, 17x less orchestration on fall-heavy scenes), and engine sleep cvars mapped onto Rapier thresholds. Same scenes, same ECS: 2.3x p50 on the contact-heavy floor scene, the KinematicController cargo gap fixed outright, 2.5x more bodies asleep than the Farseer path at rest, bit-identical double-run determinism — see [docs/physics-bench.md](docs/physics-bench.md).

## Roadmap

1. `singularity-crypto` — done
2. `singularity-physics` — done and in: `IPhysicsBackend` behind `SimulateWorld()`, rapier mirror measured in-situ (docs/physics-bench.md) — joints, pair filter, incremental collection and sleep tuning landed; prediction parity via the shared binary remains
3. `singularity-net-transport` — seam spike done (docs/net-transport.md): UDP transport, event model, FFI and stop-and-wait reliability under measured loss; the C# seam is in too — `INetChannel` over `sg_transport_*` behind `net.backend` (default `lidgren`), proven by a loopback pair test in CI; the Lidgren framer port is the next milestone
4. `singularity-pvs` — SoA session data; the pinned `PvsData` structs are already unmanaged
5. C# core onboarding — done: engine vendored (`csharp/engine`), Farseer measured in-situ (`csharp/benches`), determinism confirmed

## Building

```sh
cd rust
cargo build --release
cargo test --all
cargo run --release -p singularity-physics --bin bench
cargo run --release -p singularity-net-transport --bin bench
```

```sh
cd csharp
dotnet run --project benches/Singularity.PhysicsBench -c Release -- --scene=all --validate
dotnet run --project benches/Singularity.PhysicsBench -c Release -- --scene=all --validate --strict --backend=rapier
dotnet run --project benches/Singularity.PhysicsBench -c Release -- --scene=all --verify-sync
```

## Code style

The codebase is intentionally comment-free: names and structure carry all the meaning. CI rejects comments in sources. Commits are short.

## License

Apache-2.0 — see [LICENSE](LICENSE). Singularity is an independent project inspired by [Robust Toolbox](https://github.com/space-wizards/RobustToolbox) (MIT) by [space-wizards](https://github.com/space-wizards) and by [Bevy](https://bevyengine.org) (MIT or Apache-2.0).
