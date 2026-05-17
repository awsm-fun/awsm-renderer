// Contains the final material properties after sampling all PBR textures.
// IMPORTANT: The 'normal' field contains the perturbed normal (with normal map applied),
// NOT the geometry normal. Always use material_color.normal for lighting calculations!
struct PbrMaterialColor {
    base: vec4<f32>,
    metallic_roughness: vec2<f32>,
    normal: vec3<f32>,  // Perturbed normal from normal mapping (use this for lighting!)
    occlusion: f32,
    emissive: vec3<f32>,
    specular: f32,           // KHR_materials_specular: strength factor (default 1.0)
    specular_color: vec3<f32>, // KHR_materials_specular: F0 color modifier (default white)
    // KHR_materials_ior
    ior: f32,
    // KHR_materials_transmission
    transmission: f32,
    // KHR_materials_volume
    volume_thickness: f32,
    volume_attenuation_distance: f32,
    volume_attenuation_color: vec3<f32>,
    // KHR_materials_clearcoat
    clearcoat: f32,              // Clearcoat layer intensity (0.0 = none, 1.0 = full)
    clearcoat_roughness: f32,    // Roughness of clearcoat layer
    clearcoat_normal: vec3<f32>, // Normal for clearcoat layer (may differ from base normal)
    // KHR_materials_sheen
    sheen_color: vec3<f32>,      // Sheen color at grazing angles
    sheen_roughness: f32,        // Sheen roughness (affects sheen lobe width)
};

fn pbr_debug_material_color(material: PbrMaterial, color: PbrMaterialColor) -> vec3<f32> {
    if(pbr_debug_base_color(material.debug_bitmask)) {
        return color.base.rgb;
    }
    if(pbr_debug_metallic_roughness(material.debug_bitmask)) {
        // R = metallic, G = roughness, B = 0
        return vec3<f32>(color.metallic_roughness.x, color.metallic_roughness.y, 0.0);
    }
    if(pbr_debug_normals(material.debug_bitmask)) {
        // Remap normal from [-1,1] to [0,1] for visualization
        return color.normal * 0.5 + 0.5;
    }
    if(pbr_debug_occlusion(material.debug_bitmask)) {
        // Show occlusion as grayscale
        return vec3<f32>(color.occlusion, color.occlusion, color.occlusion);
    }
    if(pbr_debug_emissive(material.debug_bitmask)) {
        return color.emissive;
    }
    if(pbr_debug_specular(material.debug_bitmask)) {
        // Show specular color modulated by specular strength
        return color.specular_color * color.specular;
    }

    // This function was only called behind a gate, so we should never reach here.
    // return magenta to signal error
    return vec3<f32>(
        1.0,
        0.0,
        1.0
    );
}
