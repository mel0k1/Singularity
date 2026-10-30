use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::time::Instant;

use crate::{BodyId, BodyKind, BodySpec, ColliderSpec, DT, PhysicsBackend, Vec2};

#[derive(Clone, Copy, PartialEq)]
pub enum SceneKind {
    Floor,
    Rain,
    Mover,
}

pub const RAIN_SPAWNS: u32 = 300;
pub const MOVER_SPEED: f32 = 1.5;

impl SceneKind {
    pub const ALL: [SceneKind; 3] = [SceneKind::Floor, SceneKind::Rain, SceneKind::Mover];

    pub fn name(&self) -> &'static str {
        match self {
            SceneKind::Floor => "floor",
            SceneKind::Rain => "rain",
            SceneKind::Mover => "mover",
        }
    }

    pub fn ticks(&self) -> u32 {
        match self {
            SceneKind::Floor => 600,
            SceneKind::Rain => 600,
            SceneKind::Mover => 600,
        }
    }

    fn seed(&self) -> u64 {
        match self {
            SceneKind::Floor => 1011,
            SceneKind::Rain => 2022,
            SceneKind::Mover => 3033,
        }
    }
}

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    pub fn next_f32(&mut self, lo: f32, hi: f32) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        let unit = (self.0 >> 40) as f32 / 16_777_216.0;
        lo + unit * (hi - lo)
    }
}

pub struct Outcome {
    pub scene: &'static str,
    pub backend: &'static str,
    pub ticks: u32,
    pub bodies: u32,
    pub dynamics: u32,
    pub sleeping: u32,
    pub contacts_started: u64,
    pub state_hash: u64,
    pub min_y: f32,
    pub cargo_dx: f32,
    pub tick_us: Vec<f64>,
}

pub fn run(
    backend: &mut dyn PhysicsBackend,
    scene: SceneKind,
    tick_override: Option<u32>,
) -> Outcome {
    let ticks = tick_override.unwrap_or_else(|| scene.ticks());
    let mut rng = Rng::new(scene.seed());
    let mut ids: Vec<BodyId> = Vec::new();
    let mut dyn_ids: Vec<BodyId> = Vec::new();
    let mut cargo: Vec<BodyId> = Vec::new();
    let mut cargo_x0: Vec<f32> = Vec::new();

    let platform_id = match scene {
        SceneKind::Floor => {
            let platform_id = platform(backend, 64, 8, 0.0, BodyKind::Static);
            let wall_l = add_wall(backend, -32.75);
            let wall_r = add_wall(backend, 32.75);
            for row in 0..12 {
                let count = 12 - row;
                for i in 0..count {
                    let x = (i as f32 - (count - 1) as f32 / 2.0) * 0.92;
                    let y = 0.45 + row as f32 * 0.9;
                    let id = backend.add_body(
                        &BodySpec::new(BodyKind::Dynamic)
                            .at(Vec2::new(x, y))
                            .with(ColliderSpec::box_half(Vec2::new(0.45, 0.45)).with_friction(0.8)),
                    );
                    ids.push(id);
                    dyn_ids.push(id);
                }
            }
            for _ in 0..60 {
                let x = rng.next_f32(-30.0, 30.0);
                let y = rng.next_f32(4.0, 30.0);
                let id = backend.add_body(
                    &BodySpec::new(BodyKind::Dynamic)
                        .at(Vec2::new(x, y))
                        .with(ColliderSpec::disc(0.35).with_friction(0.6)),
                );
                ids.push(id);
                dyn_ids.push(id);
            }
            ids.push(wall_l);
            ids.push(wall_r);
            platform_id
        }
        SceneKind::Rain => {
            let platform_id = platform(backend, 16, 8, 0.0, BodyKind::Static);
            ids.push(add_wall(backend, -8.75));
            ids.push(add_wall(backend, 8.75));
            platform_id
        }
        SceneKind::Mover => {
            let platform_id = platform(backend, 16, 2, 0.0, BodyKind::Kinematic);
            for row in 0..2 {
                for i in 0..16 {
                    let x = (i as f32 - 15.0 / 2.0) * 0.92;
                    let y = 0.45 + row as f32 * 0.9;
                    let id = backend.add_body(
                        &BodySpec::new(BodyKind::Dynamic)
                            .at(Vec2::new(x, y))
                            .with(ColliderSpec::box_half(Vec2::new(0.45, 0.45)).with_friction(0.9)),
                    );
                    ids.push(id);
                    dyn_ids.push(id);
                    cargo.push(id);
                    cargo_x0.push(x);
                }
            }
            platform_id
        }
    };
    ids.insert(0, platform_id);

    let mut contacts = 0u64;
    let mut tick_us = Vec::with_capacity(ticks as usize);
    for t in 0..ticks {
        if let SceneKind::Rain = scene
            && t < RAIN_SPAWNS
        {
            let x = rng.next_f32(-6.0, 6.0);
            let y = 25.0 + rng.next_f32(0.0, 4.0);
            let collider = if t % 2 == 0 {
                ColliderSpec::disc(0.4).with_friction(0.6)
            } else {
                ColliderSpec::box_half(Vec2::new(0.4, 0.4)).with_friction(0.6)
            };
            let id = backend.add_body(
                &BodySpec::new(BodyKind::Dynamic)
                    .at(Vec2::new(x, y))
                    .with(collider),
            );
            ids.push(id);
            dyn_ids.push(id);
        }
        if let SceneKind::Mover = scene
            && t == 0
        {
            backend.set_body_velocity(platform_id, Vec2::new(MOVER_SPEED, 0.0));
        }
        let start = Instant::now();
        let events = backend.step(DT);
        tick_us.push(start.elapsed().as_secs_f64() * 1e6);
        contacts += events.iter().filter(|e| e.started).count() as u64;
    }

    let mut hasher = DefaultHasher::new();
    let mut sleeping = 0u32;
    let mut min_y = f32::MAX;
    for id in &ids {
        let snap = backend.snapshot(*id);
        hasher.write_u32(*id);
        hasher.write_u32(snap.translation.x.to_bits());
        hasher.write_u32(snap.translation.y.to_bits());
        hasher.write_u32(snap.rotation.to_bits());
        hasher.write_u32(snap.linear_velocity.x.to_bits());
        hasher.write_u32(snap.linear_velocity.y.to_bits());
        hasher.write_u32(snap.angular_velocity.to_bits());
        hasher.write_u32(snap.sleeping as u32);
    }
    for id in &dyn_ids {
        let snap = backend.snapshot(*id);
        if snap.sleeping {
            sleeping += 1;
        }
        if snap.translation.y < min_y {
            min_y = snap.translation.y;
        }
    }

    let cargo_dx = if scene == SceneKind::Mover {
        let mut sum = 0.0f32;
        for (id, x0) in cargo.iter().zip(cargo_x0.iter()) {
            sum += backend.snapshot(*id).translation.x - x0;
        }
        sum / cargo.len() as f32
    } else {
        0.0
    };

    Outcome {
        scene: scene.name(),
        backend: backend.name(),
        ticks,
        bodies: ids.len() as u32,
        dynamics: dyn_ids.len() as u32,
        sleeping,
        contacts_started: contacts,
        state_hash: hasher.finish(),
        min_y,
        cargo_dx,
        tick_us,
    }
}

