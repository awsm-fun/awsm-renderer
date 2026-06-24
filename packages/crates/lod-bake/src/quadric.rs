//! Symmetric 4×4 error quadric (Garland–Heckbert QEM), stored as its 10 unique
//! upper-triangular entries. All math is `f64` for numerical stability across
//! many accumulated planes; callers cast the final error to `f32`.

use glam::DVec3;

/// A symmetric 4×4 quadric `Q` such that `error(v) = [v 1]ᵀ Q [v 1]`.
///
/// Layout (row-major upper triangle):
/// ```text
/// [ a  b  c  d ]
/// [ b  e  f  g ]
/// [ c  f  h  i ]
/// [ d  g  i  j ]
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Quadric {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
    g: f64,
    h: f64,
    i: f64,
    j: f64,
}

impl Quadric {
    /// The fundamental error quadric of the plane `n·x + off = 0` (with `n`
    /// unit-length), scaled by `weight` (use triangle area to area-weight).
    pub fn from_plane(n: DVec3, off: f64, weight: f64) -> Self {
        let (x, y, z, w) = (n.x, n.y, n.z, off);
        Quadric {
            a: x * x * weight,
            b: x * y * weight,
            c: x * z * weight,
            d: x * w * weight,
            e: y * y * weight,
            f: y * z * weight,
            g: y * w * weight,
            h: z * z * weight,
            i: z * w * weight,
            j: w * w * weight,
        }
    }

    /// Accumulate another quadric in place (`Q += other`).
    pub fn add_assign(&mut self, o: &Quadric) {
        self.a += o.a;
        self.b += o.b;
        self.c += o.c;
        self.d += o.d;
        self.e += o.e;
        self.f += o.f;
        self.g += o.g;
        self.h += o.h;
        self.i += o.i;
        self.j += o.j;
    }

    /// Evaluate `[v 1]ᵀ Q [v 1]` — the squared distance error of placing the
    /// merged surface at point `v`. Clamped to `>= 0` (tiny negatives from
    /// float cancellation are meaningless).
    pub fn error_at(&self, v: DVec3) -> f64 {
        let (x, y, z) = (v.x, v.y, v.z);
        // [v 1]ᵀ Q [v 1] expanded from the symmetric entries.
        let err = self.a * x * x
            + 2.0 * self.b * x * y
            + 2.0 * self.c * x * z
            + 2.0 * self.d * x
            + self.e * y * y
            + 2.0 * self.f * y * z
            + 2.0 * self.g * y
            + self.h * z * z
            + 2.0 * self.i * z
            + self.j;
        err.max(0.0)
    }
}

/// Build the area-weighted fundamental quadric of a triangle and return it
/// alongside the triangle's geometric normal magnitude (`2·area`). Returns
/// `None` for a degenerate (zero-area) triangle.
pub fn triangle_quadric(p0: DVec3, p1: DVec3, p2: DVec3) -> Option<(Quadric, f64)> {
    let cross = (p1 - p0).cross(p2 - p0);
    let len = cross.length();
    if len <= f64::EPSILON {
        return None;
    }
    let n = cross / len;
    let off = -n.dot(p0);
    let area2 = len; // 2·area; used as the quadric weight.
    Some((Quadric::from_plane(n, off, area2), area2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_on_plane_has_zero_error() {
        // Plane z = 0; a point on it has zero error, a point above it has
        // error == height² (× weight 1).
        let q = Quadric::from_plane(DVec3::Z, 0.0, 1.0);
        assert!(q.error_at(DVec3::new(3.0, -2.0, 0.0)).abs() < 1e-12);
        let e = q.error_at(DVec3::new(0.0, 0.0, 2.0));
        assert!((e - 4.0).abs() < 1e-9, "expected 4.0, got {e}");
    }

    #[test]
    fn summed_planes_accumulate() {
        let mut q = Quadric::from_plane(DVec3::Z, 0.0, 1.0);
        q.add_assign(&Quadric::from_plane(DVec3::X, 0.0, 1.0));
        // Point (1,0,1): distance² to z=0 is 1, to x=0 is 1 → total 2.
        let e = q.error_at(DVec3::new(1.0, 5.0, 1.0));
        assert!((e - 2.0).abs() < 1e-9, "expected 2.0, got {e}");
    }
}
