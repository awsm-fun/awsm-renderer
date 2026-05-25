// Fragment input from vertex shader
struct FragmentInput {
    @builtin(position) frag_pos: vec4<f32>,
    @builtin(front_facing) front_facing: bool,
    @location(0) world_position: vec3<f32>,     // World position
    @location(1) world_normal: vec3<f32>,     // Transformed world-space normal
    @location(2) world_tangent: vec4<f32>,    // Transformed world-space tangent (w = handedness)
    @location(3) @interpolate(flat) instance_id: u32,
    {% for i in 0..color_sets %}
        @location({{ in_color_set_start + i }}) color_{{ i }}: vec4<f32>,
    {% endfor %}

    {% for i in 0..uv_sets %}
        @location({{ in_uv_set_start + i }}) uv_{{ i }}: vec2<f32>,
    {% endfor %}
}

struct FragmentOutput {
    // Rgba16float
    @location(0) color: vec4<f32>,
}

// Sample transmission background from the opaque render with screen-space refraction
// Falls back to IBL environment when refracted ray goes outside screen bounds
// Uses the physically-based approach from glTF sample renderer:
// 1. Compute refracted ray exit point in world space
// 2. Project to screen space using view/projection matrices
fn sample_transmission_background(
    frag_pos: vec4<f32>,
    world_position: vec3<f32>,
    normal: vec3<f32>,
    view_dir: vec3<f32>,
    ior: f32,
    roughness: f32,
    thickness: f32,
    dispersion: f32,
    camera: Camera,
) -> vec3<f32> {
    if (dispersion > 0.0 && thickness > 0.0) {
        // KHR_materials_dispersion: refract per RGB channel against three
        // slightly-shifted IORs and recombine. The helper handles the
        // shared ray/projection/blur work, so this stays a thin wrapper.
        // Half-spread matches the glTF Sample Renderer formula.
        let dstrength = (ior - 1.0) * 0.025 * dispersion;
        let ior_r = max(ior - dstrength, 1.0001);
        let ior_b = ior + dstrength;
        let bg_r = sample_transmission_background_for_ior(
            frag_pos, world_position, normal, view_dir, ior_r, roughness, thickness, camera,
        );
        let bg_g = sample_transmission_background_for_ior(
            frag_pos, world_position, normal, view_dir, ior, roughness, thickness, camera,
        );
        let bg_b = sample_transmission_background_for_ior(
            frag_pos, world_position, normal, view_dir, ior_b, roughness, thickness, camera,
        );
        return vec3<f32>(bg_r.r, bg_g.g, bg_b.b);
    }
    return sample_transmission_background_for_ior(
        frag_pos, world_position, normal, view_dir, ior, roughness, thickness, camera,
    );
}

