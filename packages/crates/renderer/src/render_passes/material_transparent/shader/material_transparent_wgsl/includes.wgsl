/*************** START math.wgsl ******************/
{% include "shared_wgsl/math.wgsl" %}
/*************** END math.wgsl ******************/

/*************** START meta.wgsl ******************/
{% include "shared_wgsl/vertex/geometry_mesh_meta.wgsl" %}
/*************** END meta.wgsl ******************/

/*************** START camera.wgsl ******************/
{% include "shared_wgsl/camera.wgsl" %}
/*************** END camera.wgsl ******************/

/*************** START frame_globals.wgsl ******************/
{% include "shared_wgsl/frame_globals.wgsl" %}
/*************** END frame_globals.wgsl ******************/

/*************** START transform.wgsl ******************/
{% include "shared_wgsl/vertex/transform.wgsl" %}
/*************** END transform.wgsl ******************/

{% if has_custom_vertex %}
/*************** START custom_vertex.wgsl ******************/
{{ dynamic_vertex_struct_decl|safe }}
{{ dynamic_vertex_loader_decl|safe }}
{% include "shared_wgsl/vertex/custom_vertex.wgsl" %}
/*************** END custom_vertex.wgsl ******************/
{% endif %}

/*************** START morph.wgsl ******************/
{% include "shared_wgsl/vertex/morph.wgsl" %}
/*************** END morph.wgsl ******************/

/*************** START skin.wgsl ******************/
{% include "shared_wgsl/vertex/skin.wgsl" %}
/*************** END skin.wgsl ******************/

/*************** START apply.wgsl ******************/
{% include "shared_wgsl/vertex/apply_vertex.wgsl" %}
/*************** END apply.wgsl ******************/

/*************** START vertex_color.wgsl ******************/
{% include "shared_wgsl/vertex_color.wgsl" %}
/*************** END vertex_color.wgsl ******************/

/*************** START textures.wgsl ******************/
{% include "shared_wgsl/textures.wgsl" %}
/*************** END textures.wgsl ******************/

/*************** START material.wgsl ******************/
{% include "shared_wgsl/material.wgsl" %}
/*************** END material.wgsl ******************/

/*************** START extras.wgsl ******************/
{% include "shared_wgsl/extras.wgsl" %}
/*************** END extras.wgsl ******************/

/*************** START mesh_meta.wgsl ******************/
{% include "shared_wgsl/material_mesh_meta.wgsl" %}
/*************** END mesh_meta.wgsl ******************/

/*************** START light_access_types.wgsl (always — ABI) ******************/
{% include "shared_wgsl/lighting/light_access_types.wgsl" %}
/*************** END light_access_types.wgsl ******************/
{# Transparent keeps the light accessors always-included for now (its fragment
   calls get_lights_info in the PBR/Toon paths); transparent-side light_access
   gating is a Phase-4 follow-up. #}
/*************** START light_access.wgsl ******************/
{% include "shared_wgsl/lighting/light_access.wgsl" %}
/*************** END light_access.wgsl ******************/

{% if inc.apply_lighting %}
/*************** START apply_lighting.wgsl ******************/
{% include "shared_wgsl/lighting/apply_lighting.wgsl" %}
/*************** END apply_lighting.wgsl ******************/
{% endif %}

{% if inc.brdf %}
/*************** START brdf.wgsl ******************/
{% include "shared_wgsl/lighting/brdf.wgsl" %}
/*************** END brdf.wgsl ******************/
{% endif %}

/*************** START texture_uvs.wgsl ******************/
{% include "material_transparent_wgsl/helpers/texture_uvs.wgsl" %}
/*************** END texture_uvs.wgsl ******************/

/*************** START material_color.wgsl ******************/
{% include "material_transparent_wgsl/helpers/material_color_calc.wgsl" %}
/*************** END material_color.wgsl ******************/

/*************** START vertex_color_attrib.wgsl ******************/
{% include "material_transparent_wgsl/helpers/vertex_color_attrib.wgsl" %}
/*************** END vertex_color_attrib.wgsl ******************/
