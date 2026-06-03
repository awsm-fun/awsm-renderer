//! Alignment helpers for buffer sizes and offsets.

/// ```rust
/// use awsm_renderer_core::alignment::align_to;
/// assert_eq!(align_to(0u32, 256),   0u32);
/// assert_eq!(align_to(64u32, 256), 256u32);
/// assert_eq!(align_to(511u32, 256), 512u32);
/// assert_eq!(align_to(123u8, 128u8), 128u8);
/// ```
pub fn align_to<T>(n: T, align: T) -> T
where
    T: std::ops::Rem<Output = T> + std::ops::Sub<Output = T> + std::ops::Add<Output = T> + Copy,
{
    n + padding_for(n, align)
}

/// Returns the padding required to align `n` to `align`.
pub fn padding_for<T>(n: T, align: T) -> T
where
    T: std::ops::Rem<Output = T> + std::ops::Sub<Output = T> + std::ops::Add<Output = T> + Copy,
{
    (align - (n % align)) % align
}
