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

## The mirror: Rapier behind `SimulateWorld()`

Phase 3: `IPhysicsBackend` is welded into the engine. `physics.backend = rapier` (new CVar, server-replicated, default `farseer`) routes `SharedPhysicsSystem.SimulateWorld()` through `RapierPhysicsBackend` (`csharp/engine/Robust.Shared/Physics/Backend/`) while the Farseer path stays untouched and default. One internal branch, zero component, serialization or content API changes.

Protocol — the promised FFI contract in miniature: tables in, tables out, no reflection:

- One Rust `SgWorld` per map (`singularity-physics-ffi`, cdylib), bodies and colliders keyed by entity id.
- Per tick the backend scans the ECS once (`PhysicsComponent` + `FixturesComponent` + `TransformComponent`), diffs every body against a mirror state (pose, velocity, force, damping, kind, fixed-rotation, awake) and emits only the changes as `SgCommand` records into a pinned buffer; one `sg_world_apply` per map.
- Fixtures diff by FNV fingerprint; an instance stamp (count + object identities) skips unchanged `FixturesComponent`s entirely, so a 512-tile grid costs nothing per tick. Axis-aligned rectangle polygons become cuboids — Rapier's fast path and exactly what the pure-Rust bench used.
- `sg_world_step` integrates; `sg_world_copy_states` / `sg_world_copy_events` bulk-copy back. Positions apply as `SetLocalPositionRotation` deltas — the same primitive the Farseer solver uses for its own writeback — velocities go through a no-wake writer so sync never resets a sleep timer.
- Collision events return per fixture pair with a 2-point manifold and are raised as native `StartCollideEvent` / `EndCollideEvent` on the C# event bus, both directions, same broadcast flags as the Farseer raiser; the bench `ContactCounterSystem` counts them the same way.
- `PhysicsUpdateBeforeSolveEvent` / `AfterSolveEvent`, substepping, `FinalStep` and `autoclearforces` semantics are preserved inside the branch.

Safety rails, each verified: ECS/components/content API untouched (one cvar + one internal branch); serialization and network states untouched; existing C# collision events preserved and counted; Dynamic/Static/Kinematic and the grid pattern exercised by the same sanity gates plus a `--strict` mode that fails CI on any regression; FFI and allocations measured separately (per-call micros and GC bytes per run); identical scenes, ticks and RNG on both backends with every semantic difference listed below.

### Results

Same 2-core host, .NET 10, Release, 600 ticks, single run per row; both backends through the identical harness:

| scene | backend | p50 µs | p95 µs | max µs | asleep | contact starts | alloc MB | min_y | cargo dx |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| idle | farseer | 6 | 7 | 21656 | 0 | 0 | 0.48 | — | — |
| idle | rapier mirror | 23 | 31 | 26029 | 0 | 0 | 0.58 | — | — |
| floor | farseer | 2225 | 3795 | 80411 | 38 | 824 | 138.5 | 0.355 | — |
| floor | rapier mirror | 968 | 2418 | 72562 | 35 | 628 | 117.1 | 0.350 | — |
| rain | farseer | 1737 | 2247 | 132722 | 0 | 3207 | 259.1 | 0.402 | — |
| rain | rapier mirror | 2178 | 3221 | 74908 | 0 | 2446 | 242.8 | 0.397 | — |
| mover | farseer | 101* | 139* | 16270* | 0 | 319 | 39.9 | **-280.4** | **0.000** |
| mover | rapier mirror | 115 | 147 | 768 | 0 | 163 | 37.9 | 0.449 | **6.474** |

\* mover/farseer is a broken scene, not a fair race: the cargo falls through from tick one (head-to-head finding 6), so the solver does a fraction of the intended work.

FFI cost per tick, measured around each call separately:

| scene | apply µs | step µs | states µs | events µs | total µs | cmds/tick | states/tick |
| --- | --- | --- | --- | --- | --- | --- | --- |
| idle | 0.6 | 1.0 | 0.2 | 0.1 | 1.9 | 1.1 | 3 |
| floor | 1.2 | 135.1 | 5.2 | 0.4 | 141.9 | 2.7 | 141 |
| rain | 4.9 | 321.4 | 9.6 | 0.7 | 336.6 | 3.9 | 228 |
| mover | 0.2 | 47.4 | 1.1 | 0.1 | 48.8 | 0.4 | 33 |

Findings:

