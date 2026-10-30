use singularity_physics_ffi::*;

const DT: f32 = 1.0 / 60.0;

fn cmd(op: u32, id: u64, id2: u64, id3: u64, p: [f32; 8]) -> SgCommand {
    SgCommand {
        op,
        reserved: 0,
        id,
        id2,
        id3,
        p,
    }
}

fn add_box_collider(world: &mut SgWorld, cid: u64, body: u64, hx: f32, hy: f32, friction: f32) {
    world.apply(&[cmd(
        OP_ADD_COLLIDER_BOX,
        cid,
        body,
        (2u64 << 32) | 2,
        [friction, 0.0, 1.0, 0.0, hx, hy, 0.0, 0.0],
    )]);
}

fn states(world: &SgWorld) -> Vec<SgBodyState> {
    let mut out = vec![SgBodyState::default(); world.state_count()];
    let n = world.copy_states(&mut out);
    out.truncate(n);
    out
}

fn events(world: &SgWorld) -> Vec<SgContactEvent> {
    let mut out = vec![SgContactEvent::default(); world.event_count()];
    let n = world.copy_events(&mut out);
    out.truncate(n);
    out
}

fn find_state(world: &SgWorld, id: u64) -> SgBodyState {
    states(world).into_iter().find(|s| s.id == id).unwrap()
}

