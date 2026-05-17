//! Generic web-frontend primitives (widgets, theme, utilities) used by
//! every awsm-renderer frontend. Lockstep-specific surface
//! (`lockstep-frontend-shared`) re-exports from here and adds its own
//! lockstep-flavored atoms / API client / branding on top.

pub mod atoms;
pub mod error;
pub mod logger;
pub mod prelude;
pub mod theme;
pub mod util;
