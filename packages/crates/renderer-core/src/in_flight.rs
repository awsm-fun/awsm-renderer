//! One compile per key, however many callers ask for it at once.
//!
//! The small helper pipelines in this crate (blit, sRGB conversion, mipmap,
//! BRDF LUT) live in per-thread caches that are filled when a compile
//! *finishes*. Two callers that ask for the same pipeline while the first
//! compile is still running therefore both miss the cache and both compile.
//! That was harmless while every caller awaited its pipeline on the spot, one
//! after another. It stops being harmless as soon as a pipeline is started
//! early, in the background, so that the caller which needs it later finds it
//! ready: the later caller must JOIN the compile already running, not start a
//! second one.
//!
//! [`claim`] is that join. The first caller for a key gets a guard and
//! compiles; anyone else gets a receiver that resolves when the guard is
//! dropped — on success, on failure, or because the compiling future was
//! cancelled — and then looks in the cache again.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::thread::LocalKey;

use futures::channel::oneshot;

/// The compiles in flight for one cache, and who is waiting on each.
pub(crate) struct InFlight<K>(RefCell<HashMap<K, Vec<oneshot::Sender<()>>>>);

impl<K> InFlight<K> {
    pub(crate) fn new() -> Self {
        Self(RefCell::new(HashMap::new()))
    }
}

/// What [`claim`] hands back.
pub(crate) enum Claim<K: Hash + Eq + Clone + 'static> {
    /// Nobody is compiling this key: the caller does, and keeps the guard
    /// alive until the result is in the cache (or the attempt has failed).
    Mine(InFlightGuard<K>),
    /// Someone else is compiling it. Await the receiver (its result does not
    /// matter — a dropped sender wakes it too), then check the cache again.
    Wait(oneshot::Receiver<()>),
}

/// Marks a key as being compiled. Dropping it releases the key and wakes
/// everyone who was waiting for it.
pub(crate) struct InFlightGuard<K: Hash + Eq + Clone + 'static> {
    registry: &'static LocalKey<InFlight<K>>,
    key: K,
}

impl<K: Hash + Eq + Clone + 'static> Drop for InFlightGuard<K> {
    fn drop(&mut self) {
        // `try_with`: a guard dropped during thread teardown has nobody left
        // to wake.
        let _ = self.registry.try_with(|in_flight| {
            // Dropping the senders is the wake-up.
            in_flight.0.borrow_mut().remove(&self.key);
        });
    }
}

/// Claims `key` in `registry`, or queues behind whoever holds it.
pub(crate) fn claim<K: Hash + Eq + Clone + 'static>(
    registry: &'static LocalKey<InFlight<K>>,
    key: &K,
) -> Claim<K> {
    registry.with(|in_flight| {
        let mut in_flight = in_flight.0.borrow_mut();
        match in_flight.get_mut(key) {
            Some(waiters) => {
                let (tx, rx) = oneshot::channel();
                waiters.push(tx);
                Claim::Wait(rx)
            }
            None => {
                in_flight.insert(key.clone(), Vec::new());
                Claim::Mine(InFlightGuard {
                    registry,
                    key: key.clone(),
                })
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        static REGISTRY: InFlight<u32> = InFlight::new();
    }

    fn is_resolved(rx: &mut oneshot::Receiver<()>) -> bool {
        // `Ok(None)` = still pending. A dropped sender reports `Err(Canceled)`.
        !matches!(rx.try_recv(), Ok(None))
    }

    #[test]
    fn the_first_caller_compiles_and_later_ones_wait_for_it() {
        let Claim::Mine(guard) = claim(&REGISTRY, &1) else {
            panic!("nothing was in flight: the first claim must be ours");
        };
        let Claim::Wait(mut second) = claim(&REGISTRY, &1) else {
            panic!("a second claim on a key in flight must wait");
        };
        let Claim::Wait(mut third) = claim(&REGISTRY, &1) else {
            panic!("and so must a third");
        };
        assert!(!is_resolved(&mut second));
        assert!(!is_resolved(&mut third));

        drop(guard);
        assert!(is_resolved(&mut second));
        assert!(is_resolved(&mut third));
        // The key is free again: whoever looks next and still misses the
        // cache (a failed compile) gets to try.
        assert!(matches!(claim(&REGISTRY, &1), Claim::Mine(_)));
    }

    #[test]
    fn keys_do_not_wait_on_each_other() {
        let Claim::Mine(_a) = claim(&REGISTRY, &10) else {
            panic!("first claim on 10");
        };
        assert!(matches!(claim(&REGISTRY, &11), Claim::Mine(_)));
    }
}
