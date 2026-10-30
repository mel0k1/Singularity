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
