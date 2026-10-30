# Physics backend study

Phase 2 of the roadmap: benchmark Rapier and Avian against the Farseer port that Robust Toolbox ships, behind a neutral `PhysicsBackend` trait, before any swap decision.

## Method

- Fixed 60 Hz timestep, 1 unit = 1 meter, g = 9.81 down, release build, 600 ticks per scene.
- One benchmark process drives both engines through the identical `PhysicsBackend` contract: `add_body`, `remove_body`, `set_body_velocity`, `snapshot`, `step`.
- Avian runs inside a real Bevy `App` (`TaskPoolPlugin` + `PhysicsPlugins` in `PostUpdate`) with its clock driven manually: the generic `Time` resource is advanced by 1/60 s before each `app.update()`. Rapier is stepped headlessly via `PhysicsWorld::step_with_events` with an `EventHandler` collector.
- Collision events are opt-in per collider on both engines: Rapier needs `ActiveEvents::COLLISION_EVENTS`, Avian needs `CollisionEventsEnabled`.
- Every run is verified twice for bit-identical final state (SipHash over all body snapshots) plus scenario sanity: no tunneling, kinematic platform must carry its cargo by friction.
- Scenes follow Robust Toolbox usage patterns:
  - `floor` — one static body carrying 512 tile colliders (the `grid_chunk-*` fixture pattern), a 12-row box pyramid and 60 scattered discs, all enclosed by walls.
  - `rain` — a 300-body spawn stream over 300 ticks, then 300 ticks of settling.
  - `mover` — a kinematic velocity-based platform at 1.5 m/s with 32 crates that must be carried by friction.

Hardware: 2-core Intel Xeon, release build, single process. Reproduce with `cargo run --release -p singularity-physics --bin bench`; `--validate` runs the determinism and sanity gates; `--json` emits machine-readable lines.

## Results

| scene | backend | bodies | asleep | contact starts | p50 µs | p95 µs | max µs | mean µs |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| floor | rapier | 141 | 125 | 667 | 88 | 121 | 890 | 92 |
| rain | rapier | 303 | 1 | 2573 | 165 | 243 | 307 | 158 |
| mover | rapier | 33 | 0 | 181 | 48 | 54 | 174 | 48 |
| floor | avian | 141 | 28 | 683 | 419 | 473 | 25045 | 528 |
| rain | avian | 303 | 1 | 2607 | 604 | 794 | 22309 | 595 |
| mover | avian | 33 | 0 | 183 | 238 | 291 | 26115 | 334 |

Findings:

1. Rapier leads by 3.7–5.0x on p50 in every scene. Part of the gap is Bevy orchestration tax that is inseparable from how Avian ships: on `mover`, 33 bodies still cost Avian 238 µs per tick against Rapier's 48 µs. Headless stepping is simply cheaper.
2. Avian shows rare 22–26 ms spikes (max column) — above a 16.6 ms budget for a 60 Hz server tick. Suspect is collider-tree optimization; must be profiled before any server-side use.
3. Sleep behavior diverges sharply on identical scenes (floor at t = 10 s: Rapier 125/138 asleep, Avian 28/138). Sleep tuning is a per-backend obligation, not a property — for SS14-style servers it directly caps the idle tick cost.
4. Both engines passed the double-run determinism gate bit-for-bit, which keeps the one-binary-both-ends prediction plan viable.
5. Both engines carried the kinematic platform cargo by friction, the semantics Space Station 14 grids rely on.

## Parity matrix vs the Robust Toolbox Farseer port

Semantics from the Robust Toolbox study (26.3k LOC, Farseer 3.x / Box2D 2.1 lineage, no TOI/CCD):