fn drop_scene() -> SgWorld {
    let mut world = SgWorld::new();
    world.apply(&[cmd(
        OP_ADD_BODY,
        1,
        KIND_STATIC,
        0,
        [0.0, -0.55, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    )]);
    add_box_collider(&mut world, 10, 1, 10.0, 0.5, 0.9);
    world.apply(&[cmd(
        OP_ADD_BODY,
        2,
        KIND_DYNAMIC,
        0,
        [0.0, 5.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    )]);
    add_box_collider(&mut world, 11, 2, 0.5, 0.5, 0.6);
    world
}

#[test]
fn falls_and_rests() {
    let mut world = drop_scene();
    let mut slept = false;
    for _ in 0..600 {
        world.step(DT);
        if find_state(&world, 2).flags & FLAG_ASLEEP != 0 {
            slept = true;
        }
    }
    let body = find_state(&world, 2);
    assert!((body.y - 0.45).abs() < 0.02, "rest y {body:?}");
    assert!(body.vy.abs() < 0.01);
    assert!(body.angle.abs() < 0.01);
    assert!(slept, "body never slept");
}

#[test]
fn static_never_moves() {
    let mut world = drop_scene();
    for _ in 0..120 {
        world.step(DT);
    }
    let floor = find_state(&world, 1);
    assert_eq!(floor.kind, 1);
    assert_eq!(floor.y, -0.55);
}

#[test]
fn kinematic_carries_cargo() {
    let mut world = SgWorld::new();
    world.apply(&[cmd(
        OP_ADD_BODY,
        1,
        KIND_KINEMATIC,
        0,
        [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    )]);
    add_box_collider(&mut world, 10, 1, 4.0, 0.25, 0.9);
    world.apply(&[cmd(
        OP_ADD_BODY,
        2,
        KIND_DYNAMIC,
        0,
        [0.0, 0.75, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    )]);
    add_box_collider(&mut world, 11, 2, 0.45, 0.45, 0.6);
    world.apply(&[cmd(
        OP_SET_VELOCITY,
        1,
        0,
        0,
        [1.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    )]);
    for _ in 0..300 {
        world.step(DT);
    }
    let platform = find_state(&world, 1);
    let cargo = find_state(&world, 2);
    assert!(platform.x > 5.0, "platform did not move {platform:?}");
    assert!(cargo.x > 5.0, "cargo not carried {cargo:?}");
}

#[test]
fn contact_events_start_and_stop() {
    let mut world = drop_scene();
    let mut starts: Vec<SgContactEvent> = Vec::new();
    for _ in 0..120 {
        world.step(DT);
        starts.extend(
            events(&world)
                .into_iter()
                .filter(|event| event.started != 0),
        );
    }
    assert!(!starts.is_empty(), "no start events");
    for event in &starts {
        assert!(event.point_count >= 1);
        assert!(event.ny.abs() > 0.7, "normal {event:?}");
    }
    world.apply(&[cmd(
        OP_SET_TRANSFORM,
        1,
        0,
        0,
        [0.0, -60.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    )]);
    let mut stopped = false;
    for _ in 0..120 {
        world.step(DT);
        stopped |= events(&world).iter().any(|event| event.started == 0);
    }
    assert!(stopped, "no stop events");
}

#[test]
fn force_hovers_against_gravity() {
    let mut world = SgWorld::new();
    world.apply(&[cmd(
        OP_ADD_BODY,
        1,
        KIND_DYNAMIC,
        0,
        [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    )]);
    add_box_collider(&mut world, 10, 1, 0.5, 0.5, 0.6);
    world.apply(&[cmd(
        OP_SET_FORCE,
        1,
        0,
        0,
        [0.0, 19.62, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    )]);
    for _ in 0..120 {
        world.step(DT);
    }
    let body = find_state(&world, 1);
    assert!(body.y > 1.0, "force did not lift {body:?}");
}

#[test]
fn polygon_collider_from_vertex_pool() {
    let mut world = SgWorld::new();
    world.set_vertices(&[-0.5, -0.5, 0.5, -0.5, 0.5, 0.5, -0.5, 0.5]);
    world.apply(&[cmd(
        OP_ADD_BODY,
        1,
        KIND_STATIC,
        0,
        [0.0, -0.55, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    )]);
    world.apply(&[cmd(
        OP_ADD_COLLIDER_POLY,
        10,
        1,
        (2u64 << 32) | 2,
        [0.9, 0.0, 1.0, 0.0, 0.0, 4.0, 0.0, 0.0],
    )]);
    world.apply(&[cmd(
        OP_ADD_BODY,
        2,
        KIND_DYNAMIC,
        0,
        [0.0, 5.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    )]);
    add_box_collider(&mut world, 11, 2, 0.5, 0.5, 0.6);
    for _ in 0..600 {
        world.step(DT);
    }
    let body = find_state(&world, 2);
    assert!((body.y - 0.45).abs() < 0.02, "poly floor rest {body:?}");
}

#[test]
fn teleport_wake_flag_controls_sleeping() {
    let mut world = drop_scene();
    for _ in 0..600 {
        world.step(DT);
    }
    let rest = find_state(&world, 2);
    assert!(rest.flags & FLAG_ASLEEP != 0, "never slept");
    world.apply(&[cmd(
        OP_SET_TRANSFORM,
        2,
        0,
        0,
        [rest.x, rest.y, rest.angle, 0.0, 0.0, 0.0, 0.0, 0.0],
    )]);
    world.step(DT);
    assert!(
        find_state(&world, 2).flags & FLAG_ASLEEP != 0,
        "identical pose echo woke the body"
    );
    world.apply(&[cmd(
        OP_SET_TRANSFORM,
        2,
        0,
        0,
        [1.0, 5.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
    )]);
    world.step(DT);
    assert_eq!(
        find_state(&world, 2).flags & FLAG_ASLEEP,
        0,
        "flag did not wake"
    );
}

#[test]
fn remove_body_clears_state() {
    let mut world = drop_scene();
    world.apply(&[cmd(OP_REMOVE_BODY, 2, 0, 0, [0.0; 8])]);
    world.step(DT);
    assert_eq!(world.state_count(), 1);
    assert_eq!(world.event_count(), 0);
}

#[test]
fn ffi_surface_end_to_end() {
    unsafe {
        assert_eq!(sg_abi_version(), ABI_VERSION);
        let world = sg_world_create();
        assert!(!world.is_null());
        let vertices: [f32; 8] = [-0.5, -0.5, 0.5, -0.5, 0.5, 0.5, -0.5, 0.5];
        assert_eq!(
            sg_world_set_vertices(world, vertices.as_ptr(), vertices.len()),
            8
        );
        let commands = [
            cmd(
                OP_ADD_BODY,
                1,
                KIND_STATIC,
                0,
                [0.0, -0.55, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            ),
            cmd(
                OP_ADD_COLLIDER_POLY,
                10,
                1,
                (2u64 << 32) | 2,
                [0.9, 0.0, 1.0, 0.0, 0.0, 4.0, 0.0, 0.0],
            ),
            cmd(
                OP_ADD_BODY,
                2,
                KIND_DYNAMIC,
                0,
                [0.0, 5.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            ),
            cmd(
                OP_ADD_COLLIDER_CIRCLE,
                11,
                2,
                (2u64 << 32) | 2,
                [0.6, 0.0, 1.0, 0.0, 0.35, 0.0, 0.0, 0.0],
            ),
        ];
        assert_eq!(
            sg_world_apply(world, commands.as_ptr(), commands.len()),
            commands.len() as u32
        );
        let mut total_events = 0usize;
        for _ in 0..600 {
            sg_world_step(world, DT);
            total_events += sg_world_event_count(world);
        }
        let count = sg_world_state_count(world);
        assert_eq!(count, 2);
        let mut out = vec![SgBodyState::default(); count];
        assert_eq!(
            sg_world_copy_states(world, out.as_mut_ptr(), out.len()),
            count
        );
        let body = out.into_iter().find(|s| s.id == 2).unwrap();
        assert!((body.y - 0.3).abs() < 0.02, "circle rest {body:?}");
        assert!(total_events > 0);
        sg_world_destroy(world);
    }
}

fn add_body(world: &mut SgWorld, id: u64, kind: u64, x: f32, y: f32) {
    world.apply(&[cmd(
        OP_ADD_BODY,
        id,
        kind,
        0,
        [x, y, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    )]);
}

struct JointSpec {
    id: u64,
    a: u64,
    b: u64,
    kind: u32,
    anchor_a: [f32; 2],
    anchor_b: [f32; 2],
    collide_connected: bool,
    enabled: bool,
    breakpoint: f32,
    params: [f32; 8],
}

fn add_joint(world: &mut SgWorld, spec: JointSpec) {
    world.apply(&[
        cmd(
            OP_ADD_JOINT,
            spec.id,
            spec.a,
            spec.b,
            [
                spec.anchor_a[0],
                spec.anchor_a[1],
                spec.anchor_b[0],
                spec.anchor_b[1],
                if spec.collide_connected { 1.0 } else { 0.0 },
                if spec.enabled { 1.0 } else { 0.0 },
                spec.breakpoint,
                spec.kind as f32,
            ],
        ),
        cmd(OP_SET_JOINT_PARAMS, spec.id, 0, 0, spec.params),
    ]);
}

fn distance(ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    ((ax - bx) * (ax - bx) + (ay - by) * (ay - by)).sqrt()
}

#[test]
fn rigid_distance_joint_holds_gap() {
    let mut world = SgWorld::new();
    add_body(&mut world, 1, KIND_DYNAMIC, -1.0, 20.0);
    add_body(&mut world, 2, KIND_DYNAMIC, 1.0, 20.0);
    add_box_collider(&mut world, 10, 1, 0.4, 0.4, 0.6);
    add_box_collider(&mut world, 11, 2, 0.4, 0.4, 0.6);
    add_joint(
        &mut world,
        JointSpec {
            id: 100,
            a: 1,
            b: 2,
            kind: JOINT_DISTANCE_RIGID,
            anchor_a: [0.4, 0.0],
            anchor_b: [-0.4, 0.0],
            collide_connected: true,
            enabled: true,
            breakpoint: f32::MAX,
            params: [2.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        },
    );
    assert_eq!(world.joint_count(), 1);
    for _ in 0..240 {
        world.step(DT);
    }
    let a = find_state(&world, 1);
    let b = find_state(&world, 2);
    let gap = distance(a.x, a.y, b.x, b.y);
    assert!((gap - 2.0).abs() < 0.05, "gap drifted {gap}");
}

#[test]
fn rope_joint_limits_distance() {
    let mut world = SgWorld::new();
    add_body(&mut world, 1, KIND_STATIC, 0.0, 0.0);
    add_body(&mut world, 2, KIND_DYNAMIC, 1.5, -1.0);
    add_box_collider(&mut world, 10, 1, 0.1, 0.1, 0.6);
    add_box_collider(&mut world, 11, 2, 0.4, 0.4, 0.6);
    add_joint(
        &mut world,
        JointSpec {
            id: 100,
            a: 1,
            b: 2,
            kind: JOINT_DISTANCE_RIGID,
            anchor_a: [0.0, 0.0],
            anchor_b: [0.0, 0.0],
            collide_connected: true,
            enabled: true,
            breakpoint: f32::MAX,
            params: [1.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        },
    );
    for _ in 0..240 {
        world.step(DT);
    }
    let b = find_state(&world, 2);
    let r = b.x * b.x + b.y * b.y;
    assert!(r.sqrt() <= 2.05, "rope max violated {b:?}");
    assert!(r.sqrt() >= 0.95, "rope min violated {b:?}");
}

#[test]
fn spring_joint_settles_at_rest_length() {
    let mut world = SgWorld::new();
    add_body(&mut world, 1, KIND_STATIC, 0.0, 0.0);
    add_body(&mut world, 2, KIND_DYNAMIC, 0.0, -3.0);
    add_box_collider(&mut world, 10, 1, 0.1, 0.1, 0.6);
    add_box_collider(&mut world, 11, 2, 0.4, 0.4, 0.6);
    add_joint(
        &mut world,
        JointSpec {
            id: 100,
            a: 1,
            b: 2,
            kind: JOINT_DISTANCE_SPRING,
            anchor_a: [0.0, 0.0],
            anchor_b: [0.0, 0.0],
            collide_connected: true,
            enabled: true,
            breakpoint: f32::MAX,
            params: [1.5, 20.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        },
    );
    for _ in 0..600 {
        world.step(DT);
    }
    let b = find_state(&world, 2);
    let r = (b.x * b.x + b.y * b.y).sqrt();
    let equilibrium = 1.5 + 0.64 * 9.81 / 20.0;
    assert!((r - equilibrium).abs() < 0.08, "spring rest {b:?}");
}

#[test]
fn revolute_joint_keeps_attachment() {
    let mut world = SgWorld::new();
    add_body(&mut world, 1, KIND_STATIC, 0.0, 10.0);
    add_body(&mut world, 2, KIND_DYNAMIC, 0.9, 10.0);
    add_box_collider(&mut world, 10, 1, 0.1, 0.1, 0.6);
    add_box_collider(&mut world, 11, 2, 0.45, 0.45, 0.6);
    add_joint(
        &mut world,
        JointSpec {
            id: 100,
            a: 1,
            b: 2,
            kind: JOINT_REVOLUTE,
            anchor_a: [0.0, 0.0],
            anchor_b: [0.9, 0.0],
            collide_connected: false,
            enabled: true,
            breakpoint: f32::MAX,
            params: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        },
    );
    for _ in 0..300 {
        world.step(DT);
    }
    let b = find_state(&world, 2);
    let d = distance(b.x, b.y, 0.0, 10.0);
    assert!(d < 1.2, "pendulum detached {b:?}");
    assert!(b.y < 10.0, "pendulum did not swing {b:?}");
}

#[test]
fn revolute_motor_spins() {
    let mut world = SgWorld::new();
    add_body(&mut world, 1, KIND_STATIC, 0.0, 0.0);
    add_body(&mut world, 2, KIND_DYNAMIC, 1.0, 0.0);
    add_box_collider(&mut world, 10, 1, 0.1, 0.1, 0.6);
    add_box_collider(&mut world, 11, 2, 0.45, 0.45, 0.6);
    add_joint(
        &mut world,
        JointSpec {
            id: 100,
            a: 1,
            b: 2,
            kind: JOINT_REVOLUTE,
            anchor_a: [0.0, 0.0],
            anchor_b: [1.0, 0.0],
            collide_connected: true,
            enabled: true,
            breakpoint: f32::MAX,
            params: [0.0, 0.0, 0.0, JOINT_MOTOR_ENABLED, 4.0, 50.0, 0.0, 0.0],
        },
    );
    let mut angle = 0.0f32;
    for _ in 0..300 {
        world.step(DT);
        let b = find_state(&world, 2);
        angle = b.x.atan2(b.y);
    }
    let spins = angle;
    assert!(spins.abs() > 1.0, "motor did not spin {spins}");
}

#[test]
fn weld_joint_holds_relative_angle() {
    let mut world = SgWorld::new();
    add_body(&mut world, 1, KIND_DYNAMIC, 0.0, 20.0);
    world.apply(&[cmd(
        OP_ADD_BODY,
        2,
        KIND_DYNAMIC,
        0,
        [
            1.0,
            20.0,
            std::f32::consts::FRAC_PI_2,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
        ],
    )]);
    add_box_collider(&mut world, 10, 1, 0.4, 0.4, 0.6);
    add_box_collider(&mut world, 11, 2, 0.4, 0.4, 0.6);
    add_joint(
        &mut world,
        JointSpec {
            id: 100,
            a: 1,
            b: 2,
            kind: JOINT_WELD,
            anchor_a: [0.4, 0.0],
            anchor_b: [-0.4, 0.0],
            collide_connected: true,
            enabled: true,
            breakpoint: f32::MAX,
            params: [
                std::f32::consts::FRAC_PI_2,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
            ],
        },
    );
    for _ in 0..240 {
        world.step(DT);
    }
    let a = find_state(&world, 1);
    let b = find_state(&world, 2);
    let relative = (b.angle - a.angle).rem_euclid(std::f32::consts::TAU);
    let expected = std::f32::consts::FRAC_PI_2;
    assert!(
        (relative - expected).abs() < 0.1
            || (relative - expected + std::f32::consts::TAU).abs() < 0.1,
        "weld relative angle {relative}"
    );
}

#[test]
fn joint_break_emits_event_and_disables() {
    let mut world = SgWorld::new();
    add_body(&mut world, 1, KIND_STATIC, 0.0, 0.0);
    add_body(&mut world, 2, KIND_DYNAMIC, 1.5, -0.5);
    add_box_collider(&mut world, 10, 1, 0.1, 0.1, 0.6);
    add_box_collider(&mut world, 11, 2, 0.45, 0.45, 0.6);
    add_joint(
        &mut world,
        JointSpec {
            id: 100,
            a: 1,
            b: 2,
            kind: JOINT_DISTANCE_RIGID,
            anchor_a: [0.0, 0.0],
            anchor_b: [0.0, 0.0],
            collide_connected: true,
            enabled: true,
            breakpoint: 100.0,
            params: [0.2, 0.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        },
    );
    let mut broken = false;
    for _ in 0..120 {
        world.step(DT);
        let mut out = vec![SgJointEvent::default(); world.joint_event_count()];
        let n = world.copy_joint_events(&mut out);
        if n > 0 {
            broken = true;
            assert_eq!(out[0].id, 100);
            assert!(out[0].impulse > 0.0);
        }
    }
    assert!(broken, "joint never broke");
    assert_eq!(world.joint_count(), 0);
    let b = find_state(&world, 2);
    assert!(b.y < -5.0, "broken joint still held {b:?}");

    let mut stubborn = SgWorld::new();
    add_body(&mut stubborn, 1, KIND_STATIC, 0.0, 0.0);
    add_body(&mut stubborn, 2, KIND_DYNAMIC, 1.5, -0.5);
    add_box_collider(&mut stubborn, 10, 1, 0.1, 0.1, 0.6);
    add_box_collider(&mut stubborn, 11, 2, 0.45, 0.45, 0.6);
    add_joint(
        &mut stubborn,
        JointSpec {
            id: 100,
            a: 1,
            b: 2,
            kind: JOINT_DISTANCE_RIGID,
            anchor_a: [0.0, 0.0],
            anchor_b: [0.0, 0.0],
            collide_connected: true,
            enabled: true,
            breakpoint: f32::MAX,
            params: [0.2, 0.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        },
    );
    for _ in 0..120 {
        stubborn.step(DT);
        assert_eq!(stubborn.joint_event_count(), 0);
    }
    assert_eq!(stubborn.joint_count(), 1);
}

#[test]
fn collide_connected_false_blocks_contacts() {
    let mut world = SgWorld::new();
    add_body(&mut world, 1, KIND_STATIC, 0.0, -0.55);
    add_body(&mut world, 2, KIND_DYNAMIC, 0.0, 5.0);
    add_box_collider(&mut world, 10, 1, 10.0, 0.5, 0.9);
    add_box_collider(&mut world, 11, 2, 0.5, 0.5, 0.6);
    add_joint(
        &mut world,
        JointSpec {
            id: 100,
            a: 1,
            b: 2,
            kind: JOINT_DISTANCE_RIGID,
            anchor_a: [0.0, 0.0],
            anchor_b: [0.0, 0.0],
            collide_connected: false,
            enabled: true,
            breakpoint: f32::MAX,
            params: [50.0, 100.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        },
    );
    let mut starts = 0u32;
    for _ in 0..240 {
        world.step(DT);
        starts += events(&world).iter().filter(|e| e.started != 0).count() as u32;
    }
    assert_eq!(starts, 0, "contacts leaked through joint");
    let b = find_state(&world, 2);
    assert!(b.y < -20.0, "body did not fall through {b:?}");
}

extern "C" fn rejecting_filter(_ctx: *mut std::ffi::c_void, _a: u64, _b: u64) -> u32 {
    0
}

static COUNTING_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

extern "C" fn counting_filter(_ctx: *mut std::ffi::c_void, _a: u64, _b: u64) -> u32 {
    COUNTING_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    1
}

#[test]
fn pair_filter_rejects_and_caches() {
    let mut world = drop_scene();
    world.set_pair_filter(Some((rejecting_filter, std::ptr::null_mut())));
    for _ in 0..120 {
        world.step(DT);
    }
    assert_eq!(events(&world).len(), 0);
    let body = find_state(&world, 2);
    assert!(body.y < -10.0, "filter did not reject floor {body:?}");
    assert!(world.filter_calls() > 0);
    assert!(world.cache_hits() >= world.filter_calls());

    world.set_pair_filter(None);
    let mut world_b = drop_scene();
    for _ in 0..240 {
        world_b.step(DT);
    }
    let body = find_state(&world_b, 2);
    assert!(
        (body.y - 0.45).abs() < 0.05,
        "no-filter scene broken {body:?}"
    );
}

#[test]
fn joint_add_invalidates_pair_cache() {
    let mut world = drop_scene();
    COUNTING_CALLS.store(0, std::sync::atomic::Ordering::Relaxed);
    world.set_pair_filter(Some((counting_filter, std::ptr::null_mut())));
    for _ in 0..240 {
        world.step(DT);
    }
    let body = find_state(&world, 2);
    assert!(
        (body.y - 0.45).abs() < 0.05,
        "counting filter broke rest {body:?}"
    );
    let calls_before = COUNTING_CALLS.load(std::sync::atomic::Ordering::Relaxed);
    assert!(calls_before > 0);

    add_joint(
        &mut world,
        JointSpec {
            id: 100,
            a: 1,
            b: 2,
            kind: JOINT_DISTANCE_RIGID,
            anchor_a: [0.0, 0.0],
            anchor_b: [0.0, 0.0],
            collide_connected: false,
            enabled: true,
            breakpoint: f32::MAX,
            params: [50.0, 100.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        },
    );
    let mut stopped = false;
    for _ in 0..240 {
        world.step(DT);
        stopped |= events(&world).iter().any(|e| e.started == 0);
    }
    assert!(stopped, "joint did not end the cached contact");
    let body = find_state(&world, 2);
    assert!(body.y < -20.0, "joint blocking not applied {body:?}");
    let calls_after = COUNTING_CALLS.load(std::sync::atomic::Ordering::Relaxed);
    assert!(calls_after >= calls_before, "no re-evaluation happened");
}

#[test]
fn sleep_disabled_body_never_sleeps() {
    let mut world = drop_scene();
    world.apply(&[cmd(
        OP_SET_SLEEP,
        2,
        0,
        0,
        [0.2, 0.01, 0.005, 0.0, 0.0, 0.0, 0.0, 0.0],
    )]);
    for _ in 0..600 {
        world.step(DT);
    }
    let body = find_state(&world, 2);
    assert_eq!(body.flags & FLAG_ASLEEP, 0, "body slept despite disallow");
    assert!((body.y - 0.45).abs() < 0.05, "body not resting {body:?}");
}

#[test]
fn sleep_time_until_sleep_delays() {
    let mut slow = drop_scene();
    slow.set_sleep(2.0, 0.01, 0.005);
    for _ in 0..60 {
        slow.step(DT);
    }
    let body = find_state(&slow, 2);
    assert_eq!(body.flags & FLAG_ASLEEP, 0, "slept before time_until_sleep");

    let mut fast = drop_scene();
    fast.set_sleep(0.05, 0.05, 0.5);
    let mut slept_by = None;
    for t in 0..240 {
        fast.step(DT);
        if find_state(&fast, 2).flags & FLAG_ASLEEP != 0 {
            slept_by = Some(t);
            break;
        }
    }
    assert!(slept_by.is_some(), "never slept fast");
}

#[test]
fn removed_joint_releases_bodies() {
    let mut world = SgWorld::new();
    add_body(&mut world, 1, KIND_DYNAMIC, -1.0, 20.0);
    add_body(&mut world, 2, KIND_DYNAMIC, 1.0, 20.0);
    add_box_collider(&mut world, 10, 1, 0.4, 0.4, 0.6);
    add_box_collider(&mut world, 11, 2, 0.4, 0.4, 0.6);
    add_joint(
        &mut world,
        JointSpec {
            id: 100,
            a: 1,
            b: 2,
            kind: JOINT_DISTANCE_RIGID,
            anchor_a: [0.4, 0.0],
            anchor_b: [-0.4, 0.0],
            collide_connected: true,
            enabled: true,
            breakpoint: f32::MAX,
            params: [2.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        },
    );
    world.apply(&[cmd(OP_REMOVE_JOINT, 100, 0, 0, [0.0; 8])]);
    assert_eq!(world.joint_count(), 0);
    for _ in 0..60 {
        world.step(DT);
    }
    let a = find_state(&world, 1);
    let b = find_state(&world, 2);
    assert!((a.y - 20.0).abs() > 1.0, "bodies did not fall freely {a:?}");
    assert!((a.y - b.y).abs() < 0.01, "bodies diverged without a joint");
}
