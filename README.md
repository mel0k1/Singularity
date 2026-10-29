<div align="center">

# Singularity

**A client-server game engine for multiplayer sandbox games, written in Rust on top of Bevy.**

[![CI](https://github.com/mel0k1/Singularity/actions/workflows/ci.yml/badge.svg)](https://github.com/mel0k1/Singularity/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-b7410e.svg)](https://www.rust-lang.org)

A Rust reimagining of [Robust Toolbox](https://github.com/space-wizards/RobustToolbox), the engine behind Space Station 14.

[English](README.md) · [Русский](README.ru.md)

</div>

## Why

Robust Toolbox spent a decade proving an unusual thesis: a fully authoritative server, delta-snapshot world replication with per-client visibility, client-side prediction and data-driven YAML content can carry a large multiplayer sandbox. Singularity keeps those ideas and rebuilds them in Rust.

- **Authoritative server** — the client never decides anything gameplay-critical on its own
- **Snapshot replication** — the world ships as delta snapshots with per-client potentially visible sets, not RPC soup
- **Prediction and reconciliation** — input feels instant even at high latency
- **Data-driven content** — entities, prototypes and maps are plain YAML
- **Bevy inside** — ECS, scheduling, rendering, audio and input come from Bevy where they fit

## Crates

| Crate | Role | Replaces (Robust Toolbox) |
| --- | --- | --- |
| `singularity-core` | shared vocabulary: ticks, prototypes, serialization, event bus | `Robust.Shared` |
| `singularity-server` | authoritative simulation, PVS snapshots, player sessions | `Robust.Server` |
| `singularity-client` | rendering, input, UI, audio, prediction | `Robust.Client` |

## Status

Research phase. The Robust Toolbox source, roughly 367k lines of C#, has been studied subsystem by subsystem and mapped onto Rust and Bevy primitives. No playable code yet.

## Roadmap

1. Core vocabulary on Bevy ECS: ticks, network entity ids, prototypes, YAML serialization
2. Server: fixed tick loop, snapshot protocol, per-client visibility
3. Client: prediction, reconciliation, interpolation
4. Rendering and input on top of `bevy_render`
5. UI, audio, maps, tooling

## Building

```sh
cargo build --release
cargo test --all
```

## Code style

The codebase is intentionally comment-free: names and structure carry all the meaning.

## License

Apache-2.0 — see [LICENSE](LICENSE). Singularity is an independent project inspired by [Robust Toolbox](https://github.com/space-wizards/RobustToolbox) by [space-wizards](https://github.com/space-wizards) and by [Bevy](https://bevyengine.org).
