//! Buffer helper utilities.

/// Returns a static zero slice of the requested size (up to 32 bytes).
pub fn slice_zeroes(size: usize) -> &'static [u8] {
    const ZEROES: [u8; 32] = [0; 32];
    &ZEROES[..size]
}

/// Splits a byte slice into f32 chunks for debugging.
#[allow(dead_code)]
pub fn debug_chunks_to_f32(slice: &[u8], chunk_size: usize) -> Vec<Vec<f32>> {
    u8_to_f32_vec(slice)
        .chunks(chunk_size)
        .map(|chunk| chunk.to_vec())
        .collect()
}

/// Splits a byte slice into u32 chunks for debugging.
#[allow(dead_code)]
pub fn debug_chunks_to_u32(slice: &[u8], chunk_size: usize) -> Vec<Vec<u32>> {
    u8_to_u32_vec(slice)
        .chunks(chunk_size)
        .map(|chunk| chunk.to_vec())
        .collect()
}

// From gltf spec:
// "All buffer data defined in this specification (i.e., geometry attributes, geometry indices, sparse accessor data, animation inputs and outputs, inverse bind matrices)
// MUST use little endian byte order."

/// Expands packed u16 bytes into u32 bytes (little endian).
pub fn u16_to_u32_vec(v: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(v.len() * 2);
    for chunk in v.chunks_exact(2) {
        let value = u16::from_le_bytes([chunk[0], chunk[1]]);
        // Promote each 16-bit lane to 32-bit so the caller's metadata (data_size = 4)
        // stays in sync with the packed byte stream.
        output.extend_from_slice(&(value as u32).to_le_bytes());
    }
    output
}

/// Expands packed i16 bytes into i32 bytes (little endian).
pub fn i16_to_i32_vec(v: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(v.len() * 2);
    for chunk in v.chunks_exact(2) {
        let value = i16::from_le_bytes([chunk[0], chunk[1]]);
        // Same story for signed 16-bit attributes (normals/tangents, etc.).
        output.extend_from_slice(&(value as i32).to_le_bytes());
    }
    output
}

/// Converts raw bytes into f32 values (little endian).
pub fn u8_to_f32_vec(v: &[u8]) -> Vec<f32> {
    v.chunks_exact(4)
        .map(TryInto::try_into)
        .map(Result::unwrap)
        .map(f32::from_le_bytes)
        .collect()
}

/// Iterates f32 values from raw bytes (little endian).
pub fn u8_to_f32_iter(v: &[u8]) -> impl Iterator<Item = f32> + '_ {
    v.chunks_exact(4)
        .map(TryInto::try_into)
        .map(Result::unwrap)
        .map(f32::from_le_bytes)
}

/// Converts raw bytes into i8 values.
pub fn u8_to_i8_vec(v: &[u8]) -> Vec<i8> {
    v.chunks_exact(1)
        .map(TryInto::try_into)
        .map(Result::unwrap)
        .map(i8::from_le_bytes)
        .collect()
}

/// Converts raw bytes into u16 values (little endian).
pub fn u8_to_u16_vec(v: &[u8]) -> Vec<u16> {
    v.chunks_exact(2)
        .map(TryInto::try_into)
        .map(Result::unwrap)
        .map(u16::from_le_bytes)
        .collect()
}

/// Iterates u16 values from raw bytes (little endian).
pub fn u8_to_u16_iter(v: &[u8]) -> impl Iterator<Item = u16> + '_ {
    v.chunks_exact(2)
        .map(TryInto::try_into)
        .map(Result::unwrap)
        .map(u16::from_le_bytes)
}

/// Converts raw bytes into i16 values (little endian).
pub fn u8_to_i16_vec(v: &[u8]) -> Vec<i16> {
    v.chunks_exact(2)
        .map(TryInto::try_into)
        .map(Result::unwrap)
        .map(i16::from_le_bytes)
        .collect()
}

/// Converts raw bytes into u32 values (little endian).
pub fn u8_to_u32_vec(v: &[u8]) -> Vec<u32> {
    v.chunks_exact(4)
        .map(TryInto::try_into)
        .map(Result::unwrap)
        .map(u32::from_le_bytes)
        .collect()
}

/// Iterates u32 values from raw bytes (little endian).
pub fn u8_to_u32_iter(v: &[u8]) -> impl Iterator<Item = u32> + '_ {
    v.chunks_exact(4)
        .map(TryInto::try_into)
        .map(Result::unwrap)
        .map(u32::from_le_bytes)
}
use awsm_renderer_core::{error::AwsmCoreError, renderer::AwsmRendererWebGpu};

const DIRTY_RANGE_FULL_WRITE_THRESHOLD_PERCENT: u64 = 60;
const DIRTY_RANGE_MAX_RANGES: usize = 32;

/// Writes only dirty ranges to a GPU buffer, with a full-write fallback.
pub fn write_buffer_with_dirty_ranges(
    gpu: &AwsmRendererWebGpu,
    gpu_buffer: &web_sys::GpuBuffer,
    raw_data: &[u8],
    ranges: Vec<(usize, usize)>,
) -> Result<(), AwsmCoreError> {
    write_buffer_with_dirty_ranges_config(
        gpu,
        gpu_buffer,
        raw_data,
        ranges,
        DIRTY_RANGE_FULL_WRITE_THRESHOLD_PERCENT,
        DIRTY_RANGE_MAX_RANGES,
    )
}

