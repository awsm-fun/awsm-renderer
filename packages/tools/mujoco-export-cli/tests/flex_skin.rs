//! Does linear-blend skinning reproduce MuJoCo's flex interpolation?
//!
//! An interpolated flex drives thousands of surface vertices from a small cage of
//! nodes, with weights fixed at rest — which is the definition of linear blend
//! skinning. If that equivalence holds numerically, a flex can be imported as an
//! ordinary skinned mesh and driven by the bodies its cage rides, with no
//! per-frame vertex upload anywhere in the renderer.
//!
//! This test is the evidence for that claim. It derives the weights offline, runs
//! the real simulation until the body has visibly deformed, and measures the
//! reconstruction against MuJoCo's own `flexvert_xpos`.

use std::path::PathBuf;

use awsm_renderer_mujoco_sys::{Data, Library, Model};

fn model(rel: &str) -> Option<(Library, PathBuf)> {
    let path = PathBuf::from(std::env::var_os("MUJOCO_DIR")?)
        .join("model/flex")
        .join(rel);
    if !path.exists() {
        eprintln!("SKIP: no model at {}", path.display());
        return None;
    }
    let lib = Library::load().expect("MUJOCO_DIR is set, so the library must load");
    Some((lib, path))
}

/// The eight trilinear weights for a vertex whose normalized cage coordinates
/// are `t`, indexed by our own corner-bit convention (bit `k` = axis `k`).
///
/// `t` is read verbatim from `flex_vert0` — MuJoCo already stores each vertex's
/// position inside its cell as normalized coordinates, so there is no cell
/// search and no geometry to get wrong here.
fn trilinear_weights(t: [f64; 3]) -> [f64; 8] {
    let mut w = [0.0; 8];
    for (n, slot) in w.iter_mut().enumerate() {
        *slot = [0, 1, 2]
            .map(|k| if n >> k & 1 == 1 { t[k] } else { 1.0 - t[k] })
            .iter()
            .product();
    }
    w
}

/// The cage's rest bounding box, and the node index at each corner.
///
/// MuJoCo does not promise a corner ordering, so it is *derived* from the rest
/// positions rather than assumed — an assumed order would produce a mesh that
/// looks plausible at rest and turns inside out the moment it moves.
fn cage(model: &Model<'_>, f: usize) -> [usize; 8] {
    let adr = model.flex_nodeadr()[f] as usize;
    let num = model.flex_nodenum()[f] as usize;
    let node0 = model.flex_node0();
    let at = |n: usize| {
        let o = (adr + n) * 3;
        [node0[o], node0[o + 1], node0[o + 2]]
    };

    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for n in 0..num {
        let p = at(n);
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }

    let mut corner = [usize::MAX; 8];
    for n in 0..num {
        let p = at(n);
        let mut bits = 0usize;
        for k in 0..3 {
            let mid = (lo[k] + hi[k]) * 0.5;
            if p[k] > mid {
                bits |= 1 << k;
            }
        }
        corner[bits] = n;
    }
    assert!(
        corner.iter().all(|c| *c != usize::MAX),
        "the eight cage corners must be distinct — got {corner:?}"
    );
    corner
}

/// A cage node's world position: its body's frame applied to its rest offset.
fn node_world(model: &Model<'_>, data: &Data<'_, '_>, f: usize, n: usize) -> [f64; 3] {
    let adr = model.flex_nodeadr()[f] as usize;
    let body = model.flex_nodebodyid()[adr + n] as usize;
    let o = (adr + n) * 3;
    let node = model.flex_node();
    let local = [node[o], node[o + 1], node[o + 2]];

    let xpos = data.xpos();
    let q = &data.xquat()[body * 4..body * 4 + 4];
    let rot = glam::DQuat::from_xyzw(q[1], q[2], q[3], q[0]);
    let world = rot * glam::DVec3::from_array(local)
        + glam::DVec3::new(xpos[body * 3], xpos[body * 3 + 1], xpos[body * 3 + 2]);
    world.to_array()
}

