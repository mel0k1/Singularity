#![allow(clippy::missing_safety_doc)]

use std::collections::HashMap;
use std::sync::Mutex;

use rapier2d::prelude::*;

pub const ABI_VERSION: u32 = 1;

pub const KIND_DYNAMIC: u64 = 0;
pub const KIND_STATIC: u64 = 1;
pub const KIND_KINEMATIC: u64 = 2;

pub const OP_ADD_BODY: u32 = 1;
pub const OP_REMOVE_BODY: u32 = 2;
pub const OP_ADD_COLLIDER_BOX: u32 = 3;
pub const OP_ADD_COLLIDER_CIRCLE: u32 = 4;
pub const OP_ADD_COLLIDER_POLY: u32 = 5;
pub const OP_REMOVE_COLLIDER: u32 = 6;
pub const OP_SET_TRANSFORM: u32 = 7;
pub const OP_SET_VELOCITY: u32 = 8;
pub const OP_SET_BODY_KIND: u32 = 9;
pub const OP_SET_DAMPING: u32 = 10;
pub const OP_SET_FIXED_ROTATION: u32 = 11;
pub const OP_SET_AWAKE: u32 = 12;
pub const OP_SET_GRAVITY: u32 = 13;
pub const OP_SET_FORCE: u32 = 14;

