//! Bounded undo/redo history with a total-**byte** budget (drop-oldest).
//!
//! The editor's undo log retains the *inverse* of every applied command, and a
//! `SetKind`/`PatchKind`/mesh inverse can hold a whole `NodeKind` or vertex
//! payload. Left unbounded (a bare `Vec` cleared only by `new_project`), a
//! high-volume agent session grows it in WASM linear memory until a single
//! reallocation requests ~2 GB and Chrome's allocator traps (`IMMEDIATE_CRASH` —
//! the "Aw, Snap!" OOM). Capping the *retained bytes* (not the entry count, since
//! one mesh inverse can dwarf a thousand transform inverses) bounds the growth at
//! the source.
//!
//! This is pure data (no DOM/async/reactive deps) so it lives here in the
//! protocol crate alongside [`EditorCommand`] and is **host-testable**.

use std::collections::VecDeque;

use crate::EditorCommand;

/// Default total-byte budget for one history log (undo or redo). 256 MB of
/// retained inverses is far past any human's useful undo depth yet an order of
/// magnitude below the ~2 GB single-allocation cliff. A named constant so it's
/// easy to tune.
pub const DEFAULT_HISTORY_BUDGET_BYTES: usize = 256 * 1024 * 1024;

/// A `std::io::Write` sink that only *counts* bytes — never stores them. Lets
/// [`estimate_command_bytes`] measure a command's serialized size without
/// allocating the serialized buffer (the estimate runs on every recorded edit).
struct ByteCounter(usize);

impl std::io::Write for ByteCounter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Estimate the retained heap size of a command (its undo inverse), in bytes.
///
/// Uses the command's **serialized JSON length** as a cheap, recursive,
/// safe **over-estimate**: the heavy payloads that actually drive the OOM —
/// `NodeKind` geometry (`Vec<f32>` positions/normals/uvs, `Vec<u32>` indices),
/// inline heightmap/texture blobs, WGSL source — all serialize to *at least* as
/// many bytes as they occupy in memory (floats become multi-char decimal
/// strings, so vertex data over-estimates ~2-4×). Over-estimating is the safe
/// direction: it makes the byte cap evict *sooner*, never later. A small fixed
/// per-entry overhead accounts for the `VecDeque` slot + enum discriminant.
///
/// Serialization of these `Serialize` types is effectively infallible (serde_json
/// emits non-finite floats as `null` rather than erroring); on the practically
/// unreachable error path the partial count is still a usable estimate.
pub fn estimate_command_bytes(cmd: &EditorCommand) -> usize {
    const ENTRY_OVERHEAD: usize = std::mem::size_of::<EditorCommand>();
    let mut counter = ByteCounter(0);
    let _ = serde_json::to_writer(&mut counter, cmd);
    counter.0 + ENTRY_OVERHEAD
}

/// A FIFO history log capped by a total-byte budget. Pushing past the budget
/// drops the **oldest** entries (the far end of undo reach) until back under it —
/// the standard, expected behaviour for a bounded undo stack. Each entry caches
/// its own estimated size so eviction and the running total never re-serialize.
///
/// Used for both the undo and redo logs. All operations are O(1) amortized and
/// run only on edits (never per frame), so the hot render loop pays nothing.
pub struct BoundedHistory {
    /// `(inverse command, its estimated retained bytes)`, oldest at the front.
    entries: VecDeque<(EditorCommand, usize)>,
    /// Running sum of the cached per-entry byte estimates.
    bytes: usize,
    /// Drop-oldest threshold.
    budget: usize,
}

