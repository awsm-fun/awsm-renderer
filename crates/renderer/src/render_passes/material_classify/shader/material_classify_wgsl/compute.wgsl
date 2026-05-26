// Material classify compute pass.
//
// Per 8×8 tile, scan the visibility buffer and discover which opaque
// `shader_id`s its pixels belong to. Aggregate via a workgroup-shared
// bitmask (one bit per registered bucket, including dynamic
// materials), then thread 0 atomically appends the tile's coords to
// each bucket bit is set in. The total atomic traffic is ~1 per
// workgroup-bit, regardless of the 64 threads inside.
//
// Skybox pixels (`triangle_index == U32_MAX`) are routed to the PBR
// bucket — the PBR pipeline retains the skybox-fallback `textureStore`
// in `material_opaque/.../compute.wgsl` so the existing skybox rendering
// path keeps working with zero extra plumbing. Non-PBR pipelines
// (Unlit / Toon / FlipBook / any registered dynamic material)
// early-return on skybox without writing.
//
// The bit constants + the shader_id → bit dispatch chain + the
// per-bucket extract block are all walked from the same
// `bucket_entries` list the templated `ClassifyOutput` struct uses.

{{ shader_id_consts|safe }}

{% include "shared_wgsl/math.wgsl" %}

// Bits in the workgroup-shared mask. One per registered bucket; the
// PBR bit is at index 0 by convention so the skybox-fallback routing
// (which assigns the PBR bit unconditionally for skybox pixels) maps
// cleanly. `1u << index` for each entry.
{% for entry in bucket_entries %}
const {{ entry.bucket_bit_const() }}: u32 = (1u << {{ loop.index0 }}u);
{% endfor %}

var<workgroup> tile_mask: atomic<u32>;

@compute @workgroup_size(8, 8, 1)
fn cs_main(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(local_invocation_index) lii: u32,
) {
    // Zero the workgroup mask once per dispatch. The barrier below
    // makes the zero visible to every thread before the per-pixel
    // OR's land.
    if lii == 0u {
        atomicStore(&tile_mask, 0u);
    }
    workgroupBarrier();

    let screen_dims = textureDimensions(visibility_data_tex);
    let coords = vec2<i32>(wg.xy * 8u + lid.xy);
    let in_bounds = coords.x < i32(screen_dims.x) && coords.y < i32(screen_dims.y);

    var local_bit: u32 = 0u;
    if in_bounds {
        let vis = textureLoad(visibility_data_tex, coords, 0);
        let tri = join32(vis.x, vis.y);
        if tri == U32_MAX {
            // Skybox — handled by the PBR pipeline (it retains the
            // `triangle_index == U32_MAX → sample_skybox` fallback).
            local_bit = BUCKET_BIT_PBR;
        } else {
            let meta_offset = join32(vis.z, vis.w);
            let mesh_meta = material_mesh_metas[meta_offset / 256u];
            if mesh_meta.is_hud == 0u {
                // shader_id is stored as the first u32 of each
                // material payload; `material_offset` is in bytes.
                let shader_id = materials_data[mesh_meta.material_offset / 4u];
                {% for entry in bucket_entries %}
                {% if loop.first %}if{% else %}else if{% endif %} shader_id == {{ entry.shader_id_const() }} {
                    local_bit = {{ entry.bucket_bit_const() }};
                }
                {% endfor %}
            }
            // HUD pixels are redrawn by the transparency pass — skip
            // them in classify so the opaque pipelines don't process
            // tiles that contain only HUD geometry.
        }
    }

    if local_bit != 0u {
        atomicOr(&tile_mask, local_bit);
    }
    workgroupBarrier();

    if lii == 0u {
        let mask = atomicLoad(&tile_mask);
        let tile = vec2<u32>(wg.xy);

        // One extract block per registered bucket. The atomic returns
        // the previous count, which also doubles as the next free
        // index into the tile array at
        // `classify_output.<name>_offset + index`.
        {% for entry in bucket_entries %}
        if (mask & {{ entry.bucket_bit_const() }}) != 0u {
            let idx_{{ loop.index0 }} = atomicAdd(&classify_output.{{ entry.args_field() }}.workgroup_count_x, 1u);
            let slot_{{ loop.index0 }} = classify_output.{{ entry.offset_field() }} + idx_{{ loop.index0 }};
            if slot_{{ loop.index0 }} < classify_output.{{ entry.offset_field() }} + classify_output.bucket_capacity {
                classify_output.tiles[slot_{{ loop.index0 }}] = tile;
            }
        }
        {% endfor %}
    }
}
