use futures::future::{abortable, AbortHandle};
use futures_signals::signal::{Mutable, Signal};
use std::{
    future::Future,
    sync::atomic::{AtomicUsize, Ordering},
};
use wasm_bindgen_futures::spawn_local;

/// Runs a single cancellable async task at a time.
///
/// - Calling `.load()` cancels any in-flight task and starts a new one.
/// - Calling `.cancel()` aborts the current task.
/// - `.is_loading()` returns a signal suitable for reactive UI.
/// - Dropping the `AsyncLoader` cancels the current task.
#[derive(Clone)]
pub struct AsyncLoader {
    loading: Mutable<Option<AsyncState>>,
}

impl Drop for AsyncLoader {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl Default for AsyncLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncLoader {
    pub fn new() -> Self {
        Self {
            loading: Mutable::new(None),
        }
    }

    pub fn cancel(&self) {
        self.replace(None);
    }

    fn replace(&self, value: Option<AsyncState>) {
        let mut loading = self.loading.lock_mut();

        if let Some(state) = loading.as_mut() {
            state.handle.abort();
        }

        *loading = value;
    }

    /// Cancels any in-flight task and spawns `fut`.
    ///
    /// When `fut` completes (without being cancelled), the loading state
    /// is automatically cleared.
    pub fn load<F>(&self, fut: F)
    where
        F: Future<Output = ()> + 'static,
    {
        let (fut, handle) = abortable(fut);

        let state = AsyncState::new(handle);
        let id = state.id;

        self.replace(Some(state));

        let loading = self.loading.clone();

        spawn_local(async move {
            if (fut.await).is_ok() {
                let mut loading = loading.lock_mut();

                if let Some(current_id) = loading.as_ref().map(|x| x.id) {
                    if current_id == id {
                        *loading = None;
                    }
                }
            }
        });
    }

    /// Signal that is `true` while a task is in progress.
    pub fn is_loading(&self) -> impl Signal<Item = bool> {
        self.loading.signal_ref(|x| x.is_some())
    }
}

struct AsyncState {
    id: usize,
    handle: AbortHandle,
}

impl AsyncState {
    fn new(handle: AbortHandle) -> Self {
        static ID: AtomicUsize = AtomicUsize::new(0);
        let id = ID.fetch_add(1, Ordering::SeqCst);
        Self { id, handle }
    }
}
