#![allow(clippy::missing_safety_doc)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;

use rapier2d::dynamics::JointAxis;
use rapier2d::prelude::*;

pub const ABI_VERSION: u32 = 2;

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
pub const OP_ADD_JOINT: u32 = 15;
pub const OP_REMOVE_JOINT: u32 = 16;
pub const OP_SET_SLEEP: u32 = 17;
pub const OP_SET_JOINT_PARAMS: u32 = 18;

pub const JOINT_DISTANCE_RIGID: u32 = 0;
pub const JOINT_DISTANCE_SPRING: u32 = 1;
pub const JOINT_REVOLUTE: u32 = 2;
pub const JOINT_PRISMATIC: u32 = 3;
pub const JOINT_WELD: u32 = 4;

pub const JOINT_LIMIT_ENABLED: f32 = 1.0;
pub const JOINT_MOTOR_ENABLED: f32 = 2.0;

pub const FLAG_ASLEEP: u32 = 1;

pub type PairFilterFn = extern "C" fn(ctx: *mut c_void, a: u64, b: u64) -> u32;

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

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SgJointEvent {
    pub id: u64,
    pub impulse: f32,
    pub reserved: u32,
    pub reserved2: u32,
}

#[derive(Clone, Copy)]
struct SleepConfig {
    time_until_sleep: f32,
    linear_threshold: f32,
    angular_threshold: f32,
}

impl Default for SleepConfig {
    fn default() -> Self {
        SleepConfig {
            time_until_sleep: 0.5,
            linear_threshold: 0.05,
            angular_threshold: 0.5,
        }
    }
}

#[derive(Clone, Copy)]
struct MirroredJoint {
    handle: ImpulseJointHandle,
    body_a: u64,
    body_b: u64,
    kind: u32,
    enabled: bool,
    collide_connected: bool,
    breakpoint: f32,
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

struct FilterCtx(*mut c_void);

impl Copy for FilterCtx {}

impl Clone for FilterCtx {
    fn clone(&self) -> Self {
        *self
    }
}

struct PairFilterState {
    handle_collider: HashMap<ColliderHandle, u64>,
    collider_body: HashMap<u64, u64>,
    joints: HashMap<u64, MirroredJoint>,
    callback: Option<(PairFilterFn, FilterCtx)>,
    cache: RefCell<HashMap<(u64, u64), bool>>,
    filter_calls: Cell<u64>,
    cache_hits: Cell<u64>,
}

impl Default for PairFilterState {
    fn default() -> Self {
        PairFilterState {
            handle_collider: HashMap::new(),
            collider_body: HashMap::new(),
            joints: HashMap::new(),
            callback: None,
            cache: RefCell::new(HashMap::new()),
            filter_calls: Cell::new(0),
            cache_hits: Cell::new(0),
        }
    }
}

struct SgHooks<'a> {
    state: &'a PairFilterState,
}

impl SgHooks<'_> {
    fn allow_pair(&self, collider_a: ColliderHandle, collider_b: ColliderHandle) -> bool {
        let Some(id_a) = self.state.handle_collider.get(&collider_a).copied() else {
            return true;
        };
        let Some(id_b) = self.state.handle_collider.get(&collider_b).copied() else {
            return true;
        };
        let key = if id_a < id_b {
            (id_a, id_b)
        } else {
            (id_b, id_a)
        };
        let cache = self.state.cache.borrow();
        if let Some(cached) = cache.get(&key) {
            self.state.cache_hits.set(self.state.cache_hits.get() + 1);
            return *cached;
        }
        drop(cache);
        let blocked = self.joint_blocks_pair(id_a, id_b);
        let allowed = if blocked {
            false
        } else {
            match self.state.callback {
                Some((filter, ctx)) => {
                    self.state
                        .filter_calls
                        .set(self.state.filter_calls.get() + 1);
                    filter(ctx.0, id_a, id_b) != 0
                }
                None => true,
            }
        };
        self.state.cache.borrow_mut().insert(key, allowed);
        allowed
    }

    fn joint_blocks_pair(&self, id_a: u64, id_b: u64) -> bool {
        let Some(body_a) = self.state.collider_body.get(&id_a) else {
            return false;
        };
        let Some(body_b) = self.state.collider_body.get(&id_b) else {
            return false;
        };
        if body_a == body_b {
            return true;
        }
        self.state.joints.values().any(|joint| {
            joint.enabled
                && !joint.collide_connected
                && ((joint.body_a == *body_a && joint.body_b == *body_b)
                    || (joint.body_a == *body_b && joint.body_b == *body_a))
        })
    }
}

