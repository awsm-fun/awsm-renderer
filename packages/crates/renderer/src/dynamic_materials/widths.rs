//! Per-frame encoding widths derived from the **live** bucket count.
//!
//! See `docs/plans/increase-materials.md` §0 + §3. The central insight:
//! every GPU encoding width below is a pure function of the *live*
//! `bucket_count`, NOT the configured registration cap — so the typical
//! (<16 material) scene pays exactly what it pays today, and a width only
//! grows once the live count crosses its boundary.
//!
//! These helpers are the single source of truth the classify shader, the
//! edge shaders, and the edge-buffer sizing all agree on, so the two GPU
//! encodings (classify `tile_mask` words ↔ edge `edge_slot_map` bits) can
//! never diverge for a given live count.

/// Largest registration cap [`BucketConfig`] accepts (`0xFFFE`). The top
/// two 16-bit values (`0xFFFE` skybox / `0xFFFF` empty) are reserved as the
/// widened edge-slot sentinels (§5), so the highest usable bucket index is
/// `0xFFFD` and the cap is `0xFFFE` buckets.
pub const MAX_BUCKET_ENTRIES_CEILING: u32 = 0xFFFE;

/// Default registration cap — identical to today's `MAX_BUCKET_WORDS * 32`,
/// so behavior is unchanged unless [`BucketConfig`] is set on the builder.
pub const DEFAULT_MAX_BUCKET_ENTRIES: u32 = 32;

/// Runtime tunable for the **registration ceiling** — how many co-resident
/// material buckets the registry will accept (`docs/plans/increase-materials.md`
/// §2, Option B). It sizes NOTHING per-frame: every GPU encoding width is a
/// pure function of the *live* bucket count (see [`classify_mask_words`] and
/// the edge-slot helpers), so raising the cap costs nothing until the live
/// count actually grows. Set via `AwsmRendererBuilder::with_bucket_config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BucketConfig {
    /// Max co-resident buckets the registry will accept. Default 32
    /// (== today). Valid range `1..=65534`. Values >254 require — and (§5)
    /// automatically enable — the 16-bit edge packing at runtime.
    pub max_bucket_entries: u32,
}

impl Default for BucketConfig {
    fn default() -> Self {
        Self {
            max_bucket_entries: DEFAULT_MAX_BUCKET_ENTRIES,
        }
    }
}

impl BucketConfig {
    /// Validates the cap is in `1..=65534`. Called by the builder so a bad
    /// config fails fast rather than producing a registry that can mint a
    /// bucket index the edge encoding can't represent.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_bucket_entries < 1 || self.max_bucket_entries > MAX_BUCKET_ENTRIES_CEILING {
            return Err(format!(
                "BucketConfig.max_bucket_entries = {} is out of range (must be 1..={})",
                self.max_bucket_entries, MAX_BUCKET_ENTRIES_CEILING
            ));
        }
        Ok(())
    }
}

/// Number of `atomic<u32>` words the classify `tile_mask` workgroup array
/// needs to hold one bit per live bucket (32 bits per word).
///
/// `1` at `live_bucket_count <= 32` → identical to today's single-word
/// form. A workgroup-array size must be a compile-time constant in WGSL,
/// so this value is templated into the shader and is therefore part of the
/// classify cache key (via `bucket_entries`, whose length determines it).
pub fn classify_mask_words(live_bucket_count: u32) -> u32 {
    live_bucket_count.div_ceil(32).max(1)
}

/// Width (in bits) of each per-sample bucket-index field packed into the
/// edge `edge_slot_map` (§5). `8` while the live count fits the 8-bit
/// sentinels (`0xFE` skybox / `0xFF` empty → 254 usable) — byte-identical
/// to today; `16` once the count exceeds 254 (`0xFFFE`/`0xFFFF` sentinels →
/// up to 65534). A pure function of the live count, so the 8-bit path costs
/// nothing until the count actually crosses 254.
pub fn edge_slot_bits(live_bucket_count: u32) -> u8 {
    if live_bucket_count <= 254 {
        8
    } else {
        16
    }
}