pub fn sanity(out: &Outcome) -> Result<(), String> {
    let mut problems: Vec<String> = Vec::new();
    if out.contacts_started == 0 {
        problems.push("no collision events observed".to_string());
    }
    if out.min_y < -0.5 {
        problems.push(format!(
            "body fell through the floor, min_y = {}",
            out.min_y
        ));
    }
    match out.scene {
        "mover" if out.cargo_dx < 1.0 => {
            problems.push(format!("cargo not carried, mean dx = {}", out.cargo_dx));
        }
        _ => {}
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{}:{}: {}",
            out.backend,
            out.scene,
            problems.join("; ")
        ))
    }
}

fn platform(backend: &mut dyn PhysicsBackend, w: u32, h: u32, top: f32, kind: BodyKind) -> BodyId {
    let mut spec = BodySpec::new(kind).at(Vec2::new(0.0, top - h as f32 / 2.0));
    for i in 0..w {
        for j in 0..h {
            let offset = Vec2::new(
                i as f32 + 0.5 - w as f32 / 2.0,
                j as f32 + 0.5 - h as f32 / 2.0,
            );
            spec = spec.with(
                ColliderSpec::box_half(Vec2::new(0.5, 0.5))
                    .at(offset)
                    .with_friction(0.9),
            );
        }
    }
    backend.add_body(&spec)
}

fn add_wall(backend: &mut dyn PhysicsBackend, x: f32) -> BodyId {
    let mut spec = BodySpec::new(BodyKind::Static).at(Vec2::new(x, 28.0));
    for j in 0..60 {
        spec = spec.with(
            ColliderSpec::box_half(Vec2::new(0.75, 0.5))
                .at(Vec2::new(0.0, j as f32 + 0.5 - 30.0))
                .with_friction(0.9),
        );
    }
    backend.add_body(&spec)
}
