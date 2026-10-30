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
| Body kinds | Static/Dynamic/Kinematic + custom KinematicController | Static/Dynamic/Kinematic; KinematicController via adapter | same, via adapter |
| Collide start/end events | directed entity events | `CollisionEvent` + `ActiveEvents` | `CollisionStart/End` + `CollisionEventsEnabled` |
| Cancellable pair filter | `PreventCollideEvent`, synchronous | `PhysicsHooks::pair_filter` | `CollisionHooks` via `PhysicsPluginsWithHooks` |
| Sleeping | per-body `SleepingAllowed`, custom client pass | island sleeping | island sleeping |
| TOI / CCD | not implemented (`TODO: SolveTOI`) | native CCDSolver | no, substepping only |
| Joints | Distance/Revolute/Prismatic/Weld/Friction/Mouse + relay | full set + motors | most; Friction/Mouse by hand |
| Grid fixture pattern | hundreds of fixtures on one entity | measured at 512 colliders per body | measured, same |
| Determinism | n/a (C#, both ends) | double-run bit-identical | double-run bit-identical |
| Broadphase | own `B2DynamicTree` port | BVH | obvhs BVH |

The head-to-head performance comparison against the C# Farseer port itself is still pending: it requires a C# harness driving `SharedPhysicsSystem`, which lands together with the C# core onboarding (roadmap item 5). No numbers are claimed for it yet.

## Risks for the hybrid plan

1. `PreventCollideEvent` is a synchronous cancellable filter raised per pair. In a hybrid it crosses the FFI boundary as a reverse call — per-pair callback cost must be benchmarked before content relies on it heavily.
2. Prediction requires identical solver behavior on client and server; the two Rust candidates are not interchangeable mid-stream — picking one locks the wire expectations for both ends.
3. Avian's 25 ms spikes and sleep tuning are operational flags, not blockers; both need profiling on real content, not synthetic scenes.
4. The `PhysicsBackend` trait here is the seam. In the C# core it becomes `IPhysicsBackend` behind `SharedPhysicsSystem.SimulateWorld()`, keeping components, fixtures, YAML serialization and the event bus in C# — exactly the seam Robust Toolbox already uses for native libraries.
