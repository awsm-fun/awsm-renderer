# Draft: dimforge/parry issue

Target: https://github.com/dimforge/parry/issues/new
Title: `Binned BVH build indexes the bin array unclamped (index out of bounds: the len is 8 but the index is 8)`

---

### What

`rebuild_range_binned` computes a bin index by scaling a leaf centroid into
`[0, NUM_BINS)` and indexes the bin array with it directly, with no clamp:

https://github.com/dimforge/parry/blob/master/src/partitioning/bvh/bvh_binned_build.rs#L57-L61

```rust
const NUM_BINS: usize = 8;
const BIN_EPSILON: Real = 1.0e-5;
...
let k1 = NUM_BINS as Real * (1.0 - BIN_EPSILON) / (bins_range[1] - bins_range[0]);
let k0 = bins_range[0];
for leaf in &*leaves {
    let bin_id = (k1 * (leaf.center().vget(bins_axis) - k0)) as usize;
    let bin = &mut bins[bin_id];   // <-- unclamped
```

The `(1.0 - BIN_EPSILON)` factor is the only thing keeping `bin_id` below
`NUM_BINS`. We hit the panic once, in a WebGPU renderer that inserts mesh AABBs
into a `Bvh`:

```
index out of bounds: the len is 8 but the index is 8
```

We could not reproduce it, so this is not a "here is your repro" report — it is
"this one line turns any upstream malformed input into a panic deep in parry
rather than an error at the boundary". Details below in case the analysis is
useful.

### The epsilon is a wide margin, so this is not drift

Since `bins_range` is the centroid AABB, the largest value the expression can
take for a well-formed leaf set is at `center == bins_range[1]`, where it is
exactly `NUM_BINS * (1.0 - BIN_EPSILON)` = **7.999920** → bin 7. That holds
regardless of scale — coordinates out to 1e7 with extents down to 1e-4 all give
the same 7.999920, because the range cancels.

`BIN_EPSILON` is a *relative* margin of 1e-5, roughly 170x `f32::EPSILON`
(5.96e-8), so accumulated rounding in the divide+multiply cannot close it.
Degenerate cases fall inward rather than outward: a zero-extent range makes
`k1` infinite and `(center - k0)` zero, so the product is `NaN`, and `as usize`
saturates `NaN` to 0.

So reaching index 8 means the input was already malformed — a centroid outside
the centroid AABB, which in practice means a non-finite coordinate somewhere
upstream — rather than arithmetic drifting out of range.

### Why it is still worth clamping

The panic surfaces inside parry with no reference to the offending leaf, which
sends people looking for a parry bug (it is where we looked first). A clamp
makes the out-of-bounds case unreachable and costs one `min`:

```rust
let bin_id = ((k1 * (leaf.center().vget(bins_axis) - k0)) as usize).min(NUM_BINS - 1);
```

That does not *fix* malformed input — a NaN centroid still produces a
meaningless tree — but it moves the failure to where the bad data actually is
instead of an array index eight frames down.

If you would rather keep the panic as an assertion that input is well-formed, a
`debug_assert!` naming the leaf and its centroid would serve the same diagnostic
purpose. Either is better than the bare index for us; happy to send a PR for
whichever you prefer.

### Version

`parry3d 0.28.0`, `f32` (default `Real`). Same code is present on `master` at
time of writing.