impl BoundedHistory {
    /// A history with an explicit byte budget.
    pub fn new(budget: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            budget,
        }
    }

    /// A history with the [`DEFAULT_HISTORY_BUDGET_BYTES`] budget.
    pub fn with_default_budget() -> Self {
        Self::new(DEFAULT_HISTORY_BUDGET_BYTES)
    }

    /// Record an inverse as the newest entry, evicting oldest entries while over
    /// budget. The just-pushed entry is always retained (even if it alone
    /// exceeds the budget — dropping the very edit you just made would be worse
    /// than briefly exceeding the cap).
    pub fn push(&mut self, cmd: EditorCommand) {
        let size = estimate_command_bytes(&cmd);
        self.entries.push_back((cmd, size));
        self.bytes += size;
        while self.bytes > self.budget && self.entries.len() > 1 {
            if let Some((_, old)) = self.entries.pop_front() {
                self.bytes -= old;
            }
        }
    }

    /// Pop the newest entry (the undo/redo step), updating the byte total.
    pub fn pop(&mut self) -> Option<EditorCommand> {
        let (cmd, size) = self.entries.pop_back()?;
        self.bytes -= size;
        Some(cmd)
    }

    /// Borrow the newest entry without removing it (for undo coalescing).
    pub fn last(&self) -> Option<&EditorCommand> {
        self.entries.back().map(|(cmd, _)| cmd)
    }

    /// Drop all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    /// Number of retained entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the log is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total estimated retained bytes (the cap diagnostic).
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use awsm_renderer_scene::NodeId;

    /// A small, near-constant-size inverse (a transform restore).
    fn small_cmd() -> EditorCommand {
        EditorCommand::Rename {
            id: NodeId::new(),
            name: "x".to_string(),
        }
    }

    /// A large inverse: a rename carrying a big string payload (stands in for a
    /// mesh/vertex/base64 blob — the estimator measures serialized length, so a
    /// long string is a faithful proxy for the heavy real payloads).
    fn big_cmd(bytes: usize) -> EditorCommand {
        EditorCommand::Rename {
            id: NodeId::new(),
            name: "a".repeat(bytes),
        }
    }

    #[test]
    fn estimator_scales_with_payload() {
        let small = estimate_command_bytes(&small_cmd());
        let big = estimate_command_bytes(&big_cmd(100_000));
        // The 100 KB payload must dominate the estimate and dwarf the small one.
        assert!(big > 100_000, "big estimate {big} should exceed payload");
        assert!(big > small * 100, "big {big} should dwarf small {small}");
    }

    #[test]
    fn estimator_is_an_over_estimate_of_payload() {
        // A string payload serializes to at least its own length (plus quoting +
        // the rest of the command), so the estimate never *under*-counts it.
        let n = 50_000;
        assert!(estimate_command_bytes(&big_cmd(n)) >= n);
    }

    #[test]
    fn push_evicts_oldest_over_budget() {
        // Budget fits ~3 big entries; pushing 10 must plateau, not grow.
        let one = estimate_command_bytes(&big_cmd(100_000));
        let mut h = BoundedHistory::new(one * 3 + one / 2);
        for _ in 0..10 {
            h.push(big_cmd(100_000));
        }
        assert!(
            h.bytes() <= one * 3 + one / 2,
            "bytes must stay under budget"
        );
        assert!(h.len() <= 4, "entry count must plateau, got {}", h.len());
        // The byte total must equal the sum of the surviving entries (no drift).
        assert_eq!(h.bytes(), one * h.len());
    }

    #[test]
    fn push_keeps_at_least_the_newest_even_if_oversized() {
        // A single entry larger than the whole budget is still retained.
        let mut h = BoundedHistory::new(1);
        h.push(big_cmd(10_000));
        assert_eq!(h.len(), 1);
        assert!(h.bytes() > 1);
    }

    #[test]
    fn pop_and_clear_keep_bytes_consistent() {
        let mut h = BoundedHistory::new(DEFAULT_HISTORY_BUDGET_BYTES);
        h.push(small_cmd());
        h.push(big_cmd(1000));
        let before = h.bytes();
        assert!(before > 0);
        h.pop();
        assert!(h.bytes() < before);
        assert_eq!(h.len(), 1);
        h.clear();
        assert_eq!(h.bytes(), 0);
        assert!(h.is_empty());
    }

    #[test]
    fn many_small_pushes_plateau_under_budget() {
        // The crash repro in miniature: thousands of pushes, bounded total.
        let mut h = BoundedHistory::new(64 * 1024);
        for _ in 0..10_000 {
            h.push(small_cmd());
        }
        assert!(h.bytes() <= 64 * 1024);
    }
}