#[test]
fn skinning_reproduces_a_trilinear_flex() {
    let Some((lib, path)) = model("bunny.xml") else {
        return;
    };
    let m = lib.load_model(&path).expect("bunny should compile");
    assert_eq!(m.nflex(), 1);
    let f = 0;
    assert_eq!(m.flex_interp()[f], 1, "bunny must be an interpolated flex");
    assert_eq!(m.flex_nodenum()[f], 8, "one trilinear cell");

    let vertadr = m.flex_vertadr()[f] as usize;
    let vertnum = m.flex_vertnum()[f] as usize;
    let corner = cage(&m, f);

    // Weights are formed ONCE, from coordinates MuJoCo already computed. If the
    // equivalence holds they never need recomputing — that is the whole point.
    let vert0 = m.flex_vert0();
    let weights: Vec<[f64; 8]> = (0..vertnum)
        .map(|v| {
            let o = (vertadr + v) * 3;
            trilinear_weights([vert0[o], vert0[o + 1], vert0[o + 2]])
        })
        .collect();

    for w in &weights {
        let sum: f64 = w.iter().sum();
        assert!(
            (sum - 1.0).abs() < 1e-9,
            "trilinear weights must partition unity, got {sum}"
        );
    }

    let mut data = m.forward_at_initial_pose().expect("data");

    // Measure at rest AND after the bunny has actually deformed — a weight set
    // can be exactly right at the bind pose and wrong everywhere else.
    for (label, steps) in [("rest", 0), ("settled", 400), ("late", 1200)] {
        for _ in 0..steps {
            data.step();
        }
        let cage_world: Vec<[f64; 3]> = (0..8).map(|n| node_world(&m, &data, f, n)).collect();

        let truth = data.flexvert_xpos();
        let mut max_err = 0.0f64;
        let mut sum_err = 0.0f64;
        let mut span = 0.0f64;
        for v in 0..vertnum {
            let w = &weights[v];
            let mut predicted = [0.0f64; 3];
            for (n, slot) in corner.iter().enumerate() {
                let p = cage_world[*slot];
                for k in 0..3 {
                    predicted[k] += w[n] * p[k];
                }
            }
            let o = (vertadr + v) * 3;
            let actual = [truth[o], truth[o + 1], truth[o + 2]];
            let err = (0..3)
                .map(|k| (predicted[k] - actual[k]).powi(2))
                .sum::<f64>()
                .sqrt();
            max_err = max_err.max(err);
            sum_err += err;
            span = span.max(
                (0..3)
                    .map(|k| actual[k].abs())
                    .fold(0.0f64, |a: f64, b| a.max(b)),
            );
        }
        let mean = sum_err / vertnum as f64;
        println!(
            "{label:>8}: max {:.3} nm, mean {:.3} nm over {vertnum} verts (model spans ~{:.2} m)",
            max_err * 1.0e9,
            mean * 1.0e9,
            span
        );
        // A NANOMETRE. Not "close enough to look right" — the two formulations
        // are the same arithmetic, so anything above float noise would mean a
        // wrong weight, a wrong corner, or node rotations mattering after all.
        assert!(
            max_err < 1.0e-9,
            "{label}: skinning must reproduce the flex exactly, got {:.3} nm",
            max_err * 1.0e9
        );
    }
}

#[test]
fn a_body_attached_flex_is_a_one_influence_skin() {
    // The other half of the claim: a non-interpolated flex is the degenerate
    // case, one joint per vertex at weight 1, so ONE mechanism covers both.
    let Some((lib, path)) = model("flag.xml") else {
        return;
    };
    let m = lib.load_model(&path).expect("flag should compile");
    let f = 0;
    assert_eq!(m.flex_interp()[f], 0);
    assert_eq!(m.flex_nodenum()[f], 0, "no cage — vertices ride bodies");

    let vertadr = m.flex_vertadr()[f] as usize;
    let vertnum = m.flex_vertnum()[f] as usize;
    let mut data = m.forward_at_initial_pose().expect("data");
    for _ in 0..300 {
        data.step();
    }

    let vert = m.flex_vert();
    let xpos = data.xpos();
    let truth = data.flexvert_xpos();
    let mut max_err = 0.0f64;
    for v in 0..vertnum {
        let body = m.flex_vertbodyid()[vertadr + v] as usize;
        let o = (vertadr + v) * 3;
        let q = &data.xquat()[body * 4..body * 4 + 4];
        let rot = glam::DQuat::from_xyzw(q[1], q[2], q[3], q[0]);
        let predicted = rot * glam::DVec3::new(vert[o], vert[o + 1], vert[o + 2])
            + glam::DVec3::new(xpos[body * 3], xpos[body * 3 + 1], xpos[body * 3 + 2]);
        let actual = glam::DVec3::new(truth[o], truth[o + 1], truth[o + 2]);
        max_err = max_err.max((predicted - actual).length());
    }
    println!(
        "body-attached: max {:.3} nm over {vertnum} verts",
        max_err * 1.0e9
    );
    assert!(
        max_err < 1.0e-9,
        "a body-attached vertex is its body's frame applied to its rest offset, \
         got {:.3} nm",
        max_err * 1.0e9
    );
}