fn sample_transmission_background_for_ior(
    frag_pos: vec4<f32>,
    world_position: vec3<f32>,
    normal: vec3<f32>,
    view_dir: vec3<f32>,
    ior: f32,
    roughness: f32,
    thickness: f32,
    camera: Camera,
) -> vec3<f32> {
    let screen_dims = vec2<f32>(textureDimensions(opaque_tex));
    var screen_uv = frag_pos.xy / screen_dims;
    var sample_dir = view_dir;  // Direction for IBL fallback

    // Calculate refraction for volumetric materials (KHR_materials_volume)
    let ior_val = effective_ior(ior);
    if (thickness > 0.0 && ior_val != 1.0) {
        // Snell's law: eta = n_outside / n_inside (air=1.0 -> material)
        let eta = 1.0 / ior_val;
        let refracted = refract_direction(view_dir, normal, eta);

        // Use dot product instead of length to avoid sqrt (checking for non-zero)
        if (dot(refracted, refracted) > 1e-6) {
            sample_dir = refracted;  // Use refracted direction for IBL fallback

            // Compute world-space exit point of refracted ray
            // The ray travels through the material for 'thickness' distance
            let transmission_ray = normalize(refracted) * thickness;
            let refracted_exit = world_position + transmission_ray;

            // Project exit point to clip space
            let clip_pos = camera.view_proj * vec4<f32>(refracted_exit, 1.0);

            // Perspective divide to get NDC [-1, 1]
            let ndc = clip_pos.xy / clip_pos.w;

            // Convert NDC to UV [0, 1], flip Y for texture coordinates
            screen_uv = vec2<f32>(ndc.x + 1.0, 1.0 - ndc.y) * 0.5;
        }
    }

    // Check if UV is outside screen bounds
    // If so, fall back to IBL environment sampling instead of clamping
    if (screen_uv.x < 0.0 || screen_uv.x > 1.0 || screen_uv.y < 0.0 || screen_uv.y > 1.0) {
        // Sample environment in the refracted direction with roughness-based blur
        {% if has_lighting_ibl() %}
            let ibl_info = get_lights_info().ibl;
            let max_mip = f32(ibl_info.prefiltered_env_mip_count - 1u);
            let mip_level = roughness * max_mip;
            return textureSampleLevel(ibl_filtered_env_tex, ibl_filtered_env_sampler, sample_dir, mip_level).rgb;
        {% else %}
            // No IBL available, return black
            return vec3<f32>(0.0);
        {% endif %}
    }

    // Pick a mip level from the IOR-adjusted roughness. This matches the
    // glTF Sample Renderer formula: `mip = log2(width) * roughness * clamp(ior*2 - 2, 0, 1)`.
    // The opaque RT now carries a real mip chain (built by `OpaqueMipgen`
    // after the opaque pass), so we get a properly pre-filtered fetch in
    // one hardware sample instead of the multi-tap Gaussian we used to
    // approximate it.
    let ior_roughness_factor = clamp(ior * 2.0 - 2.0, 0.0, 1.0);
    let blur_roughness = roughness * ior_roughness_factor;
    let max_mip = f32(textureNumLevels(opaque_tex) - 1u);
    let mip_level = clamp(log2(screen_dims.x) * blur_roughness, 0.0, max_mip);
    let mip_i = i32(mip_level + 0.5);
    let mip_size = vec2<f32>(textureDimensions(opaque_tex, u32(mip_i)));
    let mip_max = vec2<i32>(mip_size) - vec2<i32>(1);
    let mip_coord = clamp(vec2<i32>(screen_uv * mip_size), vec2<i32>(0), mip_max);
    return textureLoad(opaque_tex, mip_coord, mip_i).rgb;
}

