use std::env;

use singularity_physics::scenes::{SceneKind, run, sanity};
use singularity_physics::{AvianBackend, RapierBackend};

fn main() {
    let args: Vec<String> = env::args().collect();
    let validate = args.iter().any(|a| a == "--validate");
    let ticks = args
        .iter()
        .find_map(|a| a.strip_prefix("--ticks="))
        .and_then(|v| v.parse::<u32>().ok());

    println!("singularity physics bench: rust backends headless");
    println!(
        "scenes {} | dt {:.5}s | ticks {}",
        SceneKind::ALL.len(),
        singularity_physics::DT,
        ticks.unwrap_or(600),
    );

    let mut failed = false;
    for scene in SceneKind::ALL {
        let rapier = {
            let mut backend = RapierBackend::new();
            run(&mut backend, scene, ticks)
        };
        report(&rapier);
        let avian = {
            let mut backend = AvianBackend::new();
            run(&mut backend, scene, ticks)
        };
        report(&avian);

        if validate {
            let pairs = [
                (
                    "rapier",
                    &rapier,
                    run(&mut RapierBackend::new(), scene, ticks),
                ),
                ("avian", &avian, run(&mut AvianBackend::new(), scene, ticks)),
            ];
            for (name, outcome, second) in pairs {
                let determinism = if outcome.state_hash == second.state_hash {
                    "identical"
                } else {
                    failed = true;
                    "diverged"
                };
                match sanity(outcome) {
                    Ok(()) => println!("{:>7} {:>6} {}", scene.name(), name, determinism),
                    Err(problems) => {
                        failed = true;
                        println!(
                            "{:>7} {:>6} {} {}",
                            scene.name(),
                            name,
                            determinism,
                            problems
                        );
                    }
                }
            }
        }
    }

    if failed {
        std::process::exit(2);
    }
}

fn report(outcome: &singularity_physics::scenes::Outcome) {
    let us = &outcome.tick_us;
    let max = us.iter().cloned().fold(0.0_f64, f64::max);
    let mean = us.iter().sum::<f64>() / us.len() as f64;
    println!(
        "{:>7} {:>6} bodies {:>3} asleep {:>3} contacts {:>5} p50 {:>7.0}us p95 {:>7.0}us max {:>7.0}us mean {:>7.0}us min_y {:>8.3} cargo_dx {:>7.3} hash {:016x}",
        outcome.scene,
        outcome.backend,
        outcome.bodies,
        outcome.sleeping,
        outcome.contacts_started,
        p(us, 0.50),
        p(us, 0.95),
        max,
        mean,
        outcome.min_y,
        outcome.cargo_dx,
        outcome.state_hash,
    );
}

fn p(values: &[f64], percentile: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let k = ((percentile * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[k - 1]
}
