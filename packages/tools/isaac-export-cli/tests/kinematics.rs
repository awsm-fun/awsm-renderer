//! Forward kinematics over UsdPhysics joints, and the scripted-motion capture.

use std::path::{Path, PathBuf};

use awsm_renderer_isaac_export_cli::kinematics::{geom_poses, record, JointKind, Motion, Sweep};
use awsm_renderer_isaac_export_cli::{export, Export, Options};
use glam::{DMat4, DQuat, DVec3};

fn fixture() -> Export {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/robot.usda");
    export(&p, &Options::default()).unwrap()
}

fn close(a: DMat4, b: DMat4, eps: f64) -> bool {
    a.abs_diff_eq(b, eps)
}

const MOTION: Motion = Motion {
    seconds: 4.0,
    fps: 30.0,
    amplitude: 0.5,
    cycles: 2,
};

#[test]
fn the_rest_position_is_solved_from_the_authored_pose() {
    let out = fixture();
    let arm = &out.articulation;
    assert_eq!(arm.joints.len(), 1, "{:?}", arm.notes);
    let hinge = &arm.joints[0];
    assert_eq!(hinge.name, "hinge");
    assert!(matches!(hinge.kind, JointKind::Revolute { .. }));
    assert!(
        (hinge.rest - std::f64::consts::FRAC_PI_2).abs() < 1e-6,
        "{}",
        hinge.rest
    );
    assert!(
        hinge.residual.0 < 1e-9 && hinge.residual.1 < 1e-6,
        "{:?}",
        hinge.residual
    );
    // At rest, FK IS the authored pose.
    let world = arm.forward(&arm.rest_positions());
    for (a, b) in world.iter().zip(&arm.rest) {
        assert!(close(*a, *b, 1e-9));
    }
}

#[test]
fn moving_a_joint_moves_its_child_about_the_joint() {
    let out = fixture();
    let arm = &out.articulation;
    let child = arm.joints[0].child;
    // q = 0 undoes the authored 90°: the arm faces +X, still on the anchor.
    let world = arm.forward(&[0.0]);
    let (_, r, p) = world[child].to_scale_rotation_translation();
    assert!(r.abs_diff_eq(DQuat::IDENTITY, 1e-9), "{r:?}");
    assert!(p.abs_diff_eq(DVec3::new(1.0, 0.0, 1.0), 1e-9), "{p:?}");
    // The base is not driven by anything and stays put.
    let base = out
        .sidecar
        .bodies
        .iter()
        .position(|b| b.name.as_deref() == Some("base"))
        .unwrap();
    assert!(close(world[base], arm.rest[base], 1e-12));
}

#[test]
fn a_recorded_sweep_starts_at_the_imported_pose_loops_and_respects_limits() {
    let out = fixture();
    let arm = &out.articulation;
    let capture = record(&out.sidecar, arm, &MOTION);
    assert_eq!(capture.frames.len(), 120);
    assert_eq!(capture.geom_count as usize, out.sidecar.geoms.len());
    assert_eq!(
        capture.source, out.sidecar.source,
        "same fingerprint as the sidecar"
    );

    // Frame 0 = the sidecar's own world poses: playback starts without a jump.
    let first = &capture.frames[0].geom_poses;
    for (g, pose) in out.sidecar.geoms.iter().zip(first.chunks(7)) {
        let want = [g.world_pos[0], g.world_pos[1], g.world_pos[2]];
        for k in 0..3 {
            assert!(
                (pose[k] as f64 - want[k]).abs() < 1e-5,
                "{:?}: {pose:?}",
                g.name
            );
        }
        let q = DQuat::from_xyzw(
            pose[4] as f64,
            pose[5] as f64,
            pose[6] as f64,
            pose[3] as f64,
        );
        let w = DQuat::from_xyzw(
            g.world_quat[1],
            g.world_quat[2],
            g.world_quat[3],
            g.world_quat[0],
        );
        assert!(q.dot(w).abs() > 1.0 - 1e-6, "{:?}", g.name);
    }

    // One loop later the joints are back at rest: the capture loops seamlessly.
    let sweep = Sweep::new(arm, MOTION.amplitude);
    let omega = std::f64::consts::TAU * MOTION.cycles as f64 / MOTION.seconds;
    let end = sweep.at(omega * MOTION.seconds);
    for (q, j) in end.iter().zip(&arm.joints) {
        assert!((q - j.rest).abs() < 1e-9);
    }
    // And it moves, within the limits.
    let mut moved = 0.0f64;
    for i in 0..120 {
        let q = sweep.at(omega * i as f64 / MOTION.fps)[0];
        let (lo, hi) = arm.joints[0].limits.unwrap();
        assert!(q >= lo - 1e-12 && q <= hi + 1e-12);
        moved = moved.max((q - arm.joints[0].rest).abs());
    }
    assert!(moved > 0.1, "the hinge swings ({moved} rad)");
}

#[test]
fn geom_poses_follow_their_bodies() {
    let out = fixture();
    let arm = &out.articulation;
    let world = arm.forward(&[0.0]);
    let poses = geom_poses(&out.sidecar, &world);
    // The arm's baked quad rides the arm: its geom rotation is the arm's.
    let (i, _) = out
        .sidecar
        .geoms
        .iter()
        .enumerate()
        .find(|(_, g)| g.name.as_deref() == Some("quad (Blue)"))
        .unwrap();
    let q = &poses[i * 7 + 3..i * 7 + 7];
    assert!((q[0].abs() - 1.0).abs() < 1e-6, "{q:?}");
}

/// The real robots: every moving joint's frames agree with the authored
/// bodies, FK at rest reproduces the export, and the movable joint counts are
/// right (Franka: 7 revolute + 2 prismatic fingers). Needs the assets (see
/// tests/export.rs).
#[test]
#[ignore = "needs the Isaac assets downloaded locally"]
fn real_isaac_joints_are_consistent() {
    let mut ran = 0;
    for (var, movable) in [("AWSM_ISAAC_FRANKA", 9), ("AWSM_ISAAC_ANYMAL_D", 12)] {
        let Ok(p) = std::env::var(var) else { continue };
        let out = export(&PathBuf::from(p), &Options::default()).unwrap();
        let arm = &out.articulation;
        assert_eq!(
            arm.joints.iter().filter(|j| j.is_movable()).count(),
            movable
        );
        // Every MOVING joint's frames agree with the bodies it moves. ANYmal's
        // fixed foot joints do not (3 cm): those are reported, pose kept.
        for j in arm.joints.iter().filter(|j| j.is_movable()) {
            assert!(
                j.residual.0 < 1e-4 && j.residual.1 < 1e-4,
                "{}: {:?}",
                j.name,
                j.residual
            );
        }
        for n in &arm.notes {
            assert!(n.contains("the authored pose is kept"), "{n}");
        }
        // FK at rest IS the export.
        let world = arm.forward(&arm.rest_positions());
        for (a, b) in world.iter().zip(&arm.rest) {
            assert!(close(*a, *b, 1e-9));
        }
        ran += 1;
    }
    assert!(ran > 0, "set AWSM_ISAAC_FRANKA and/or AWSM_ISAAC_ANYMAL_D");
}
