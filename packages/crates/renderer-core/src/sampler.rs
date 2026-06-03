//! Sampler descriptor helpers.

use crate::compare::CompareFunction;

/// Builder for a GPU sampler descriptor.
#[derive(Default, Debug, Clone, PartialEq)]
pub struct SamplerDescriptor<'a> {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createSampler#descriptor
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuSamplerDescriptor.html
    pub address_mode_u: Option<AddressMode>,
    pub address_mode_v: Option<AddressMode>,
    pub address_mode_w: Option<AddressMode>,
    pub compare: Option<CompareFunction>,
    pub label: Option<&'a str>,
    pub lod_min_clamp: Option<f32>,
    pub lod_max_clamp: Option<f32>,
    pub max_anisotropy: Option<u16>,
    pub mag_filter: Option<FilterMode>,
    pub min_filter: Option<FilterMode>,
    pub mipmap_filter: Option<MipmapFilterMode>,
}

/// WebGPU sampler address mode.
// https://docs.rs/web-sys/latest/web_sys/enum.GpuAddressMode.html
/// WebGPU sampler address mode.
pub type AddressMode = web_sys::GpuAddressMode;
/// WebGPU sampler filter mode.
// https://docs.rs/web-sys/latest/web_sys/enum.GpuFilterMode.html
/// WebGPU sampler filter mode.
pub type FilterMode = web_sys::GpuFilterMode;
/// WebGPU sampler mipmap filter mode.
// https://docs.rs/web-sys/latest/web_sys/enum.GpuMipmapFilterMode.html
/// WebGPU sampler mipmap filter mode.
pub type MipmapFilterMode = web_sys::GpuMipmapFilterMode;

// js conversions

impl From<SamplerDescriptor<'_>> for web_sys::GpuSamplerDescriptor {
    fn from(descriptor: SamplerDescriptor) -> Self {
        let sampler_js = web_sys::GpuSamplerDescriptor::new();

        if let Some(address_mode_u) = descriptor.address_mode_u {
            sampler_js.set_address_mode_u(address_mode_u);
        }
        if let Some(address_mode_v) = descriptor.address_mode_v {
            sampler_js.set_address_mode_v(address_mode_v);
        }
        if let Some(address_mode_w) = descriptor.address_mode_w {
            sampler_js.set_address_mode_w(address_mode_w);
        }
        if let Some(compare) = descriptor.compare {
            sampler_js.set_compare(compare);
        }
        if let Some(label) = descriptor.label {
            sampler_js.set_label(label);
        }
        if let Some(lod_min_clamp) = descriptor.lod_min_clamp {
            sampler_js.set_lod_min_clamp(lod_min_clamp);
        }
        if let Some(lod_max_clamp) = descriptor.lod_max_clamp {
            sampler_js.set_lod_max_clamp(lod_max_clamp);
        }
        if let Some(max_anisotropy) = descriptor.max_anisotropy {
            sampler_js.set_max_anisotropy(max_anisotropy);
        }
        if let Some(mag_filter) = descriptor.mag_filter {
            sampler_js.set_mag_filter(mag_filter);
        }
        if let Some(min_filter) = descriptor.min_filter {
            sampler_js.set_min_filter(min_filter);
        }
        if let Some(mipmap_filter) = descriptor.mipmap_filter {
            sampler_js.set_mipmap_filter(mipmap_filter);
        }

        sampler_js
    }
}