pub const FLAG_ASLEEP: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SgCommand {
    pub op: u32,
    pub reserved: u32,
    pub id: u64,
    pub id2: u64,
    pub id3: u64,
    pub p: [f32; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SgBodyState {
    pub id: u64,
    pub kind: u32,
    pub flags: u32,
    pub x: f32,
    pub y: f32,
    pub angle: f32,
    pub vx: f32,
    pub vy: f32,
    pub omega: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SgContactEvent {
    pub a: u64,
    pub b: u64,
    pub started: u32,
    pub point_count: u32,
    pub nx: f32,
    pub ny: f32,
    pub p0x: f32,
    pub p0y: f32,
    pub p1x: f32,
    pub p1y: f32,
}

struct RawEvent {
    a: ColliderHandle,
    b: ColliderHandle,
    started: bool,
}

#[derive(Default)]
struct EventCollector {
    raw: Mutex<Vec<RawEvent>>,
}

impl EventHandler for EventCollector {
    fn handle_collision_event(
        &self,
        _bodies: &RigidBodySet,
        _colliders: &ColliderSet,
        event: CollisionEvent,
        _contact_pair: Option<&ContactPair>,
    ) {
        let (a, b, started) = match event {
            CollisionEvent::Started(a, b, _) => (a, b, true),
            CollisionEvent::Stopped(a, b, _) => (a, b, false),
        };
        self.raw.lock().unwrap().push(RawEvent { a, b, started });
    }

    fn handle_contact_force_event(
        &self,
        _dt: Real,
        _bodies: &RigidBodySet,
        _colliders: &ColliderSet,
        _contact_pair: &ContactPair,
        _total_force_magnitude: Real,
    ) {
    }

    fn handle_soft_body_tear_event(&self, _soft_bodies: &SoftBodySet, _event: &SoftBodyTearEvent) {}
}

pub struct SgWorld {
    physics: PhysicsWorld,
    collector: EventCollector,
    body_handles: HashMap<u64, RigidBodyHandle>,
    body_order: Vec<u64>,
    collider_handles: HashMap<u64, ColliderHandle>,
    collider_body: HashMap<u64, u64>,
    handle_collider: HashMap<ColliderHandle, u64>,
    vertices: Vec<f32>,
    events: Vec<SgContactEvent>,
}

impl Default for SgWorld {
    fn default() -> Self {
        SgWorld::new()
    }
}

impl SgWorld {
    pub fn new() -> SgWorld {
        let mut physics = PhysicsWorld::new();
        physics.gravity = Vector::new(0.0, -9.81);
        SgWorld {
            physics,
            collector: EventCollector::default(),
            body_handles: HashMap::new(),
            body_order: Vec::new(),
            collider_handles: HashMap::new(),
            collider_body: HashMap::new(),
            handle_collider: HashMap::new(),
            vertices: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn set_vertices(&mut self, data: &[f32]) {
        self.vertices.clear();
        self.vertices.extend_from_slice(data);
    }

    pub fn apply(&mut self, commands: &[SgCommand]) -> u32 {
        let mut consumed = 0u32;
        for cmd in commands {
            consumed += 1;
            match cmd.op {
                OP_ADD_BODY => self.add_body(cmd),
                OP_REMOVE_BODY => self.remove_body(cmd.id),
                OP_ADD_COLLIDER_BOX | OP_ADD_COLLIDER_CIRCLE | OP_ADD_COLLIDER_POLY => {
                    self.add_collider(cmd)
                }
                OP_REMOVE_COLLIDER => self.remove_collider(cmd.id),
                OP_SET_TRANSFORM => {
                    if let Some(body) = self.body_mut(cmd.id) {
                        body.set_position(
                            Pose2::new(Vector::new(cmd.p[0], cmd.p[1]), cmd.p[2]),
                            cmd.p[3] != 0.0,
                        );
                    }
                }
                OP_SET_VELOCITY => {
                    if let Some(body) = self.body_mut(cmd.id) {
                        let linvel = Vector::new(cmd.p[0], cmd.p[1]);
                        let delta = body.linvel() - linvel;
                        let wake =
                            delta.dot(delta) > 1.0e-12 || (body.angvel() - cmd.p[2]).abs() > 1.0e-6;
                        body.set_linvel(linvel, wake);
                        body.set_angvel(cmd.p[2], wake);
                    }
                }
                OP_SET_BODY_KIND => {
                    if let Some(body) = self.body_mut(cmd.id) {
                        body.set_body_type(kind_of(cmd.id2), true);
                    }
                }
                OP_SET_DAMPING => {
                    if let Some(body) = self.body_mut(cmd.id) {
                        body.set_linear_damping(cmd.p[0]);
                        body.set_angular_damping(cmd.p[1]);
                    }
                }
                OP_SET_FIXED_ROTATION => {
                    if let Some(body) = self.body_mut(cmd.id) {
                        body.set_enabled_rotations(true, true, cmd.id2 == 0, true);
                    }
                }
                OP_SET_AWAKE => {
                    if let Some(body) = self.body_mut(cmd.id) {
                        if cmd.id2 != 0 {
                            body.wake_up(true);
                        } else {
                            body.sleep();
                        }
                    }
                }
                OP_SET_GRAVITY => {
                    self.physics.gravity = Vector::new(cmd.p[0], cmd.p[1]);
                }
                OP_SET_FORCE => {
                    if let Some(body) = self.body_mut(cmd.id) {
                        body.reset_forces(false);
                        body.reset_torques(false);
                        if cmd.p[0] != 0.0 || cmd.p[1] != 0.0 {
                            body.add_force(Vector::new(cmd.p[0], cmd.p[1]), false);
                        }
                        if cmd.p[2] != 0.0 {
                            body.add_torque(cmd.p[2], false);
                        }
                    }
                }
                _ => {}
            }
        }
        consumed
    }

    pub fn step(&mut self, dt: f32) {
        self.physics.integration_parameters.dt = dt;
        self.collector.raw.lock().unwrap().clear();
        self.physics.step_with_events(&(), &self.collector);
        let raw = std::mem::take(&mut *self.collector.raw.lock().unwrap());
        self.events.clear();
        for e in raw {
            let Some(cid_a) = self.handle_collider.get(&e.a).copied() else {
                continue;
            };
            let Some(cid_b) = self.handle_collider.get(&e.b).copied() else {
                continue;
            };
            let mut event = SgContactEvent {
                a: cid_a,
                b: cid_b,
                started: u32::from(e.started),
                point_count: 0,
                nx: 0.0,
                ny: 0.0,
                p0x: 0.0,
                p0y: 0.0,
                p1x: 0.0,
                p1y: 0.0,
            };
            if e.started {
                self.fill_manifold(e.a, e.b, &mut event);
            }
            self.events.push(event);
        }
    }

    pub fn state_count(&self) -> usize {
        self.body_order.len()
    }

    pub fn copy_states(&self, out: &mut [SgBodyState]) -> usize {
        let mut n = 0usize;
        for id in &self.body_order {
            if n >= out.len() {
                break;
            }
            let Some(handle) = self.body_handles.get(id) else {
                continue;
            };
            let Some(body) = self.physics.bodies.get(*handle) else {
                continue;
            };
            let flags = if body.is_sleeping() { FLAG_ASLEEP } else { 0 };
            out[n] = SgBodyState {
                id: *id,
                kind: kind_u32(body.body_type()),
                flags,
                x: body.translation().x,
                y: body.translation().y,
                angle: body.rotation().angle(),
                vx: body.linvel().x,
                vy: body.linvel().y,
                omega: body.angvel(),
            };
            n += 1;
        }
        n
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    pub fn copy_events(&self, out: &mut [SgContactEvent]) -> usize {
        let n = self.events.len().min(out.len());
        out[..n].copy_from_slice(&self.events[..n]);
        n
    }

    fn fill_manifold(
        &self,
        handle_a: ColliderHandle,
        handle_b: ColliderHandle,
        event: &mut SgContactEvent,
    ) {
        let Some(collider) = self.physics.colliders.get(handle_a) else {
            return;
        };
        let pose = collider.position();
        let Some(pair) = self.physics.contact_pair(handle_a, handle_b) else {
            return;
        };
        let Some(manifold) = pair.manifolds().first() else {
            return;
        };
        let normal = pose.transform_vector(manifold.local_n1);
        event.nx = normal.x;
        event.ny = normal.y;
        let mut count = 0u32;
        for point in manifold.points.iter().take(2) {
            let world = pose.transform_point(point.local_p1);
            match count {
                0 => {
                    event.p0x = world.x;
                    event.p0y = world.y;
                }
                1 => {
                    event.p1x = world.x;
                    event.p1y = world.y;
                }
                _ => {}
            }
            count += 1;
        }
        event.point_count = count;
    }

    fn add_body(&mut self, cmd: &SgCommand) {
        if self.body_handles.contains_key(&cmd.id) {
            return;
        }
        let builder = kind_builder(cmd.id2)
            .translation(Vector::new(cmd.p[0], cmd.p[1]))
            .rotation(cmd.p[2]);
        let handle = self.physics.insert_body(builder);
        self.body_handles.insert(cmd.id, handle);
        self.body_order.push(cmd.id);
    }

    fn remove_body(&mut self, id: u64) {
        let Some(handle) = self.body_handles.remove(&id) else {
            return;
        };
        self.physics.remove_body_with_colliders(handle, true);
        self.body_order.retain(|other| *other != id);
        let stale: Vec<u64> = self
            .collider_body
            .iter()
            .filter(|(_, body)| **body == id)
            .map(|(cid, _)| *cid)
            .collect();
        for cid in stale {
            self.forget_collider(cid);
        }
    }

    fn add_collider(&mut self, cmd: &SgCommand) {
        if self.collider_handles.contains_key(&cmd.id) {
            return;
        }
        let Some(parent) = self.body_handles.get(&cmd.id2).copied() else {
            return;
        };
        let builder = match cmd.op {
            OP_ADD_COLLIDER_BOX => ColliderBuilder::cuboid(cmd.p[4], cmd.p[5])
                .translation(Vector::new(cmd.p[6], cmd.p[7])),
            OP_ADD_COLLIDER_CIRCLE => {
                ColliderBuilder::ball(cmd.p[4]).translation(Vector::new(cmd.p[5], cmd.p[6]))
            }
            OP_ADD_COLLIDER_POLY => {
                let offset = cmd.p[4] as usize;
                let count = cmd.p[5] as usize;
                let end = match offset.checked_add(count) {
                    Some(end) => end,
                    None => return,
                };
                if end > self.vertices.len() / 2 {
                    return;
                }
                let points: Vec<Vector> = (0..count)
                    .map(|i| {
                        Vector::new(
                            self.vertices[(offset + i) * 2],
                            self.vertices[(offset + i) * 2 + 1],
                        )
                    })
                    .collect();
                match ColliderBuilder::convex_hull(&points) {
                    Some(builder) => builder,
                    None => return,
                }
            }
            _ => return,
        };
        let builder = builder
            .friction(cmd.p[0])
            .restitution(cmd.p[1])
            .density(cmd.p[2])
            .sensor(cmd.p[3] != 0.0)
            .collision_groups(InteractionGroups::new(
                Group::from_bits_retain((cmd.id3 >> 32) as u32),
                Group::from_bits_retain(cmd.id3 as u32),
                InteractionTestMode::And,
            ))
            .active_events(ActiveEvents::COLLISION_EVENTS);
        let handle = self.physics.insert_collider(builder, Some(parent));
        self.collider_handles.insert(cmd.id, handle);
        self.collider_body.insert(cmd.id, cmd.id2);
        self.handle_collider.insert(handle, cmd.id);
    }

    fn remove_collider(&mut self, id: u64) {
        let Some(handle) = self.collider_handles.remove(&id) else {
            return;
        };
        self.physics.remove_collider(handle);
        self.forget_collider(id);
    }

    fn forget_collider(&mut self, id: u64) {
        self.collider_body.remove(&id);
        if let Some(handle) = self.collider_handles.remove(&id) {
            self.handle_collider.remove(&handle);
        }
    }

    fn body_mut(&mut self, id: u64) -> Option<&mut RigidBody> {
        let handle = self.body_handles.get(&id).copied()?;
        self.physics.bodies.get_mut(handle)
    }
}

fn kind_builder(kind: u64) -> RigidBodyBuilder {
    match kind {
        KIND_STATIC => RigidBodyBuilder::fixed(),
        KIND_KINEMATIC => RigidBodyBuilder::kinematic_velocity_based(),
        _ => RigidBodyBuilder::dynamic(),
    }
}

fn kind_of(kind: u64) -> RigidBodyType {
    match kind {
        KIND_STATIC => RigidBodyType::Fixed,
        KIND_KINEMATIC => RigidBodyType::KinematicVelocityBased,
        _ => RigidBodyType::Dynamic,
    }
}

fn kind_u32(kind: RigidBodyType) -> u32 {
    match kind {
        RigidBodyType::Fixed => 1,
        RigidBodyType::KinematicVelocityBased | RigidBodyType::KinematicPositionBased => 2,
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn sg_abi_version() -> u32 {
    ABI_VERSION
}

#[unsafe(no_mangle)]
pub extern "C" fn sg_world_create() -> *mut SgWorld {
    Box::into_raw(Box::new(SgWorld::new()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_world_destroy(world: *mut SgWorld) {
    if !world.is_null() {
        unsafe { drop(Box::from_raw(world)) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_world_set_vertices(
    world: *mut SgWorld,
    data: *const f32,
    count: usize,
) -> u32 {
    if world.is_null() || data.is_null() {
        return 0;
    }
    let world = unsafe { &mut *world };
    let slice = unsafe { std::slice::from_raw_parts(data, count) };
    world.set_vertices(slice);
    count as u32
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_world_apply(
    world: *mut SgWorld,
    commands: *const SgCommand,
    count: usize,
) -> u32 {
    if world.is_null() || commands.is_null() {
        return 0;
    }
    let world = unsafe { &mut *world };
    let slice = unsafe { std::slice::from_raw_parts(commands, count) };
    world.apply(slice)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_world_step(world: *mut SgWorld, dt: f32) {
    if world.is_null() {
        return;
    }
    unsafe { (*world).step(dt) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_world_state_count(world: *const SgWorld) -> usize {
    if world.is_null() {
        return 0;
    }
    unsafe { (*world).state_count() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_world_copy_states(
    world: *const SgWorld,
    out: *mut SgBodyState,
    cap: usize,
) -> usize {
    if world.is_null() || out.is_null() {
        return 0;
    }
    let world = unsafe { &*world };
    let slice = unsafe { std::slice::from_raw_parts_mut(out, cap) };
    world.copy_states(slice)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_world_event_count(world: *const SgWorld) -> usize {
    if world.is_null() {
        return 0;
    }
    unsafe { (*world).event_count() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_world_copy_events(
    world: *const SgWorld,
    out: *mut SgContactEvent,
    cap: usize,
) -> usize {
    if world.is_null() || out.is_null() {
        return 0;
    }
    let world = unsafe { &*world };
    let slice = unsafe { std::slice::from_raw_parts_mut(out, cap) };
    world.copy_events(slice)
}
