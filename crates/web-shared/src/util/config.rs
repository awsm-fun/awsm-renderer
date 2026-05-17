#[macro_export]
macro_rules! required_build_env {
    ($key:literal $(,)?) => {{
        match option_env!($key) {
            Some(value) => value,
            None => panic!(concat!($key, " must be set by task/trunk build")),
        }
    }};
}

pub use crate::required_build_env;