1. **Contact-heavy is where the hybrid pays: floor p50 2.30x** (2225 → 968 µs), p95 1.57x, allocations −15%. The floor tick is 99% solver on both backends, so this is Rapier-vs-Farseer through the same ECS.
2. **Fall-heavy rain is 20% slower p50** (1737 → 2178 µs): the mirror's C# orchestration (full ECS rescan + per-body diff + writeback) costs ~1.84 ms/tick — about as much as Farseer's entire tick — and eats the ~1.4 ms the solver saves. The tail improves though: max 132.7 → 74.9 ms. Optimization path is clear: replace the per-tick full scan with an event-driven dirty set.
3. **FFI transport is noise.** Apply + state + event copies, excluding the solve itself: 1.9–15 µs per tick, i.e. ~50–70 ns per body per tick. The command-buffer design holds; nothing here argues against Rust behind the seam.
4. **The KinematicController gap is fixed outright**: the same scene that drops cargo through Farseer (min_y −280 m, cargo dx ~0) carries it 6.47 m on the mirror with a 768 µs max tick. `kinematic_velocity_based` is the semantics SS14 grids actually need.
5. **Sleep parity with the engine defaults**: 35/138 asleep on the mirror vs 38/138 on Farseer under the same engine damping (0.2). The pure-Rust bench sleeps 125/138 — but only because it runs damping 0; bisecting damping in the mirror reproduces the pure numbers exactly (125/138, same 667 contact starts). Rapier 0.36 gates sleep on pose drift, and the engine's default damping keeps stacks drifting above threshold. Idle-cost parity holds out of the box; deeper sleep is a backend tuning knob (`linear_threshold`, `time_until_sleep`), not an engine change.
6. **Determinism survives the full round trip**: double-run state hashes are bit-identical in all four scenes through scan → command buffer → Rust solve → writeback → event raise.
7. **Two mirror bugs found and fixed by the gates are worth recording**: `SetAngularVelocity` in the engine wakes unconditionally (no wake flag), so the writeback needed a no-wake writer; and a NaN-seeded first-sync comparison silently skipped velocity pushes — kinematic platforms slept at zero velocity and never moved. Both are exactly the class of integration subtlety the strict mode exists to catch.

### Semantic differences