fn write_buffer_with_dirty_ranges_config(
    gpu: &AwsmRendererWebGpu,
    gpu_buffer: &web_sys::GpuBuffer,
    raw_data: &[u8],
    ranges: Vec<(usize, usize)>,
    full_write_threshold_percent: u64,
    max_ranges: usize,
) -> Result<(), AwsmCoreError> {
    let prepared = prepare_dirty_ranges_config(
        &ranges,
        raw_data.len(),
        full_write_threshold_percent,
        max_ranges,
    );
    if prepared.is_empty() {
        return Ok(());
    }

    for (offset, size) in prepared {
        if size == 0 {
            continue;
        }
        let end = offset.saturating_add(size);
        debug_assert!(end <= raw_data.len());
        if let Some(slice) = raw_data.get(offset..end) {
            if !slice.is_empty() {
                gpu.write_buffer(gpu_buffer, Some(offset), slice, None, None)?;
            }
        }
    }

    Ok(())
}

/// Apply the renderer-wide dirty-range upload policy:
/// 1. **Too many ranges** (> 32 by default): collapse to a single
///    `(0, total_bytes)` range so the upload is one full write
///    instead of N tiny ones.
/// 2. **Density** (dirty bytes ≥ 60% of total by default): same —
///    collapse to one full write. The per-range overhead exceeds
///    the savings beyond this point.
/// 3. **Sort + coalesce** adjacent ranges so consumers always see a
///    minimal, monotonically-ordered range list.
///
/// Used by both the writeBuffer fallback path
/// (`write_buffer_with_dirty_ranges`) and the mapped-staging-ring
/// upload path (`MappedUploader::write_dirty_ranges`) so both honour
/// the same worst-case-bounding policy. Without this on the mapped
/// path, a frame that dirties many disjoint slots would queue one
/// `copy_buffer_to_buffer` per slot — the bound matters because
/// command-record cost is JS-side and dominates at high range count.
pub fn prepare_dirty_ranges(ranges: &[(usize, usize)], total_bytes: usize) -> Vec<(usize, usize)> {
    prepare_dirty_ranges_config(
        ranges,
        total_bytes,
        DIRTY_RANGE_FULL_WRITE_THRESHOLD_PERCENT,
        DIRTY_RANGE_MAX_RANGES,
    )
}

fn prepare_dirty_ranges_config(
    ranges: &[(usize, usize)],
    total_bytes: usize,
    full_write_threshold_percent: u64,
    max_ranges: usize,
) -> Vec<(usize, usize)> {
    if total_bytes == 0 || ranges.is_empty() {
        return Vec::new();
    }

    // Many ranges → collapse to a single full-buffer range.
    if ranges.len() > max_ranges {
        return vec![(0, total_bytes)];
    }

    // Density check → collapse to a single full-buffer range.
    let total = total_bytes as u64;
    let dirty: u64 = ranges.iter().map(|(_, size)| *size as u64).sum();
    if dirty.saturating_mul(100) >= total.saturating_mul(full_write_threshold_percent) {
        return vec![(0, total_bytes)];
    }

    if ranges.len() == 1 {
        return ranges.to_vec();
    }

    let mut owned: Vec<(usize, usize)> = ranges.to_vec();
    owned.sort_unstable_by_key(|(start, _)| *start);
    coalesce_ranges(owned)
}

// Coalesces adjacent dirty ranges to minimise `writeBuffer` calls.
// Tests below pin the merge behaviour so a future refactor can't
// silently degrade the upload path back to many small calls.
fn coalesce_ranges(ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    if ranges.is_empty() {
        return ranges;
    }

    let mut merged = Vec::with_capacity(ranges.len());
    let mut current_start = ranges[0].0;
    let mut current_end = current_start.saturating_add(ranges[0].1);

    for (start, size) in ranges.into_iter().skip(1) {
        let end = start.saturating_add(size);
        if start <= current_end {
            current_end = current_end.max(end);
        } else {
            merged.push((current_start, current_end - current_start));
            current_start = start;
            current_end = end;
        }
    }

    merged.push((current_start, current_end - current_start));
    merged
}

#[cfg(test)]
mod coalesce_tests {
    use super::coalesce_ranges;

    #[test]
    fn adjacent_ranges_merge() {
        let ranges = vec![(0, 16), (16, 16), (32, 8)];
        assert_eq!(coalesce_ranges(ranges), vec![(0, 40)]);
    }

    #[test]
    fn overlapping_ranges_merge() {
        let ranges = vec![(0, 32), (16, 32)];
        assert_eq!(coalesce_ranges(ranges), vec![(0, 48)]);
    }

    #[test]
    fn disjoint_ranges_stay_separate() {
        let ranges = vec![(0, 16), (32, 16)];
        assert_eq!(coalesce_ranges(ranges), vec![(0, 16), (32, 16)]);
    }

    #[test]
    fn empty_input_is_empty_output() {
        let ranges: Vec<(usize, usize)> = vec![];
        assert_eq!(coalesce_ranges(ranges), vec![]);
    }
}
