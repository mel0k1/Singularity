use std::collections::HashMap;
use std::time::Duration;

use avian2d::prelude::*;
use bevy::app::{App, PostUpdate, TaskPoolPlugin};
use bevy::ecs::entity::Entity;
use bevy::ecs::message::Messages;
use bevy::time::{Fixed, Real, Time, Virtual};
use bevy::transform::components::Transform;

use crate::{BodyId, BodyKind, BodySnapshot, BodySpec, ContactPair, PhysicsBackend, Shape, Vec2};

pub struct AvianBackend {
    app: App,
    bodies: Vec<Option<Entity>>,
    collider_owner: HashMap<Entity, BodyId>,
    next_id: BodyId,
}

impl AvianBackend {
    pub fn new() -> AvianBackend {
        let mut app = App::new();
        app.add_plugins((TaskPoolPlugin::default(), PhysicsPlugins::new(PostUpdate)));
        app.insert_resource(Time::<()>::default());
        app.insert_resource(Time::<Real>::default());
        app.insert_resource(Time::<Virtual>::default());
        app.insert_resource(Time::<Fixed>::default());
        app.insert_resource(Gravity(Vec2::new(0.0, -9.81)));
        app.finish();
        AvianBackend {
            app,
            bodies: Vec::new(),
            collider_owner: HashMap::new(),
            next_id: 0,
        }
    }

    fn body_entity(&self, id: BodyId) -> Option<Entity> {
        self.bodies.get(id as usize).copied().flatten()
    }
}

impl Default for AvianBackend {
    fn default() -> Self {
        AvianBackend::new()
    }
}

impl PhysicsBackend for AvianBackend {
    fn name(&self) -> &'static str {
        "avian"
    }

    fn add_body(&mut self, spec: &BodySpec) -> BodyId {
        let kind = match spec.kind {
            BodyKind::Static => RigidBody::Static,
            BodyKind::Dynamic => RigidBody::Dynamic,
            BodyKind::Kinematic => RigidBody::Kinematic,
        };
        let id = self.next_id;
        self.next_id += 1;
        debug_assert_eq!(self.bodies.len(), id as usize);
        let body_entity = self
            .app
            .world_mut()
            .spawn((
                kind,
                Position(spec.translation),
                Rotation {
                    cos: spec.rotation.cos(),
                    sin: spec.rotation.sin(),
                },
                LinearVelocity(spec.linear_velocity),
                AngularVelocity(0.0),
                CollisionEventsEnabled,
            ))
            .id();
        self.bodies.push(Some(body_entity));
        self.collider_owner.insert(body_entity, id);

        let single_zero = spec.colliders.len() == 1 && spec.colliders[0].offset == Vec2::ZERO;
        if single_zero {
            let c = &spec.colliders[0];
            self.app.world_mut().entity_mut(body_entity).insert((
                collider_of(&c.shape),
                Friction::new(c.friction),
                Restitution::new(c.restitution),
                ColliderDensity(c.density),
            ));
        } else {
            let children: Vec<Entity> = spec
                .colliders
                .iter()
                .map(|c| {
                    self.app
                        .world_mut()
                        .spawn((
                            collider_of(&c.shape),
                            Friction::new(c.friction),
                            Restitution::new(c.restitution),
                            ColliderDensity(c.density),
                            Transform::from_xyz(c.offset.x, c.offset.y, 0.0),
                            CollisionEventsEnabled,
                        ))
                        .id()
                })
                .collect();
            for child in children {
                self.app
                    .world_mut()
                    .entity_mut(body_entity)
                    .add_child(child);
                self.collider_owner.insert(child, id);
            }
        }
        id
    }

    fn remove_body(&mut self, id: BodyId) {
        if let Some(Some(entity)) = self.bodies.get(id as usize) {
            let entity = *entity;
            self.app.world_mut().despawn(entity);
        }
        if let Some(slot) = self.bodies.get_mut(id as usize) {
            *slot = None;
        }
    }

    fn set_body_velocity(&mut self, id: BodyId, velocity: Vec2) {
        if let Some(Some(entity)) = self.bodies.get(id as usize) {
            let entity = *entity;
            if let Some(mut vel) = self.app.world_mut().get_mut::<LinearVelocity>(entity) {
                vel.0 = velocity;
            }
        }
    }

    fn snapshot(&self, id: BodyId) -> BodySnapshot {
        if let Some(entity) = self.body_entity(id) {
            let world = self.app.world();
            let pos = world
                .get::<Position>(entity)
                .map(|p| p.0)
                .unwrap_or(Vec2::ZERO);
            let rot = world
                .get::<Rotation>(entity)
                .map(|r| r.sin.atan2(r.cos))
                .unwrap_or(0.0);
            let lin = world
                .get::<LinearVelocity>(entity)
                .map(|v| v.0)
                .unwrap_or(Vec2::ZERO);
            let ang = world
                .get::<AngularVelocity>(entity)
                .map(|a| a.0)
                .unwrap_or(0.0);
            BodySnapshot {
                translation: pos,
                rotation: rot,
                linear_velocity: lin,
                angular_velocity: ang,
                sleeping: world.get::<Sleeping>(entity).is_some(),
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

    fn step(&mut self, dt: f32) -> Vec<ContactPair> {
        self.app
            .world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs_f32(dt));
        self.app.update();
        let world = self.app.world_mut();
        let mut out = Vec::new();
        let mut starts = world.resource_mut::<Messages<CollisionStart>>();
        for ev in starts.drain() {
            if let (Some(a), Some(b)) = (
                self.collider_owner.get(&ev.collider1).copied(),
                self.collider_owner.get(&ev.collider2).copied(),
            ) {
                out.push(ContactPair {
                    a,
                    b,
                    started: true,
                });
            }
        }
        let mut ends = world.resource_mut::<Messages<CollisionEnd>>();
        for ev in ends.drain() {
            if let (Some(a), Some(b)) = (
                self.collider_owner.get(&ev.collider1).copied(),
                self.collider_owner.get(&ev.collider2).copied(),
            ) {
                out.push(ContactPair {
                    a,
                    b,
                    started: false,
                });
            }
        }
        out
    }
}

fn collider_of(shape: &Shape) -> Collider {
    match shape {
        Shape::Disc { radius } => Collider::circle(*radius),
        Shape::Box { half } => Collider::rectangle(half.x * 2.0, half.y * 2.0),
        Shape::Ngon { radius, sides } => {
            let points: Vec<Vec2> = (0..*sides)
                .map(|i| {
                    let a = i as f32 * std::f32::consts::TAU / *sides as f32;
                    Vec2::new(radius * a.cos(), radius * a.sin())
                })
                .collect();
            Collider::convex_hull(&points).unwrap()
        }
    }
}