impl PhysicsHooks for SgHooks<'_> {
    fn filter_contact_pair(&self, context: &PairFilterContext) -> Option<SolverFlags> {
        if self.allow_pair(context.collider1, context.collider2) {
            Some(SolverFlags::COMPUTE_RIGID_IMPULSES)
        } else {
            None
        }
    }

    fn filter_intersection_pair(&self, context: &PairFilterContext) -> bool {
        self.allow_pair(context.collider1, context.collider2)
    }

    fn modify_solver_contacts(&self, _context: &mut ContactModificationContext) {}
}

pub struct SgWorld {
    physics: PhysicsWorld,
    collector: EventCollector,
    filter: PairFilterState,
    body_handles: HashMap<u64, RigidBodyHandle>,
    body_order: Vec<u64>,
    collider_handles: HashMap<u64, ColliderHandle>,
    vertices: Vec<f32>,
    events: Vec<SgContactEvent>,
    joint_events: Vec<SgJointEvent>,
    sleep_config: SleepConfig,
    last_dt: f32,
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
            filter: PairFilterState::default(),
            body_handles: HashMap::new(),
            body_order: Vec::new(),
            collider_handles: HashMap::new(),
            vertices: Vec::new(),
            events: Vec::new(),
            joint_events: Vec::new(),
            sleep_config: SleepConfig::default(),
            last_dt: 1.0 / 60.0,
        }
    }

    pub fn set_vertices(&mut self, data: &[f32]) {
        self.vertices.clear();
        self.vertices.extend_from_slice(data);
    }

    pub fn set_pair_filter(&mut self, filter: Option<(PairFilterFn, *mut c_void)>) {
        self.filter.callback = filter.map(|(f, ctx)| (f, FilterCtx(ctx)));
        self.filter.cache.borrow_mut().clear();
    }

    pub fn set_sleep(
        &mut self,
        time_until_sleep: f32,
        linear_threshold: f32,
        angular_threshold: f32,
    ) {
        self.sleep_config = SleepConfig {
            time_until_sleep,
            linear_threshold,
            angular_threshold,
        };
        let handles: Vec<RigidBodyHandle> = self.body_handles.values().copied().collect();
        for handle in handles {
            self.apply_sleep(handle);
        }
    }

    fn apply_sleep(&mut self, handle: RigidBodyHandle) {
        let config = self.sleep_config;
        if let Some(body) = self.physics.bodies.get_mut(handle) {
            let activation = body.activation_mut();
            activation.time_until_sleep = config.time_until_sleep;
            activation.normalized_linear_threshold = config.linear_threshold;
            activation.angular_threshold = config.angular_threshold;
        }
    }

    fn set_body_sleep_allowed(&mut self, id: u64, allowed: bool) {
        let Some(handle) = self.body_handles.get(&id).copied() else {
            return;
        };
        if let Some(body) = self.physics.bodies.get_mut(handle) {
            if allowed {
                let config = self.sleep_config;
                let activation = body.activation_mut();
                activation.time_until_sleep = config.time_until_sleep;
                activation.normalized_linear_threshold = config.linear_threshold;
                activation.angular_threshold = config.angular_threshold;
            } else {
                let activation = body.activation_mut();
                activation.normalized_linear_threshold = -1.0;
                activation.angular_threshold = -1.0;
            }
        }
    }

    fn clear_pair_cache_for(&mut self, body_a: u64, body_b: u64) {
        let mut drop_keys = Vec::new();
        for (collider, body) in &self.filter.collider_body {
            if *body != body_a && *body != body_b {
                continue;
            }
            for (other, other_body) in &self.filter.collider_body {
                if *other_body != body_a && *other_body != body_b {
                    continue;
                }
                let key = if collider < other {
                    (*collider, *other)
                } else {
                    (*other, *collider)
                };
                drop_keys.push(key);
            }
        }
        for key in drop_keys {
            self.filter.cache.borrow_mut().remove(&key);
        }
    }

    pub fn apply(&mut self, commands: &[SgCommand]) -> u32 {
        let mut consumed = 0u32;
        let mut i = 0usize;
        while i < commands.len() {
            let cmd = commands[i];
            match cmd.op {
                OP_ADD_JOINT => {
                    let params = commands
                        .get(i + 1)
                        .filter(|next| next.op == OP_SET_JOINT_PARAMS && next.id == cmd.id)
                        .map(|next| next.p);
                    if let Some(params) = params {
                        self.add_joint(&cmd, &params);
                        i += 2;
                        consumed += 2;
                    } else {
                        i += 1;
                        consumed += 1;
                    }
                }
                OP_SET_JOINT_PARAMS => {
                    i += 1;
                    consumed += 1;
                }
                OP_REMOVE_JOINT => {
                    self.remove_joint(cmd.id);
                    i += 1;
                    consumed += 1;
                }
                OP_SET_SLEEP => {
                    if cmd.id == 0 {
                        self.set_sleep(cmd.p[0], cmd.p[1], cmd.p[2]);
                    } else {
                        self.set_body_sleep_allowed(cmd.id, cmd.p[3] != 0.0);
                    }
                    i += 1;
                    consumed += 1;
                }
                OP_ADD_BODY => {
                    self.add_body(&cmd);
                    i += 1;
                    consumed += 1;
                }
                OP_REMOVE_BODY => {
                    self.remove_body(cmd.id);
                    i += 1;
                    consumed += 1;
                }
                OP_ADD_COLLIDER_BOX | OP_ADD_COLLIDER_CIRCLE | OP_ADD_COLLIDER_POLY => {
                    self.add_collider(&cmd);
                    i += 1;
                    consumed += 1;
                }
                OP_REMOVE_COLLIDER => {
                    self.remove_collider(cmd.id);
                    i += 1;
                    consumed += 1;
                }
                OP_SET_TRANSFORM => {
                    if let Some(body) = self.body_mut(cmd.id) {
                        body.set_position(
                            Pose2::new(Vector::new(cmd.p[0], cmd.p[1]), cmd.p[2]),
                            cmd.p[3] != 0.0,
                        );
                    }
                    i += 1;
                    consumed += 1;
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
                    i += 1;
                    consumed += 1;
                }
                OP_SET_BODY_KIND => {
                    if let Some(body) = self.body_mut(cmd.id) {
                        body.set_body_type(kind_of(cmd.id2), true);
                    }
                    i += 1;
                    consumed += 1;
                }
                OP_SET_DAMPING => {
                    if let Some(body) = self.body_mut(cmd.id) {
                        body.set_linear_damping(cmd.p[0]);
                        body.set_angular_damping(cmd.p[1]);
                    }
                    i += 1;
                    consumed += 1;
                }
                OP_SET_FIXED_ROTATION => {
                    if let Some(body) = self.body_mut(cmd.id) {
                        body.set_enabled_rotations(true, true, cmd.id2 == 0, true);
                    }
                    i += 1;
                    consumed += 1;
                }
                OP_SET_AWAKE => {
                    if let Some(body) = self.body_mut(cmd.id) {
                        if cmd.id2 != 0 {
                            body.wake_up(true);
                        } else {
                            body.sleep();
                        }
                    }
                    i += 1;
                    consumed += 1;
                }
                OP_SET_GRAVITY => {
                    self.physics.gravity = Vector::new(cmd.p[0], cmd.p[1]);
                    i += 1;
                    consumed += 1;
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
                    i += 1;
                    consumed += 1;
                }
                _ => {
                    i += 1;
                    consumed += 1;
                }
            }
        }
        consumed
    }

    pub fn step(&mut self, dt: f32) {
        self.physics.integration_parameters.dt = dt;
        self.last_dt = dt;
        self.collector.raw.lock().unwrap().clear();
        {
            let hooks = SgHooks {
                state: &self.filter,
            };
            self.physics.step_with_events(&hooks, &self.collector);
        }
        let raw = std::mem::take(&mut *self.collector.raw.lock().unwrap());
        self.events.clear();
        for e in raw {
            let Some(cid_a) = self.filter.handle_collider.get(&e.a).copied() else {
                continue;
            };
            let Some(cid_b) = self.filter.handle_collider.get(&e.b).copied() else {
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
        self.collect_joint_breaks();
    }

    fn collect_joint_breaks(&mut self) {
        self.joint_events.clear();
        if self.filter.joints.is_empty() {
            return;
        }
        let dt = if self.last_dt > 0.0 {
            self.last_dt
        } else {
            1.0 / 60.0
        };
        let inv_dt = 1.0 / dt;
        let iterations = self
            .physics
            .integration_parameters
            .num_solver_iterations
            .max(1) as f32;
        let mut broken: Vec<u64> = Vec::new();
        for (id, joint) in &self.filter.joints {
            if !joint.enabled || joint.breakpoint == f32::MAX || joint.breakpoint <= 0.0 {
                continue;
            }
            let Some(impulse_joint) = self.physics.impulse_joints.get(joint.handle) else {
                continue;
            };
            let impulse = joint_force_magnitude(impulse_joint) * inv_dt * iterations;
            if impulse * impulse > joint.breakpoint * joint.breakpoint {
                self.joint_events.push(SgJointEvent {
                    id: *id,
                    impulse,
                    reserved: 0,
                    reserved2: 0,
                });
                broken.push(*id);
            }
        }
        for id in broken {
            if let Some(joint) = self.filter.joints.get_mut(&id) {
                self.physics.impulse_joints.remove(joint.handle, true);
                joint.enabled = false;
            }
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

    pub fn joint_event_count(&self) -> usize {
        self.joint_events.len()
    }

    pub fn copy_joint_events(&self, out: &mut [SgJointEvent]) -> usize {
        let n = self.joint_events.len().min(out.len());
        out[..n].copy_from_slice(&self.joint_events[..n]);
        n
    }

    pub fn joint_count(&self) -> usize {
        self.filter.joints.values().filter(|j| j.enabled).count()
    }

    pub fn joint_impulse(&self, id: u64) -> f32 {
        let Some(joint) = self.filter.joints.get(&id) else {
            return 0.0;
        };
        let Some(impulse_joint) = self.physics.impulse_joints.get(joint.handle) else {
            return 0.0;
        };
        let dt = if self.last_dt > 0.0 {
            self.last_dt
        } else {
            1.0 / 60.0
        };
        let iterations = self
            .physics
            .integration_parameters
            .num_solver_iterations
            .max(1) as f32;
        joint_force_magnitude(impulse_joint) / dt * iterations
    }
    pub fn filter_calls(&self) -> u64 {
        self.filter.filter_calls.get()
    }

    pub fn cache_hits(&self) -> u64 {
        self.filter.cache_hits.get()
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
        let config = self.sleep_config;
        if let Some(body) = self.physics.bodies.get_mut(handle) {
            let activation = body.activation_mut();
            activation.time_until_sleep = config.time_until_sleep;
            activation.normalized_linear_threshold = config.linear_threshold;
            activation.angular_threshold = config.angular_threshold;
        }
        self.body_handles.insert(cmd.id, handle);
        self.body_order.push(cmd.id);
    }

    fn remove_body(&mut self, id: u64) {
        let Some(handle) = self.body_handles.remove(&id) else {
            return;
        };
        self.remove_joints_for_body(id);
        self.physics.remove_body_with_colliders(handle, true);
        self.body_order.retain(|other| *other != id);
        let stale: Vec<u64> = self
            .filter
            .collider_body
            .iter()
            .filter(|(_, body)| **body == id)
            .map(|(cid, _)| *cid)
            .collect();
        for cid in stale {
            self.forget_collider(cid);
        }
    }

    fn add_joint(&mut self, cmd: &SgCommand, params: &[f32; 8]) {
        self.remove_joint(cmd.id);
        let joint = MirroredJoint {
            handle: ImpulseJointHandle::invalid(),
            body_a: cmd.id2,
            body_b: cmd.id3,
            kind: cmd.p[7] as u32,
            enabled: cmd.p[5] != 0.0,
            collide_connected: cmd.p[4] != 0.0,
            breakpoint: cmd.p[6],
        };
        self.filter.joints.insert(cmd.id, joint);
        if !joint.enabled {
            return;
        }
        let Some(handle_a) = self.body_handles.get(&cmd.id2).copied() else {
            return;
        };
        let Some(handle_b) = self.body_handles.get(&cmd.id3).copied() else {
            return;
        };
        let anchor_a = Vector::new(cmd.p[0], cmd.p[1]);
        let anchor_b = Vector::new(cmd.p[2], cmd.p[3]);
        let Some(joint_params) = build_joint(
            joint.kind,
            anchor_a,
            anchor_b,
            joint.collide_connected,
            params,
        ) else {
            return;
        };
        let handle = self
            .physics
            .impulse_joints
            .insert(handle_a, handle_b, joint_params, true);
        if let Some(stored) = self.filter.joints.get_mut(&cmd.id) {
            stored.handle = handle;
        }
        self.clear_pair_cache_for(cmd.id2, cmd.id3);
    }

    fn remove_joint(&mut self, id: u64) {
        let Some(joint) = self.filter.joints.remove(&id) else {
            return;
        };
        if joint.handle != ImpulseJointHandle::invalid() {
            self.physics.impulse_joints.remove(joint.handle, true);
        }
        self.clear_pair_cache_for(joint.body_a, joint.body_b);
    }

    fn remove_joints_for_body(&mut self, body: u64) {
        let stale: Vec<u64> = self
            .filter
            .joints
            .iter()
            .filter(|(_, joint)| joint.body_a == body || joint.body_b == body)
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            self.remove_joint(id);
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
                InteractionTestMode::Or,
            ))
            .active_events(ActiveEvents::COLLISION_EVENTS)
            .active_hooks(
                ActiveHooks::FILTER_CONTACT_PAIRS | ActiveHooks::FILTER_INTERSECTION_PAIR,
            );
        let handle = self.physics.insert_collider(builder, Some(parent));
        self.collider_handles.insert(cmd.id, handle);
        self.filter.collider_body.insert(cmd.id, cmd.id2);
        self.filter.handle_collider.insert(handle, cmd.id);
    }

    fn remove_collider(&mut self, id: u64) {
        let Some(handle) = self.collider_handles.remove(&id) else {
            return;
        };
        self.physics.remove_collider(handle);
        self.forget_collider(id);
    }

    fn forget_collider(&mut self, id: u64) {
        self.filter.collider_body.remove(&id);
        if let Some(handle) = self.collider_handles.remove(&id) {
            self.filter.handle_collider.remove(&handle);
        }
        self.filter
            .cache
            .borrow_mut()
            .retain(|(a, b), _| *a != id && *b != id);
    }

    fn body_mut(&mut self, id: u64) -> Option<&mut RigidBody> {
        let handle = self.body_handles.get(&id).copied()?;
        self.physics.bodies.get_mut(handle)
    }
}

fn build_joint(
    kind: u32,
    anchor_a: Vector,
    anchor_b: Vector,
    collide_connected: bool,
    params: &[f32; 8],
) -> Option<GenericJoint> {
    match kind {
        JOINT_DISTANCE_RIGID => {
            let min = params[0].max(0.0);
            let max = params[1].max(min);
            let mut joint = RopeJoint::new(max);
            joint.set_local_anchor1(anchor_a);
            joint.set_local_anchor2(anchor_b);
            joint.set_contacts_enabled(collide_connected);
            joint.data.set_limits(JointAxis::LinX, [min, max]);
            Some(joint.into())
        }
        JOINT_DISTANCE_SPRING => {
            if params[1] <= 0.0 {
                let rest = params[0].max(0.0);
                let mut joint = RopeJoint::new(rest);
                joint.set_local_anchor1(anchor_a);
                joint.set_local_anchor2(anchor_b);
                joint.set_contacts_enabled(collide_connected);
                joint.data.set_limits(JointAxis::LinX, [rest, rest]);
                return Some(joint.into());
            }
            let mut joint = SpringJoint::new(params[0].max(0.0), params[1], params[2]);
            joint.set_local_anchor1(anchor_a);
            joint.set_local_anchor2(anchor_b);
            joint.set_contacts_enabled(collide_connected);
            Some(joint.into())
        }
        JOINT_REVOLUTE => {
            let mut joint = RevoluteJoint::new();
            joint.set_local_anchor1(anchor_a);
            joint.set_local_anchor2(anchor_b);
            joint.set_contacts_enabled(collide_connected);
            let reference = params[0];
            joint.data.local_frame2.rotation = Rotation::new(-reference);
            if params[3] != 0.0 {
                joint.set_limits([params[1], params[2]]);
            }
            if params[3] >= JOINT_MOTOR_ENABLED {
                joint.set_motor_model(MotorModel::ForceBased);
                joint.set_motor_velocity(params[4], 1.0);
                joint.set_motor_max_force(params[5]);
            }
            Some(joint.into())
        }
        JOINT_PRISMATIC => {
            let mut joint = PrismaticJoint::new(Vector::new(params[0], params[1]));
            joint.set_local_anchor1(anchor_a);
            joint.set_local_anchor2(anchor_b);
            joint.set_contacts_enabled(collide_connected);
            let reference = params[2];
            joint.data.local_frame2.rotation = Rotation::new(-reference);
            if params[5] != 0.0 {
                joint.set_limits([params[3], params[4]]);
            }
            if params[5] >= JOINT_MOTOR_ENABLED {
                joint.set_motor_model(MotorModel::ForceBased);
                joint.set_motor_velocity(params[6], 1.0);
                joint.set_motor_max_force(params[7]);
            }
            Some(joint.into())
        }
        JOINT_WELD => {
            let mut joint = FixedJoint::new();
            joint.set_local_frame1(Pose2::new(anchor_a, 0.0));
            joint.set_local_frame2(Pose2::new(anchor_b, -params[0]));
            joint.set_contacts_enabled(collide_connected);
            Some(joint.into())
        }
        _ => None,
    }
}

fn joint_force_magnitude(joint: &ImpulseJoint) -> f32 {
    let mut sum = joint.impulses.length_squared();
    for limits in &joint.data.limits {
        sum += limits.impulse * limits.impulse;
    }
    for motor in &joint.data.motors {
        sum += motor.impulse * motor.impulse;
    }
    sum.sqrt()
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
pub unsafe extern "C" fn sg_world_set_pair_filter(
    world: *mut SgWorld,
    filter: Option<PairFilterFn>,
    ctx: *mut c_void,
) {
    if world.is_null() {
        return;
    }
    let world = unsafe { &mut *world };
    world.set_pair_filter(filter.map(|f| (f, ctx)));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_world_set_sleep(
    world: *mut SgWorld,
    time_until_sleep: f32,
    linear_threshold: f32,
    angular_threshold: f32,
) {
    if world.is_null() {
        return;
    }
    unsafe { (*world).set_sleep(time_until_sleep, linear_threshold, angular_threshold) };
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

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_world_joint_event_count(world: *const SgWorld) -> usize {
    if world.is_null() {
        return 0;
    }
    unsafe { (*world).joint_event_count() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_world_copy_joint_events(
    world: *const SgWorld,
    out: *mut SgJointEvent,
    cap: usize,
) -> usize {
    if world.is_null() || out.is_null() {
        return 0;
    }
    let world = unsafe { &*world };
    let slice = unsafe { std::slice::from_raw_parts_mut(out, cap) };
    world.copy_joint_events(slice)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sg_world_joint_count(world: *const SgWorld) -> usize {
    if world.is_null() {
        return 0;
    }
    unsafe { (*world).joint_count() }
}