/// u32 words per edge pixel in the `edge_slot_map` region: 4 samples ×
/// `edge_slot_bits` / 32. 8-bit → 1 word, 16-bit → 2 words.
pub fn edge_slot_words_per_edge(live_bucket_count: u32) -> u32 {
    match edge_slot_bits(live_bucket_count) {
        8 => 1,
        _ => 2,
    }
}

// Packed edge-slot sentinels. The 8-bit pair is unchanged from today; the
// 16-bit pair is used once the live count exceeds 254. Classify packs the
// truncated form (`full_u32_sentinel & mask`); the resolve shaders compare
// the unpacked field against these.
pub const EDGE_SENTINEL_SKYBOX_8: u32 = 0xFE;
pub const EDGE_SENTINEL_EMPTY_8: u32 = 0xFF;
pub const EDGE_SENTINEL_SKYBOX_16: u32 = 0xFFFE;
pub const EDGE_SENTINEL_EMPTY_16: u32 = 0xFFFF;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_slot_bits_flips_at_254() {
        assert_eq!(edge_slot_bits(0), 8);
        assert_eq!(edge_slot_bits(16), 8);
        assert_eq!(edge_slot_bits(254), 8); // 254 usable in 8-bit
        assert_eq!(edge_slot_bits(255), 16);
        assert_eq!(edge_slot_bits(1024), 16);
        assert_eq!(edge_slot_bits(65534), 16);
        assert_eq!(edge_slot_words_per_edge(254), 1);
        assert_eq!(edge_slot_words_per_edge(255), 2);
        assert_eq!(edge_slot_words_per_edge(1024), 2);
    }

    #[test]
    fn edge_slot_bits_can_represent_every_live_index() {
        // Lockstep invariant (§5/§8): the chosen width must encode every
        // bucket index 0..live-1 plus leave room for the two sentinels.
        for &live in &[1u32, 16, 254, 255, 1024, 65534] {
            let max_index = live - 1;
            let bits = edge_slot_bits(live);
            let sentinel_floor = if bits == 8 { 0xFEu32 } else { 0xFFFEu32 };
            assert!(
                max_index < sentinel_floor,
                "live={live}: max index {max_index} collides with the {bits}-bit sentinel floor {sentinel_floor}"
            );
        }
    }

    #[test]
    fn classify_mask_words_matches_today_at_small_counts() {
        // The historical default cap is 32 → exactly one mask word, so the
        // generated WGSL stays byte-identical to today for every typical
        // scene. This is the §9.1 parity baseline guarantee.
        assert_eq!(classify_mask_words(0), 1);
        assert_eq!(classify_mask_words(1), 1);
        assert_eq!(classify_mask_words(16), 1);
        assert_eq!(classify_mask_words(32), 1);
    }

    #[test]
    fn bucket_config_default_is_32_and_validates() {
        let cfg = BucketConfig::default();
        assert_eq!(cfg.max_bucket_entries, 32);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn bucket_config_validation_bounds() {
        assert!(BucketConfig {
            max_bucket_entries: 1
        }
        .validate()
        .is_ok());
        assert!(BucketConfig {
            max_bucket_entries: 254
        }
        .validate()
        .is_ok());
        assert!(BucketConfig {
            max_bucket_entries: 1024
        }
        .validate()
        .is_ok());
        assert!(BucketConfig {
            max_bucket_entries: 65534
        }
        .validate()
        .is_ok());
        // Out of range.
        assert!(BucketConfig {
            max_bucket_entries: 0
        }
        .validate()
        .is_err());
        assert!(BucketConfig {
            max_bucket_entries: 65535
        }
        .validate()
        .is_err());
    }

    #[test]
    fn classify_mask_words_grows_one_word_per_32_buckets() {
        assert_eq!(classify_mask_words(33), 2);
        assert_eq!(classify_mask_words(64), 2);
        assert_eq!(classify_mask_words(65), 3);
        assert_eq!(classify_mask_words(254), 8); // ceil(254/32) = 8
        assert_eq!(classify_mask_words(255), 8);
        assert_eq!(classify_mask_words(256), 8);
        assert_eq!(classify_mask_words(1024), 32); // ceil(1024/32) = 32
        assert_eq!(classify_mask_words(65534), 2048); // ceil(65534/32)
    }
}
