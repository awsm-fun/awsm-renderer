// Material classify compute pass.
//
// Per 8×8 tile, scan the visibility buffer and discover which opaque
// `shader_id`s its pixels belong to. Aggregate via a workgroup-shared
// 4-bit mask (one bit per shader_id), then thread 0 atomically
// appends the tile's coords to each bucket bit is set in. The total
// atomic traffic is ~1 per workgroup-bit, regardless of the 64 threads
// inside.
//
// Skybox pixels (`triangle_index == U32_MAX`) are routed to the PBR
// bucket — the PBR pipeline retains the skybox-fallback `textureStore`
// in `material_opaque/.../compute.wgsl` so the existing skybox rendering
// path keeps working with zero extra plumbing. Unlit / Toon pipelines
// early-return on skybox without writing.

{{ shader_id_consts|safe }}

// `U32_MAX` is already declared in `shared_wgsl/math.wgsl`, which the
// bind-groups WGSL above pulls in.

// Bits in the workgroup-shared mask. Match the
// `classify_output.{pbr,unlit,toon}_offset` ordering on the host side
// and the `bucket_index` used by the material-opaque template's
// `dispatch_workgroups_indirect(args_buffer, bucket_index * 16)`.
const BUCKET_BIT_PBR: u32 = 1u;
const BUCKET_BIT_UNLIT: u32 = 2u;
const BUCKET_BIT_TOON: u32 = 4u;

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
                if shader_id == SHADER_ID_PBR {
                    local_bit = BUCKET_BIT_PBR;
                } else if shader_id == SHADER_ID_UNLIT {
                    local_bit = BUCKET_BIT_UNLIT;
                } else if shader_id == SHADER_ID_TOON {
                    local_bit = BUCKET_BIT_TOON;
                }
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

        // PBR bucket. The atomic returns the previous count, which
        // also doubles as the next free index into the tile array
        // at `classify_output.pbr_offset + index`.
        if (mask & BUCKET_BIT_PBR) != 0u {
            let idx = atomicAdd(&classify_output.args_pbr.workgroup_count_x, 1u);
            let slot = classify_output.pbr_offset + idx;
            if slot < classify_output.pbr_offset + classify_output.bucket_capacity {
                classify_output.tiles[slot] = tile;
            }
        }
        if (mask & BUCKET_BIT_UNLIT) != 0u {
            let idx = atomicAdd(&classify_output.args_unlit.workgroup_count_x, 1u);
            let slot = classify_output.unlit_offset + idx;
            if slot < classify_output.unlit_offset + classify_output.bucket_capacity {
                classify_output.tiles[slot] = tile;
            }
        }
        if (mask & BUCKET_BIT_TOON) != 0u {
            let idx = atomicAdd(&classify_output.args_toon.workgroup_count_x, 1u);
            let slot = classify_output.toon_offset + idx;
            if slot < classify_output.toon_offset + classify_output.bucket_capacity {
                classify_output.tiles[slot] = tile;
            }
        }
    }
}
