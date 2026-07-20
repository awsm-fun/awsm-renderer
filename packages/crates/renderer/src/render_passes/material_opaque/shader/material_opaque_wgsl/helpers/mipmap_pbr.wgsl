// mipmap_pbr.wgsl — Tier B (PBR-internal) gradient builder, split out of
// mipmap.wgsl (Phase 2). Returns PbrMaterialGradients from a PbrMaterial; gated
// by inc.material_color_calc. The generic UV-derivative machinery it builds on
// stays in mipmap.wgsl (Tier A). See taxonomy in awsm-materials::shader_includes.
// Computes UV derivatives for each texture type, which are used with textureSampleGrad
// This enables hardware anisotropic filtering in compute shaders
{# Skinny materials: PBR-only (returns PbrMaterialGradients, gated in
   material_color_calc.wgsl). Only compute_material_color calls it. #}
{% if inc.material_color_calc %}
fn pbr_get_gradients(
    barycentric: vec3<f32>,         // (b0, b1, b2)
    bary_derivs: vec4<f32>,         // (db1dx, db1dy, db2dx, db2dy)
    material: PbrMaterial,
    triangle_indices: vec3<u32>,
    attribute_data_offset: u32,
    vertex_attribute_stride: u32,
    uv_sets_index: u32,
    world_normal: vec3<f32>,        // For orthographic anisotropic correction
    view_matrix: mat4x4<f32>        // For orthographic anisotropic correction
) -> PbrMaterialGradients {

    var out : PbrMaterialGradients;

    // Load extension data on-demand for gradient computation
    let specular = pbr_material_load_specular(material.specular_index);
    let transmission = pbr_material_load_transmission(material.transmission_index);
    let volume = pbr_material_load_volume(material.volume_index);
    let clearcoat = pbr_material_load_clearcoat(material.clearcoat_index);
    let sheen = pbr_material_load_sheen(material.sheen_index);
    let diffuse_trans = pbr_material_load_diffuse_transmission(material.diffuse_transmission_index);
    let anisotropy = pbr_material_load_anisotropy(material.anisotropy_index);
    let iridescence = pbr_material_load_iridescence(material.iridescence_index);
    {% if pbr_features.secondary_maps %}
    let secondary = pbr_material_load_secondary_maps(material.secondary_maps_index);
    {% endif %}

    if (material.base_color_tex_info.exists) {
        out.base_color = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            material.base_color_tex_info,
            world_normal,
            view_matrix
        );
    }

    if (material.metallic_roughness_tex_info.exists) {
        out.metallic_roughness = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            material.metallic_roughness_tex_info,
            world_normal,
            view_matrix
        );
    }

    if (material.normal_tex_info.exists) {
        out.normal = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            material.normal_tex_info,
            world_normal,
            view_matrix
        );
    }

    if (material.occlusion_tex_info.exists) {
        out.occlusion = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            material.occlusion_tex_info,
            world_normal,
            view_matrix
        );
    }

    if (material.emissive_tex_info.exists) {
        out.emissive = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            material.emissive_tex_info,
            world_normal,
            view_matrix
        );
    }

    if (specular.tex_info.exists) {
        out.specular = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            specular.tex_info,
            world_normal,
            view_matrix
        );
    }

    if (specular.color_tex_info.exists) {
        out.specular_color = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            specular.color_tex_info,
            world_normal,
            view_matrix
        );
    }

    if (transmission.tex_info.exists) {
        out.transmission = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            transmission.tex_info,
            world_normal,
            view_matrix
        );
    }

    if (volume.thickness_tex_info.exists) {
        out.volume_thickness = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            volume.thickness_tex_info,
            world_normal,
            view_matrix
        );
    }

    // Clearcoat textures
    if (clearcoat.tex_info.exists) {
        out.clearcoat = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            clearcoat.tex_info,
            world_normal,
            view_matrix
        );
    }

    if (clearcoat.roughness_tex_info.exists) {
        out.clearcoat_roughness = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            clearcoat.roughness_tex_info,
            world_normal,
            view_matrix
        );
    }

    if (clearcoat.normal_tex_info.exists) {
        out.clearcoat_normal = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            clearcoat.normal_tex_info,
            world_normal,
            view_matrix
        );
    }

    // Sheen textures
    if (sheen.color_tex_info.exists) {
        out.sheen_color = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            sheen.color_tex_info,
            world_normal,
            view_matrix
        );
    }

    if (sheen.roughness_tex_info.exists) {
        out.sheen_roughness = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            sheen.roughness_tex_info,
            world_normal,
            view_matrix
        );
    }

    // KHR_materials_diffuse_transmission
    if (diffuse_trans.tex_info.exists) {
        out.diffuse_transmission = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            diffuse_trans.tex_info,
            world_normal,
            view_matrix
        );
    }

    if (diffuse_trans.color_tex_info.exists) {
        out.diffuse_transmission_color = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            diffuse_trans.color_tex_info,
            world_normal,
            view_matrix
        );
    }

    // KHR_materials_anisotropy
    if (anisotropy.tex_info.exists) {
        out.anisotropy = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            anisotropy.tex_info,
            world_normal,
            view_matrix
        );
    }

    // KHR_materials_iridescence
    if (iridescence.tex_info.exists) {
        out.iridescence = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            iridescence.tex_info,
            world_normal,
            view_matrix
        );
    }

    if (iridescence.thickness_tex_info.exists) {
        out.iridescence_thickness = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset, vertex_attribute_stride,
            uv_sets_index,
            iridescence.thickness_tex_info,
            world_normal,
            view_matrix
        );
    }

    {% if pbr_features.secondary_maps %}
    // Secondary / detail maps (engine extension) — one gradient per bound slot.
    if (secondary.base_color_tex_info.exists) {
        out.secondary_base_color = get_uv_derivatives(
            barycentric, bary_derivs, triangle_indices,
            attribute_data_offset, vertex_attribute_stride, uv_sets_index,
            secondary.base_color_tex_info, world_normal, view_matrix
        );
    }
    if (secondary.normal_tex_info.exists) {
        out.secondary_normal = get_uv_derivatives(
            barycentric, bary_derivs, triangle_indices,
            attribute_data_offset, vertex_attribute_stride, uv_sets_index,
            secondary.normal_tex_info, world_normal, view_matrix
        );
    }
    if (secondary.metallic_roughness_tex_info.exists) {
        out.secondary_metallic_roughness = get_uv_derivatives(
            barycentric, bary_derivs, triangle_indices,
            attribute_data_offset, vertex_attribute_stride, uv_sets_index,
            secondary.metallic_roughness_tex_info, world_normal, view_matrix
        );
    }
    if (secondary.occlusion_tex_info.exists) {
        out.secondary_occlusion = get_uv_derivatives(
            barycentric, bary_derivs, triangle_indices,
            attribute_data_offset, vertex_attribute_stride, uv_sets_index,
            secondary.occlusion_tex_info, world_normal, view_matrix
        );
    }
    if (secondary.emissive_tex_info.exists) {
        out.secondary_emissive = get_uv_derivatives(
            barycentric, bary_derivs, triangle_indices,
            attribute_data_offset, vertex_attribute_stride, uv_sets_index,
            secondary.emissive_tex_info, world_normal, view_matrix
        );
    }
    {% endif %}

    return out;
}
{% endif %}{# end inc.material_color_calc (pbr_get_gradients) #}
