//! Query filters applied to leaves during spatial queries.

use super::node::SceneNode;

/// Predicate filter applied to leaves during a frustum query. Each call
/// site fills this in once and the iterator handles the per-leaf check.
#[derive(Debug, Clone, Copy, Default)]
pub struct NodeFilter {
    pub exclude_hidden: bool,
    pub exclude_hud: bool,
    pub require_cast_shadows: bool,
    pub require_receive_shadows: bool,
}

impl NodeFilter {
    /// Default for the camera (geometry) pass: skip hidden, keep HUD.
    pub fn camera_default() -> Self {
        Self {
            exclude_hidden: true,
            exclude_hud: false,
            require_cast_shadows: false,
            require_receive_shadows: false,
        }
    }

    /// Filter for the shadow-caster pass: cast_shadows && !hidden && !hud.
    pub fn shadow_caster() -> Self {
        Self {
            exclude_hidden: true,
            exclude_hud: true,
            require_cast_shadows: true,
            require_receive_shadows: false,
        }
    }

    /// All-active filter: !hidden.
    pub fn visible() -> Self {
        Self {
            exclude_hidden: true,
            exclude_hud: false,
            require_cast_shadows: false,
            require_receive_shadows: false,
        }
    }

    /// Returns true if the node passes the filter.
    pub fn matches(&self, node: &SceneNode) -> bool {
        if self.exclude_hidden && node.flags.hidden {
            return false;
        }
        if self.exclude_hud && node.flags.hud {
            return false;
        }
        if self.require_cast_shadows && !node.flags.cast_shadows {
            return false;
        }
        if self.require_receive_shadows && !node.flags.receive_shadows {
            return false;
        }
        true
    }
}
