use std::collections::HashMap;
use std::sync::Mutex;

use rapier2d::prelude::*;

use crate::{BodyId, BodyKind, BodySnapshot, BodySpec, ColliderSpec, PhysicsBackend, Shape, Vec2};

pub struct RapierBackend {
    world: PhysicsWorld,
    bodies: Vec<Option<RigidBodyHandle>>,
    collider_owner: HashMap<ColliderHandle, BodyId>,
    collector: EventCollector,
    next_id: BodyId,
}

struct RawEvent {
    a: ColliderHandle,
    b: ColliderHandle,
    started: bool,
}

struct EventCollector {
    raw: Mutex<Vec<RawEvent>>,
}

impl EventHandler for EventCollector {
    fn handle_collision_event(
        &self,
        _bodies: &RigidBodySet,
        _colliders: &ColliderSet,
        event: CollisionEvent,
        _contact_pair: Option<&rapier2d::prelude::ContactPair>,
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
        _contact_pair: &rapier2d::prelude::ContactPair,
        _total_force_magnitude: Real,
    ) {
    }

    fn handle_soft_body_tear_event(&self, _soft_bodies: &SoftBodySet, _event: &SoftBodyTearEvent) {}
}

impl RapierBackend {
    pub fn new() -> RapierBackend {
        let mut world = PhysicsWorld::new();
        world.gravity = Vector::new(0.0, -9.81);
        RapierBackend {
            world,
            bodies: Vec::new(),
            collider_owner: HashMap::new(),
            collector: EventCollector {
                raw: Mutex::new(Vec::new()),
            },
            next_id: 0,
        }
    }
}

impl Default for RapierBackend {
    fn default() -> Self {
        RapierBackend::new()
    }
}

impl PhysicsBackend for RapierBackend {
    fn name(&self) -> &'static str {
        "rapier"
    }

    fn add_body(&mut self, spec: &BodySpec) -> BodyId {
        let builder = match spec.kind {
            BodyKind::Static => RigidBodyBuilder::fixed(),
            BodyKind::Dynamic => RigidBodyBuilder::dynamic(),
            BodyKind::Kinematic => RigidBodyBuilder::kinematic_velocity_based(),
        }
        .translation(Vector::new(spec.translation.x, spec.translation.y))
        .rotation(spec.rotation)
        .linvel(Vector::new(spec.linear_velocity.x, spec.linear_velocity.y));
        let handle = self.world.insert_body(builder);
        let id = self.next_id;
        self.next_id += 1;
        debug_assert_eq!(self.bodies.len(), id as usize);
        self.bodies.push(Some(handle));
        for c in &spec.colliders {
            let ch = self
                .world
                .insert_collider(collider_builder(c), Some(handle));
            self.collider_owner.insert(ch, id);
        }
        id
    }

    fn remove_body(&mut self, id: BodyId) {
        if let Some(Some(handle)) = self.bodies.get(id as usize) {
            let handle = *handle;
            self.world.remove_body_with_colliders(handle, true);
        }
        if let Some(slot) = self.bodies.get_mut(id as usize) {
            *slot = None;
        }
    }

    fn set_body_velocity(&mut self, id: BodyId, velocity: Vec2) {
        let handle = self.bodies.get(id as usize).copied().flatten();
        if let Some(body) = handle.and_then(|h| self.world.bodies.get_mut(h)) {
            body.set_linvel(Vector::new(velocity.x, velocity.y), true);
        }
    }

    fn snapshot(&self, id: BodyId) -> BodySnapshot {
        let handle = self.bodies.get(id as usize).copied().flatten();
        if let Some(body) = handle.and_then(|h| self.world.bodies.get(h)) {
            BodySnapshot {
                translation: to_vec2(body.translation()),
                rotation: body.rotation().angle(),
                linear_velocity: to_vec2(body.linvel()),
                angular_velocity: body.angvel(),
                sleeping: body.is_sleeping(),
            }
        } else {
            BodySnapshot {
                translation: Vec2::ZERO,
                rotation: 0.0,
                linear_velocity: Vec2::ZERO,
                angular_velocity: 0.0,
                sleeping: true,
            }
        }
    }

    fn step(&mut self, dt: f32) -> Vec<crate::ContactPair> {
        self.world.integration_parameters.dt = dt;
        self.collector.raw.lock().unwrap().clear();
        self.world.step_with_events(&(), &self.collector);
        let raw = std::mem::take(&mut *self.collector.raw.lock().unwrap());
        raw.into_iter()
            .filter_map(|e| {
                let a = self.collider_owner.get(&e.a).copied();
                let b = self.collider_owner.get(&e.b).copied();
                Some(crate::ContactPair {
                    a: a?,
                    b: b?,
                    started: e.started,
                })
            })
            .collect()
    }
}

fn collider_builder(c: &ColliderSpec) -> ColliderBuilder {
    match c.shape {
        Shape::Disc { radius } => ColliderBuilder::ball(radius),
        Shape::Box { half } => ColliderBuilder::cuboid(half.x, half.y),
        Shape::Ngon { radius, sides } => {
            let points = ngon_points(radius, sides);
            ColliderBuilder::convex_hull(&points).unwrap()
        }
    }
    .friction(c.friction)
    .restitution(c.restitution)
    .density(c.density)
    .active_events(ActiveEvents::COLLISION_EVENTS)
    .translation(Vector::new(c.offset.x, c.offset.y))
}

fn ngon_points(radius: f32, sides: u32) -> Vec<Vector> {
    (0..sides)
        .map(|i| {
            let a = i as f32 * std::f32::consts::TAU / sides as f32;
            Vector::new(radius * a.cos(), radius * a.sin())
        })
        .collect()
}

fn to_vec2(v: Vector) -> Vec2 {
    Vec2::new(v.x, v.y)
}