@fragment
fn fs_main(input: FragmentInput) -> FragmentOutput {
    var out: FragmentOutput;

    // Convert raw camera uniform to friendly structure
    let camera = camera_from_raw(camera_raw);
    let frame_globals = frame_globals_from_raw(frame_globals_raw);

    // Handle double-sided materials: flip normal and tangent handedness for back faces
    // This must be done BEFORE material color computation so normal mapping uses the correct orientation
    var world_normal = input.world_normal;
    var world_tangent = input.world_tangent;
    if (!input.front_facing) {
        world_normal = -world_normal;
        // Flip tangent handedness to maintain correct TBN orientation
        world_tangent.w = -world_tangent.w;
    }

    // Calculate surface to camera vector for lighting
    // For orthographic cameras, use camera forward direction (parallel rays)
    // For perspective cameras, compute per-fragment from camera position
    // Detect orthographic: proj[3][3] == 1.0 for orthographic, 0.0 for perspective
    let is_orthographic = abs(camera.proj[3][3] - 1.0) < 0.001;
    let camera_forward = normalize(camera.inv_view[2].xyz);
    let surface_to_camera = select(
        normalize(camera.position - input.world_position),  // perspective
        camera_forward,                                      // orthographic
        is_orthographic
    );

    // Get shader_id to determine material type
    let material_offset = material_mesh_meta.material_offset;
    let shader_id = material_load_shader_id(material_offset);

    var color: vec3<f32>;
    var base_alpha: f32;

    if (shader_id == SHADER_ID_UNLIT) {
        // Unlit material path
        let unlit_material = unlit_get_material(material_offset);
        let unlit_color = unlit_get_material_color(unlit_material, input);
        color = compute_unlit_output(unlit_color);
        base_alpha = unlit_color.base.a;
    } else if (shader_id == SHADER_ID_FLIPBOOK) {
        // FlipBook (sprite-sheet) material — samples a single cell from
        // a grid-uniform atlas based on `frame_globals.time + time_offset`.
        let fb = flipbook_get_material(material_offset);
        var fb_sampled: vec4<f32> = vec4<f32>(1.0);
        if fb.atlas_tex_info.exists {
            let fb_uv = texture_uv(fb.atlas_tex_info, input);
            let fb_cell_uv = flipbook_compute_cell_uv(fb, fb_uv, frame_globals.time);
            fb_sampled = texture_pool_sample(fb.atlas_tex_info, fb_cell_uv);
        }
        // Alpha-mask handling: transparent path uses `discard`. The
        // shared `flipbook_finalize_color` zeros the alpha on cutoff —
        // we discard explicitly here first so we don't shade past the
        // cutoff threshold.
        if (fb.alpha_mode == ALPHA_MODE_MASK) {
            let cutoff_alpha = fb_sampled.a * fb.tint.a;
            if (cutoff_alpha < fb.alpha_cutoff) {
                discard;
            }
        }
        let fb_result = flipbook_finalize_color(fb, fb_sampled, frame_globals.time);
        color = fb_result.rgb;
        // For mask materials past the cutoff, force alpha = 1 (we
        // already passed the discard above).
        base_alpha = select(fb_result.a, 1.0, fb.alpha_mode == ALPHA_MODE_MASK);
    } else if (shader_id == SHADER_ID_TOON) {
        // Toon material path
        let toon_material = toon_get_material(material_offset);
        let lights_info = get_lights_info();
        color = compute_toon_lit_color(
            toon_material,
            world_normal,
            surface_to_camera,
            input.world_position,
            lights_info,
        );
        base_alpha = toon_material.base_color_factor.a;
    } else {
        // PBR material path (default)
        let material = pbr_get_material(material_offset);

        // Sample all PBR material textures and compute material properties
        let material_color = pbr_get_material_color(
            material,
            world_normal,
            world_tangent,
            input
        );

        // Get lighting info
        let lights_info = get_lights_info();

        // Check if we need screen-space transmission
        let metallic = clamp(material_color.metallic_roughness.x, 0.0, 1.0);
        let effective_transmission = material_color.transmission * (1.0 - metallic);

        if(material.debug_bitmask != 0u) {
            color = pbr_debug_material_color(material, material_color);
        } else  if (effective_transmission > 0.0) {
            // Sample transmission background from opaque render
            let roughness = max(clamp(material_color.metallic_roughness.y, 0.0, 1.0), 0.04);
            let transmission_background = sample_transmission_background(
                input.frag_pos,
                input.world_position,
                material_color.normal,
                -surface_to_camera,  // view direction (towards surface)
                material_color.ior,
                roughness,
                material_color.volume_thickness,
                material_color.dispersion,
                camera,
            );

            // Apply lighting with screen-space transmission
            color = apply_lighting_with_transmission(
                material_color,
                surface_to_camera,
                input.world_position,
                lights_info,
                transmission_background,
                (material_mesh_meta.receive_shadows & material_mesh_meta.shadow_receiver_gate),
            );
        } else {
            // Standard lighting without transmission
            color = apply_lighting(
                material_color,
                surface_to_camera,
                input.world_position,
                lights_info,
                (material_mesh_meta.receive_shadows & material_mesh_meta.shadow_receiver_gate),
            );
        }

        base_alpha = material_color.base.a;
    }

    // Apply per-instance tint (Stage-3b — mirrors the opaque non-MSAA path).
    if (input.instance_id != INSTANCE_ATTR_NONE) {
        let attr = instance_attrs[input.instance_id];
        let tint = unpack4x8unorm(attr.color_packed);
        color = color * tint.rgb;
        base_alpha = base_alpha * tint.a * attr.alpha;
    }

    // Output final color with alpha
    let premult_rgb = color * base_alpha;
    out.color = vec4<f32>(premult_rgb, base_alpha);

    return out;
}