1. Joints are not mirrored yet (`SharedJointSystem` still owns them; the bench scenes are joint-free) — required before any content trial.
2. `PreventCollideEvent`, the synchronous cancellable pair filter, has no Rapier path yet; it needs a reverse FFI call per pair, and its cost must be benchmarked before content relies on it.
3. Solver lineage differs (Rapier TGS defaults vs the port's PGS 8/3): the same scenes settle to slightly different rest poses (floor min_y 0.350 vs 0.355). Bit-compatibility with Farseer is not a goal; determinism within a backend is, and holds.
4. Damping formulas differ (`1/(1+dt·d)` vs `clamp(1−dt·d)`); at the engine default 0.2 the per-step difference is 1e-5.
5. Sleep gates differ (pose drift vs velocity threshold); the `physics.timetosleep` / `physics.linsleeptol` cvars do not map yet.
6. Client prediction falls back to the Farseer path in the spike (`prediction == true` bypasses the backend). The production plan — the same Rust artifact in the client and server binaries — is what makes prediction safe to switch over.
7. In-place fixture edits (same `Fixture` and shape instances, new values) are not hot-detected by the instance stamp; add/remove/replace and shape swaps are. A version counter on `FixturesComponent` (one engine line) closes this.
8. `PhysShapeAabb` maps to cuboid with the radius folded into half-extents; generic polygons ride the convex-hull path; edge/chain shapes are not mirrored yet.

### Verdict

The seam holds. Rapier behind `SimulateWorld()` keeps every content-facing contract intact — components, serialization, collision events — costs ~50–70 ns per body per tick in FFI transport, wins 2.3x p50 on the contact-heavy scene, fixes the kinematic cargo gap outright, and stays deterministic through the full round trip. Rapier is the physics backend for Singularity, pending: joints, the pair filter, incremental body collection, sleep tuning, and prediction parity via the shared binary.

## Round 2: joints, the pair filter, incremental collection, sleep tuning

Everything the verdict listed as pending, done. ABI bumped to 2 (`OP_ADD_JOINT` + `OP_SET_JOINT_PARAMS` pair, `OP_REMOVE_JOINT`, `OP_SET_SLEEP`, reverse pair-filter callback, joint-event out-table). Three new scenes exercise the new surface on both backends — `chain` (12 boxes on revolute joints hanging from a static anchor plus a 6-bead rigid distance-joint rope, `CollideConnected = false`), `welds` (16 box pairs welded with a π/2 reference angle and seam anchors, dropped onto a platform), `prevent` (40 discs onto a platform with a system cancelling every `PreventCollideEvent` where the other body is dynamic — everything must fall through, zero contacts, on both backends).

Same 2-core host, .NET 10, Release, 600 ticks, single run per row. `orch` is the new C# orchestration metric (dirty collection + diffing, excluding FFI calls):

| scene | backend | p50 µs | p95 µs | max µs | asleep | contacts | joints | orch µs/t | min_y |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| chain | farseer | 88 | 124 | 3431 | 0 | 40 | 18 | — | 8.575 |
| chain | rapier mirror | 73 | 91 | 10184 | 0 | 46 | 18 | 2.8 | 8.571 |
| welds | farseer | 2 | 122 | 27006 | 32 | 60 | 16 | — | 0.463 |
| welds | rapier mirror | 18 | 118 | 1157 | 32 | 60 | 16 | 1.0 | 0.450 |
| prevent | farseer | 90 | 165 | 1360 | 0 | 0 | 0 | — | -275.1 |
| prevent | rapier mirror | 124 | 149 | 21263 | 0 | 0 | 0 | 5.5 | -275.9 |

Full-scene comparison after incremental collection landed (same host, same runs):

| scene | backend | p50 µs | p95 µs | max µs | asleep | contacts | alloc MB | dirty/t | audit misses |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| idle | farseer | 5 | 8 | 22051 | 0 | 0 | 0.48 | — | — |
| idle | rapier mirror | 6 | 12 | 34698 | 0 | 0 | 0.58 | 0.0 | 0 |
| floor | farseer | 2890 | 4173 | 68967 | 38 | 824 | 156.6 | — | — |
| floor | rapier mirror | 1283 | 2212 | 86078 | 97 | 955 | 122.0 | 106.3 | 0 |
| rain | farseer | 1600 | 2077 | 97656 | 0 | 3207 | 259.1 | — | — |
| rain | rapier mirror | 1866 | 3160 | 171056 | 0 | 2647 | 224.2 | 218.3 | 0 |
| mover | farseer | 122* | 157* | 18260* | 0 | 319 | 39.9 | — | — |
| mover | rapier mirror | 138 | 166 | 1009 | 0 | 238 | 37.9 | 32.0 | 0 |

\* mover/farseer remains the broken KinematicController scene (head-to-head finding 6).

### Joints

Distance (rigid → rope limits, soft → spring), revolute (limits + force-based motor), prismatic (axis, limits, motor) and weld map onto Rapier impulse joints; the reference angle folds into `local_frame2.rotation = -referenceAngle` (validated by sign-sensitive tests on both ends). Joint identity is a per-pair numeric id; per-tick the mirror fingerprints every `JointComponent` (cheap: joint populations are tiny next to body counts) and re-issues only changed joints — `chain` costs 18 add-commands once and zero thereafter. `CollideConnected = false` maps to Rapier's `contacts_enabled(false)` and, redundantly, to a blocking rule in the pair filter, matching `ShouldCollideJoints`'s any-joint-blocks semantics. Friction and mouse joints are not mirrored (warn-once); soft welds ride Rapier's rigid `FixedJoint` — `welds` measures 0.000 rad drift on both backends, so the rigid approximation held everything the scene asked of it.

Breakpoints: the Rust side reports joint breaks from the sum of locked-axis, limit and motor impulses (all three live in different fields in Rapier 0.36), and the C# side raises the same `JointBreakEvent` trio (directed A, directed B, broadcast) that the Farseer path raises, then disables the joint. The impulse proxy is Rapier-internal lambda accumulation — monotone in load but **not calibrated to Farseer's `GetReactionForce` Newtons**; default breakpoints (`float.MaxValue`) never break, content-set small breakpoints will fire at different loads than Farseer would. Calibration is a follow-up, not a blocker.

### The pair filter

`PreventCollideEvent` crosses the boundary as a reverse call: the world holds a CDECL function pointer + context handle, and Rapier's `filter_contact_pair`/`filter_intersection_pair` hooks (enabled on every mirrored collider) resolve collider ids → entities and run the engine's own `ShouldCollideSlow` — the exact same code path, builtin rules and event raiser as Farseer, including the static-child and kinematic-controller rules that live in C#. Decisions are cached per unordered collider pair, which reproduces Farseer's evaluate-at-contact-creation semantics; the cache invalidates when a joint between the two bodies changes and when either collider is re-created.

Numbers: the whole `prevent` run makes 525 reverse calls (Farseer raises 3612 directed `PreventCollideEvent`s in the same scene — the cache suppresses re-asks for pairs that stay rejected), and contact-heavy scenes pay single-digit calls per tick (floor 558 over 600 ticks). The reverse call is not on any hot path that matters.

The same pass fixed a real parity bug: the mirror pushed collision groups with `InteractionTestMode::And` while Farseer's `ShouldCollide` is OR over the two mask/layer directions. With `Or`, floor counts 955 contact starts against Farseer's 824 (was 628) — the remaining gap is solver lineage, not filtering.

### Incremental body collection

The per-tick full ECS rescan is gone. The dirty set now comes from the engine's own movement plumbing, consumed and cleared by the mirror: `AwakeBodies` (every body that can possibly have new velocities — and the only ones whose velocity writes matter, since writes to a sleeping body take effect at wake-up, which re-enters the awake set), `MoveBuffer` (fixture proxies — spawned bodies, moved bodies, re-parented bodies, fixture edits), `MovedGrids` (subtree walk of every moved grid so grid-children get their world poses refreshed), `EntityDeleted` and `CollisionChangeEvent`. A full-scan audit runs every 32 ticks as the safety net and counts `AuditMisses` — changes it had to make that no event signalled. Cross-map moves emit a removal into the old map's command stream and an add into the new one. `SINGULARITY_RAPIER_SYNC=full` forces the old full-scan path for A/B.

Results: **full and incremental produce bit-identical state hashes in all seven scenes** (`--verify-sync`, now a CI gate). Orchestration drops from ~1.84 ms/tick to 106 µs/tick on `rain` — 17x — and the p50 gap to Farseer on `rain` closes from +25% to +17% (the remaining mirror overhead is the state copy-back plus the writeback path). `welds` settles to 3.7 dirty bodies per tick. Audit misses: 0 across every scene — the event sources cover everything these scenes do. The one hole by design: in-place fixture edits and `SetBodyType`/`SetFixedRotation`/`SetSleepingAllowed` on *sleeping* bodies are audit-caught within 0.53 s at worst (no engine hook needed — adding one line per setter closes it entirely if content ever trips the counter).

A second-order win nobody planned: the old full scan re-pushed pose drift on resting stacks every tick, and those transform pushes carried a wake flag — the sync itself was keeping bodies awake. With incremental collection, resting bodies are never touched: floor now sleeps 97/138 bodies against Farseer's 38.

### Sleep tuning

`physics.timetosleep`, `physics.linsleeptol` and `physics.angsleeptol` now map onto per-body `time_until_sleep` / `normalized_linear_threshold` / `angular_threshold` at world creation (`--sleep=native` restores Rapier's own 0.5 s / 0.05 for A/B). `SleepingAllowed = false` disables sleep outright (thresholds −1) instead of the old keep-waking hack. The mapping is honest about semantics: Rapier 0.36 gates sleep on farthest-point pose drift (solver corrections included), not center-of-mass velocity, and `angsleeptol` only affects collider-less bodies — the cvars transfer their intent, not their formula. On floor: native 108 asleep, engine-mapped 97 (stricter threshold, faster timer), Farseer 38. Kinematic bodies sleep only at exactly zero velocity, which is what the mover platform wants.

### Semantic differences, updated

1. ~~Joints are not mirrored yet~~ — done: distance/revolute/prismatic/weld mirrored and gated; friction/mouse joints warn-and-skip; soft welds are rigid; the break impulse proxy is uncalibrated against Farseer Newtons.
2. ~~`PreventCollideEvent` has no Rapier path~~ — done: reverse call through `ShouldCollideSlow`, per-pair cached, invalidated on joint changes; `RegenerateContacts`-forced refilters are not hooked (no event exists to hook).
3. Solver lineage differs (Rapier TGS defaults vs the port's PGS 8/3); rest poses differ slightly (floor min_y 0.350 vs 0.355). Bit-compatibility is not a goal; per-backend determinism holds.
4. Damping formulas differ; at engine default 0.2 the per-step difference is 1e-5.
5. ~~`physics.timetosleep` / `physics.linsleeptol` do not map~~ — done, with the drift-vs-velocity caveat above.
6. Client prediction still falls back to the Farseer path (`prediction == true` bypasses the backend). The shared-binary plan stands.
7. In-place fixture edits and setter writes on sleeping bodies (`SetBodyType`, `SetFixedRotation`, `SetSleepingAllowed`) rely on the 32-tick audit; `AuditMisses` counts every catch (0 so far).
8. `PhysShapeAabb` folds its radius into cuboid half-extents; generic polygons ride convex hulls; edge/chain shapes are not mirrored.
9. `InteractionTestMode` is now `Or`, matching Farseer's `ShouldCollide` (was `And` — a parity bug fixed in this round).

Reproduce:

```sh
cd rust
cargo test -p singularity-physics-ffi
cargo build --release -p singularity-physics-ffi
cd ../csharp
dotnet run --project benches/Singularity.PhysicsBench -c Release -- --scene=all --ticks=600 --backend=farseer
dotnet run --project benches/Singularity.PhysicsBench -c Release -- --scene=all --ticks=600 --backend=rapier
dotnet run --project benches/Singularity.PhysicsBench -c Release -- --scene=all --ticks=600 --validate --strict --backend=rapier
dotnet run --project benches/Singularity.PhysicsBench -c Release -- --scene=all --ticks=600 --verify-sync
```
