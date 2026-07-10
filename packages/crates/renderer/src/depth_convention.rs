//! The renderer's depth convention — forward-Z (near→0, far→1) vs
//! REVERSE-Z (near→1, far→0) — as one value every producer reads
//! (docs/plans/003-reverse-z.md).
//!
//! Reverse-Z pairs the reversed depth distribution with float32's exponent
//! bunching near 0.0, cancelling perspective's far-field precision starvation
//! to near-uniform precision. Everything that touches depth derives from this
//! ONE value: projection builders, depth clears, compare directions, HZB
//! reduce ops, frustum-plane extraction, and background sentinels. Flipping a
//! subset silently over/under-culls or mis-renders — never hardcode a depth
//! constant in a main-camera path; read the convention.
//!
//! Shadows keep their OWN convention until the stage-7 lockstep migration
//! (writer + receiver + compare + clear move together) — shadow code uses
//! [`DepthConvention::FORWARD`] explicitly, not the feature flag.

use awsm_renderer_core::compare::CompareFunction;

/// The active depth convention. Copy — pass by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DepthConvention {
    /// `true` = reverse-Z (near→1, far→0, clear 0.0, GreaterEqual, background
    /// sentinel `depth <= 0.0`, "closer" = larger).
    pub reverse_z: bool,
}

impl DepthConvention {
    /// The classic forward-Z convention (near→0/far→1). Shadow paths pin this
    /// until their stage-7 lockstep migration.
    pub const FORWARD: Self = Self { reverse_z: false };

    /// Depth-buffer clear value = the FARTHEST depth (background sentinel).
    pub fn clear_value(self) -> f32 {
        if self.reverse_z {
            0.0
        } else {
            1.0
        }
    }

    /// Depth test for "closer or equal wins" (the standard opaque test).
    pub fn compare(self) -> CompareFunction {
        if self.reverse_z {
            CompareFunction::GreaterEqual
        } else {
            CompareFunction::LessEqual
        }
    }

    /// Strict variant of [`Self::compare`] (the few `Less`/`Greater` sites).
    pub fn compare_strict(self) -> CompareFunction {
        if self.reverse_z {
            CompareFunction::Greater
        } else {
            CompareFunction::Less
        }
    }

    /// Whether `depth` is the background/sky sentinel (carries the clear
    /// value). WGSL consumers get the equivalent branch via their
    /// `reverse_z` template axis; keep the two in lockstep.
    pub fn is_background(self, depth: f32) -> bool {
        if self.reverse_z {
            depth <= 0.0
        } else {
            depth >= 1.0
        }
    }

    /// The NEAREST possible depth value ("closest" extreme) — reverse of the
    /// clear value. HZB/min-max reductions initialize "find the nearest"
    /// scans from the FARTHEST ([`Self::clear_value`]) and "find the
    /// farthest" scans from this.
    pub fn nearest_value(self) -> f32 {
        if self.reverse_z {
            1.0
        } else {
            0.0
        }
    }

    /// `true` when depth `a` is closer to the camera than `b`.
    pub fn is_closer(self, a: f32, b: f32) -> bool {
        if self.reverse_z {
            a > b
        } else {
            a < b
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_and_reverse_are_exact_mirrors() {
        let f = DepthConvention { reverse_z: false };
        let r = DepthConvention { reverse_z: true };
        assert_eq!(f.clear_value(), 1.0);
        assert_eq!(r.clear_value(), 0.0);
        assert_eq!(f.compare(), CompareFunction::LessEqual);
        assert_eq!(r.compare(), CompareFunction::GreaterEqual);
        assert_eq!(f.compare_strict(), CompareFunction::Less);
        assert_eq!(r.compare_strict(), CompareFunction::Greater);
        assert!(f.is_background(1.0) && !f.is_background(0.5));
        assert!(r.is_background(0.0) && !r.is_background(0.5));
        assert!(f.is_closer(0.1, 0.9) && !f.is_closer(0.9, 0.1));
        assert!(r.is_closer(0.9, 0.1) && !r.is_closer(0.1, 0.9));
        assert_eq!(f.nearest_value(), 0.0);
        assert_eq!(r.nearest_value(), 1.0);
    }
}
