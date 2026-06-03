//! Prelude pulled in by every consumer module. Generic — pure
//! widget/theme/util surface plus the dominator + futures-signals +
//! wasm-bindgen helpers every UI file needs. Lockstep-specific
//! reexports live in `lockstep_frontend_shared::prelude`.

pub use crate::atoms::buttons::*;
pub use crate::atoms::checkbox::*;
pub use crate::atoms::dropdown::*;
pub use crate::atoms::file_picker::*;
pub use crate::atoms::icons::*;
pub use crate::atoms::label::*;
pub use crate::atoms::modal::*;
pub use crate::atoms::progress_bar::*;
pub use crate::atoms::secret_input::*;
pub use crate::atoms::segmented::*;
pub use crate::atoms::text_area::*;
pub use crate::atoms::text_input::*;
pub use crate::atoms::toast::*;
pub use crate::error::*;
pub use crate::theme::{chrome::*, color::*, misc::*, typography::*, z_index::*};
pub use crate::util::{async_loader::*, mixins::*, signal::*};
pub use wasm_bindgen::prelude::*;

use dominator::DomBuilder;
pub use dominator::{
    apply_methods, attrs, class, clone, events, fragment, html, link, pseudo, styles, svg,
    with_node, Dom, Fragment,
};
pub use futures_signals::{
    map_ref,
    signal::{Mutable, Signal, SignalExt},
    signal_vec::{MutableVec, SignalVec, SignalVecExt},
};
pub use std::sync::{Arc, LazyLock, Mutex, RwLock};

// mixin aliases and helper traits
pub type MixinStub<T> = fn(DomBuilder<T>) -> DomBuilder<T>;

pub trait MixinFnOnce<T>: FnOnce(DomBuilder<T>) -> DomBuilder<T> {}
impl<T, F> MixinFnOnce<T> for F where F: FnOnce(DomBuilder<T>) -> DomBuilder<T> {}

pub trait MixinFn<T>: Fn(DomBuilder<T>) -> DomBuilder<T> {}
impl<T, F> MixinFn<T> for F where F: Fn(DomBuilder<T>) -> DomBuilder<T> {}
