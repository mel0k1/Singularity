use singularity_physics::scenes::{SceneKind, run, sanity};
use singularity_physics::{AvianBackend, RapierBackend};

#[test]
fn rapier_scenarios() {
    for scene in SceneKind::ALL {
        let mut a = RapierBackend::new();
        let out1 = run(&mut a, scene, Some(360));
        let mut b = RapierBackend::new();
        let out2 = run(&mut b, scene, Some(360));
        assert_eq!(
            out1.state_hash,
            out2.state_hash,
            "rapier {} nondeterministic",
            scene.name()
        );
        sanity(&out1).unwrap();
    }
}

#[test]
fn avian_scenarios() {
    for scene in SceneKind::ALL {
        let mut a = AvianBackend::new();
        let out1 = run(&mut a, scene, Some(360));
        let mut b = AvianBackend::new();
        let out2 = run(&mut b, scene, Some(360));
        assert_eq!(
            out1.state_hash,
            out2.state_hash,
            "avian {} nondeterministic",
            scene.name()
        );
        sanity(&out1).unwrap();
    }
}
