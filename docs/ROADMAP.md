# Next up
- Temporal shaders ✅ — renderer-wide `FrameGlobals` uniform (`time`,
  `delta_time`, `frame_count`, `resolution`) bound alongside the camera
  in every shading pass + a first-party `FlipBook` sprite-sheet
  material as its load-bearing consumer. Particle bridge migrated to
  read `delta_time` from `frame_globals()` so pause / time-scale /
  replay flow through automatically.
- Make transparent meshes pickable
  - Maybe global "editor_mode" on renderer that toggles some less-efficient behavior
    - All meshes get both transparent and opaque geometry 
    - Picking pass samples both and uses alpha test to discard transparent fragments below threshold

- Font rendering
    - https://github.com/EricLengyel/Slug
    
- Shadows ✅ — directional CSM (cascade-array texture) + spot + point cube; PCF / PCSS (including real cube blocker search); SSCS; temporal throttle on the far cascade; EVSM hybrid (moment-write compute + 2× Gaussian blur), per-cascade skip-unchanged.
  - Transparent-pass shadow bind-group consolidation (deferred; blocked on adapter `maxBindGroups=4`)
  - Editor UI for the shadow controls panel (schema fields are editable in `project.json`)

- SSAO
  - Can probably be done in opaque pass 
  - Should be optional/togglable

- Animation support in sidebar (or get rid of it)

- Light culling 
  - research best practices
  - should optimize for opaque pass (i.e. only light fragments that made it to the screen?)
  - MAYBE:
    - One draw call
    - Divides the screen into tiles (e.g., 16x16 pixels)
    - For each tile, build a list of lights that affect that region of the screen.
    - Write list of lights to storage buffer, indexed by tile

- make it easier to configure initial sizes for dynamic buffers
  - derive from scanning gltf?

# Multithreading

Dynamic/Uniform storages could be SharedArrayBuffer
Requires more design/thought (don't want to expose raw manipulation)

## Approach

High-level, approach is to keep going through gltf models, one at a time, making them each work starting with minimal and feature-tests.

As more features are added, support is added into the core engine.

## GLTF Support

If it's supported here, corresponding core functionality is also supported

- Loaders
    - [x] document
    - [x] buffers
    - [x] images
- Caching
    - [x] Shaders by ShaderKey
    - [x] RenderPipelines by RenderPipelineKey
    - [x] PipelineLayouts by PipelineLayoutKey
- Accessors
    - [x] basic
    - [x] sparse
- Hierarchy
    - [x] transforms
    - [x] scene graph
- Geometry
    - [x] positions
    - [x] morphing
    - [x] skinning
        - multiple sets (as many as fit in attribute slots)
- Animation
    - [x] morph targets (keyable by MorphKey)
    - [x] transform targets (keyable by TransformKey)
    - [x] linear interpolation
    - [x] step interpolation
    - [x] cubic spline interpolation
- Extensions
    - https://github.com/KhronosGroup/glTF/tree/main/extensions/2.0/Khronos
    - [x] EXT_mesh_gpu_instancing
    - [x] KHR_materials_unlit
    - [x] KHR_materials_emissive_strength
    - [x] KHR_materials_clearcoat
    - [x] KHR_materials_sheen
    - [x] KHR_materials_specular
    - [x] KHR_materials_transmission
    - [x] KHR_materials_volume
    - [x] KHR_materials_dispersion
    - [x] KHR_materials_diffuse_transmission
    - [x] KHR_materials_anisotropy
    - [x] KHR_materials_iridescence
    - [x] KHR_materials_ior
    - [x] KHR_texture_transform
    - [x] KHR_lights_punctual
        - [x] Directional
        - [x] Point
        - [x] Spot
- Materials
    - [x] PBR metallic-roughness
        - [x] base color
        - [x] metallic
        - [x] roughness
        - [x] normal
        - [x] occlusion
        - [x] emissive
    - [x] mipmaps
- Lighting
    - [x] IBL
        - [x] diffuse irradiance
        - [x] specular prefiltered
        - [x] BRDF LUT
    - [x] punctual lights


## Drawing
- [x] non-indexed
- [x] indexed
- [x] instancing
- [x] Opaque front to back
- [x] Transparent back to front
- [x] Anti-aliasing
  - [x] MSAA
  - [x] SMAA
- [ ] SSAO

## Textures
- [x] 2D textures
- [x] Mipmaps

## Skybox
- [x] load ktf
- [x] generate colors
- [x] generate pseudo-sky

## IBL Helpers
- [x] Load ktf
- [x] generate colors
- [x] Document third-party tooling and easy flow

## Animation system
- [x] Players
    - [x] speed control
    - [x] loop control
    - [x] play/pause
        - [ ] test
    - [x] direction
        - [ ] test
- [x] Clips and samplers (see gltf features for details)
- [ ] Events

## Post-processing
- [x] Basic render-texture support
- [x] Tonemapping
- [x] Bloom
- [x] TAA
- [x] DOF

## Demo-only

- [x] Camera
    - [x] Orbit controls
    - [x] Orthographic
    - [x] Perspective
    - [x] Initial fit for AABB (not perfect, but good enough)

## Optimizations

- [ ] Multithreading
- [x] Texture pools 
- [x] Dynamic buffer primitives
    - Single gpu binding
    - Offset-driven
    - Allows insertions and deletions at runtime
    - Separate CPU vs. GPU updates
    - Fixed and flexible (buddy) modes
    - [ ] Evaluate if mapped buffers helps here
- [x] Transforms
    - One dynamic uniform bind group
    - Dirty flag
- [x] AABB
    - Only update when transform changes
- [x] Morphs and Skins
    - Global buffers and dirty flags
- [x] Meshes
    - One vertex buffer
    - One index buffer
    - Gpu gating on dirty flag
- [x] Transform instancing
    - One dynamic uniform bind group for transforms
    - Gpu gating on dirty flag
- Camera
    - [x] Single uniform buffer
    - [x] Frustum culling
