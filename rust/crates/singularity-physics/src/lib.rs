pub mod backends;
pub mod scenes;

pub use backends::avian::AvianBackend;
pub use backends::rapier::RapierBackend;

pub use bevy::math::Vec2;

pub const DT: f32 = 1.0 / 60.0;

pub type BodyId = u32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BodyKind {
    Static,
    Dynamic,
    Kinematic,
}

#[derive(Clone, Copy, Debug)]
pub enum Shape {
    Disc { radius: f32 },
    Box { half: Vec2 },
    Ngon { radius: f32, sides: u32 },
}

#[derive(Clone, Copy, Debug)]
pub struct ColliderSpec {
    pub shape: Shape,
    pub offset: Vec2,
    pub friction: f32,
    pub restitution: f32,
    pub density: f32,
}

impl ColliderSpec {
    pub fn disc(radius: f32) -> ColliderSpec {
        ColliderSpec {
            shape: Shape::Disc { radius },
            offset: Vec2::ZERO,
            friction: 0.6,
            restitution: 0.0,
            density: 1.0,
        }
    }

    pub fn box_half(half: Vec2) -> ColliderSpec {
        ColliderSpec {
            shape: Shape::Box { half },
            offset: Vec2::ZERO,
            friction: 0.6,
            restitution: 0.0,
            density: 1.0,
        }
    }

    pub fn ngon(radius: f32, sides: u32) -> ColliderSpec {
        ColliderSpec {
            shape: Shape::Ngon { radius, sides },
            offset: Vec2::ZERO,
            friction: 0.6,
            restitution: 0.0,
            density: 1.0,
        }
    }

    pub fn at(mut self, offset: Vec2) -> ColliderSpec {
        self.offset = offset;
        self
    }

    pub fn with_friction(mut self, friction: f32) -> ColliderSpec {
        self.friction = friction;
        self
    }
}

#[derive(Clone, Debug)]
pub struct BodySpec {
    pub kind: BodyKind,
    pub translation: Vec2,
    pub rotation: f32,
    pub linear_velocity: Vec2,
    pub colliders: Vec<ColliderSpec>,
}

impl BodySpec {
    pub fn new(kind: BodyKind) -> BodySpec {
        BodySpec {
            kind,
            translation: Vec2::ZERO,
            rotation: 0.0,
            linear_velocity: Vec2::ZERO,
            colliders: Vec::new(),
        }
    }

    pub fn at(mut self, translation: Vec2) -> BodySpec {
        self.translation = translation;
        self
    }

    pub fn vel(mut self, velocity: Vec2) -> BodySpec {
        self.linear_velocity = velocity;
        self
    }

    pub fn with(mut self, collider: ColliderSpec) -> BodySpec {
        self.colliders.push(collider);
        self
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BodySnapshot {
    pub translation: Vec2,
    pub rotation: f32,
    pub linear_velocity: Vec2,
    pub angular_velocity: f32,
    pub sleeping: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct ContactPair {
    pub a: BodyId,
    pub b: BodyId,
    pub started: bool,
}

pub trait PhysicsBackend {
    fn name(&self) -> &'static str;
    fn add_body(&mut self, spec: &BodySpec) -> BodyId;
    fn remove_body(&mut self, id: BodyId);
    fn set_body_velocity(&mut self, id: BodyId, velocity: Vec2);
    fn snapshot(&self, id: BodyId) -> BodySnapshot;
    fn step(&mut self, dt: f32) -> Vec<ContactPair>;
}