| Capability | Robust Toolbox (Farseer port) | Rapier 0.36 | Avian 0.8 |
| --- | --- | --- | --- |
| Body kinds | Static/Dynamic/Kinematic + KinematicController (measured: gives Dynamics no contact support — see head-to-head) | Static/Dynamic/Kinematic; kinematic_velocity_based carries cargo natively | same, via adapter |
| Collide start/end events | directed entity events | `CollisionEvent` + `ActiveEvents` | `CollisionStart/End` + `CollisionEventsEnabled` |
| Cancellable pair filter | `PreventCollideEvent`, synchronous | `PhysicsHooks::pair_filter` | `CollisionHooks` via `PhysicsPluginsWithHooks` |
| Sleeping | per-body `SleepingAllowed`, custom client pass | island sleeping | island sleeping |
| TOI / CCD | not implemented (`TODO: SolveTOI`) | native CCDSolver | no, substepping only |
| Joints | Distance/Revolute/Prismatic/Weld/Friction/Mouse + relay | full set + motors | most; Friction/Mouse by hand |
| Grid fixture pattern | hundreds of fixtures on one entity | measured at 512 colliders per body | measured, same |
| Determinism | n/a (C#, both ends) | double-run bit-identical | double-run bit-identical |
| Broadphase | own `B2DynamicTree` port | BVH | obvhs BVH |

The head-to-head against the C# Farseer port is done — see [Head-to-head: the Farseer port, in-situ](#head-to-head-the-farseer-port-in-situ) below.

## Risks for the hybrid plan

1. `PreventCollideEvent` is a synchronous cancellable filter raised per pair. In a hybrid it crosses the FFI boundary as a reverse call — per-pair callback cost must be benchmarked before content relies on it heavily.
2. Prediction requires identical solver behavior on client and server; the two Rust candidates are not interchangeable mid-stream — picking one locks the wire expectations for both ends.
3. Avian's 25 ms spikes and sleep tuning are operational flags, not blockers; both need profiling on real content, not synthetic scenes.
4. The `PhysicsBackend` trait here is the seam. In the C# core it becomes `IPhysicsBackend` behind `SharedPhysicsSystem.SimulateWorld()`, keeping components, fixtures, YAML serialization and the event bus in C# — exactly the seam Robust Toolbox already uses for native libraries.

## Head-to-head: the Farseer port, in-situ

The C# core is on board: `csharp/engine` vendors RobustToolbox @ fdb576c, and `csharp/benches/Singularity.PhysicsBench` drives the real ECS through `RobustServerSimulation` — the same headless, thread-free host the engine uses for its own benchmarks. Scenes are line-for-line ports of `scenes.rs`: same xorshift RNG, same seeds (1011/2022/3033), same collider sizes, frictions and density 1, free rotation, gravity 9.81 down, one 1/60 s step per tick (`physics.target_minimum_tickrate=30` disables substepping to match Rust stepping).

Method difference vs the table above: Rust timed `backend.step()`; the harness times the whole server tick (`IEntityManager.TickUpdate`), every system included. The `idle` scene measures exactly that overhead — p50 8 µs for an empty world — so at p50 the comparison is physics against physics.

| scene | backend | bodies | asleep | contact starts | p50 µs | p95 µs | max µs | mean µs |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| idle | farseer | 3 | 0 | 0 | 8 | 13 | 29728 | 59 |
| floor | farseer | 141 | 38 | 824 | 2269 | 11579 | 106557 | 4264 |
| rain | farseer | 303 | 0 | 3207 | 1796 | 3632 | 148620 | 2527 |
| mover | farseer | 33 | 0 | 319 | 128* | 159 | 86554 | 277 |

\* mover: the cargo fell through (finding 6), so the solver did less work than the scene intends — the row is not comparable.

```json
{"scene":"idle","backend":"farseer","ticks":600,"bodies":3,"dynamics":0,"asleep":0,"contact_starts":0,"p50_us":8.1,"p95_us":13.3,"max_us":29728.5,"mean_us":59.057,"min_y":0,"cargo_dx":0,"state_hash":"afc5b4ba6752ed33"}
{"scene":"floor","backend":"farseer","ticks":600,"bodies":141,"dynamics":138,"asleep":38,"contact_starts":824,"p50_us":2269,"p95_us":11579.2,"max_us":106557.2,"mean_us":4263.984,"min_y":0.3546837,"cargo_dx":0,"state_hash":"07b41510e9264928"}
{"scene":"rain","backend":"farseer","ticks":600,"bodies":303,"dynamics":300,"asleep":0,"contact_starts":3207,"p50_us":1795.5,"p95_us":3632.5,"max_us":148620.2,"mean_us":2527.483,"min_y":0.4017237,"cargo_dx":0,"state_hash":"85109f61bb8d6813"}
{"scene":"mover","backend":"farseer","ticks":600,"bodies":33,"dynamics":32,"asleep":0,"contact_starts":319,"p50_us":128.1,"p95_us":159.4,"max_us":86554.1,"mean_us":277.206,"min_y":-280.41022,"cargo_dx":3.306195e-07,"state_hash":"f7167614a0443645"}
```

Findings:

1. p50 vs Rapier: floor 25.8x (2269/88), rain 10.9x (1796/165), mover 2.7x. vs Avian: floor 5.4x, rain 3.0x. The Farseer port is the slowest backend in every scene, an order of magnitude behind on the contact-heavy floor scene.
2. C# orchestration is the cheapest part of the tick, not the problem. An idle full server tick costs 8 µs p50; floor physics adds ~2.26 ms on top. 99.6% of a floor tick is physics. The hybrid thesis in numbers: keep the ECS in C#, move the solver out.
3. Long tail: floor p95 11.6 ms against a 16.6 ms budget, and 87–149 ms max spikes across scenes. Per-tick allocations in the engine path need profiling before any server use; ServerGarbageCollection was on.
4. Sleep divergence: floor 38/138 asleep after 10 s, where Rapier has 125/138 (Avian 28/138). Farseer defaults (`physics.timetosleep` 0.2 s, `physics.linsleeptol` 0.01) keep the scene hot — the real driver of idle cost for SS14-style servers.
5. Determinism: double-run state hashes (FNV-1a over per-body translation, rotation, velocity and sleep bits) are bit-identical in all four scenes. Same prediction-plan viability as the Rust backends.
6. Parity gap, mover: `BodyType.KinematicController` gives Dynamic bodies no contact support. Crates fall through the platform from tick one — min_y -280 m after 10 s, cargo dx ~0, and the sanity gate fails. `GetInvMass` hands the Dynamic side invMass 0 in both orderings, so the solver can only move the platform. Rust's `kinematic_velocity_based` carries cargo by friction natively. What SS14 actually ships: moving grids are Dynamic under thrust; KinematicController is a transient docking state with zero test coverage in the engine tree. Verdict: a velocity-driven carrier is not expressible in the Farseer port without solver changes.

Reproduce (.NET 10, no content, vendored engine at `csharp/engine`):

```sh
cd csharp
dotnet run --project benches/Singularity.PhysicsBench -c Release -- --scene=all --ticks=600 --validate --json
```

Hardware: same class as the Rust bench — 2-core Intel Xeon, Release, single process, single run.
