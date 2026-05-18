# Task

Your task is to fix all of the extension listed below in the `Extensions` section such that the local test model pages work.

That is, specifically:

- TextureTransform
- Dispersion
- DiffuseTransmission
- Anisotropy
- Iridescence
- Lights

You must update this file to track your work in the `Updates` section at the end, and collect any notes that may be required for further investigation

# Scope

If you need to explore other pages than the ones listed, in order to ensure we haven't introduced more problems, please do.

If you need to adjust the settings on the local test page, such as turning off direct lighting or using a different environment map, please do.

If it helps you to run a different dev server, e.g. the editor site, please do so (it should be easy to deduce how to manage that from the Taskfiles)

# Working files

It's okay to have errors while you are working, but at the end, once all your changes are all finished, everything should be fixed (as well as `cargo fmt` and `cargo clippy --workspace`)

There is no need to push separate commits, unless it will help you bisect. It's okay for things to be broken between commits if you do.

# Guidelines

Please consider efficiency and code clarity top priorities. This is a complex renderer for a game engine, small performance differences matter, as well as being able to reason about the code.

Performance matters a great deal, if you need to research techniques and deeply consider the best way to do things, please do.

However, our target is browsers with WebGPU. We should *not* need to go beyond typical browser settings, and ideally the renderer should work on mobile devices. For example, we should not add an additional color target to the geometry output which would push it to 5, beyond the 4 typically allowed on mobile devices.

At a high level, many of the test scenes use *multiple* extensions. Please keep that in mind - you may need to test other scenes than the ones listed to ensure nothing unexpected broke.

# Setup

Run the dev server via `task model-tests:dev`

This allows you to visit the "model tests" site at http://127.0.0.1:9080/ in your preview browser and it will rebuild/reload on changes

The model test site is itself not 100% perfect in terms of the camera frustum cutoffs - you may need to adjust those values

# Extensions

Each of the following sections lists a GLTF extension as well as:

1. The official spec reference page
2. Some local test model page
3. The corresponding test model README on the GLTF extensions which contains an example image and, sometimes, explanation of specific errors
4. Additional notes

## TextureTransform

* Reference page: https://github.com/KhronosGroup/glTF/blob/main/extensions/2.0/Khronos/KHR_texture_transform/README.md
* Local test model page 1 (regular): http://127.0.0.1:9080/app/model/TextureTransformTest 
* Test model README 1 (regular): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/TextureTransformTest 
* Local test model page 2 (multitest): http://127.0.0.1:9080/app/model/TextureTransformMultiTest
* Test model README 2 (multitest): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/TextureTransformMultiTest 

Notes:

This is partially implemented, `test 1 (regular)` looks fine, but if you compare `test 2 (multitest)` with the screenshots in the README, you can see errors in:

* Normal
* Occlusion
* Clearcut Normal

## Dispersion

* Reference page: https://github.com/KhronosGroup/glTF/blob/main/extensions/2.0/Khronos/KHR_materials_dispersion/README.md
* Local test model page 1 (compare): http://127.0.0.1:9080/app/model/CompareDispersion
* Test model readme 1 (compare): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/CompareDispersion
* Local test 2 (sequence): http://127.0.0.1:9080/app/model/DispersionTest
* Test model readme 2 (sequence): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/DispersionTest
* Local test 3 (dragon): http://127.0.0.1:9080/app/model/DragonDispersion
* Test model readme 3 (dragon): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/DragonDispersion

## Diffuse Transmission

* Reference page: https://github.com/KhronosGroup/glTF/blob/main/extensions/2.0/Khronos/KHR_materials_diffuse_transmission/README.md
* Local test model 1 (basic): http://127.0.0.1:9080/app/model/DiffuseTransmissionTest
* test model readme 1 (basic): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/DiffuseTransmissionTest
* Local test model 2 (teacup): http://127.0.0.1:9080/app/model/DiffuseTransmissionTeacup
* test model readme 2 (teacup): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/DiffuseTransmissionTeacup
* Local test model 3 (plant): http://127.0.0.1:9080/app/model/DiffuseTransmissionPlant
* test model readme 3 (plant): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/DiffuseTransmissionPlant

## Anisotropy

* Reference page: https://github.com/KhronosGroup/glTF/blob/main/extensions/2.0/Khronos/KHR_materials_anisotropy/README.md
* Local model 1 (compare): http://127.0.0.1:9080/app/model/CompareAnisotropy 
* test model readme 1 (compare): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/CompareAnisotropy
* Local model 2 (barn lamp): http://127.0.0.1:9080/app/model/AnisotropyBarnLamp
* test model readme 2 (barn lamp): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/AnisotropyBarnLamp
* Local model 3 (disc test): http://127.0.0.1:9080/app/model/AnisotropyDiscTest
* test model readme 3 (disc test): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/AnisotropyDiscTest
* Local model 4 (rotation test): http://127.0.0.1:9080/app/model/AnisotropyRotationTest
* test model readme 4 (rotation test): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/AnisotropyRotationTest
* Local model 5 (strength test): http://127.0.0.1:9080/app/model/AnisotropyStrengthTest
* test model readme 5 (strength test): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/AnisotropyStrengthTest

## Iridescence

* Reference page: https://github.com/KhronosGroup/glTF/blob/main/extensions/2.0/Khronos/KHR_materials_iridescence/README.md
* Local model 1 (compare): http://127.0.0.1:9080/app/model/CompareIridescence 
* test model readme 1 (compare): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/CompareIridescence
* Local model 2 (abalone): http://127.0.0.1:9080/app/model/IridescenceAbalone
* test model readme 2 (abalone): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/IridescenceAbalone
* Local model 3 (dielectric spheres): http://127.0.0.1:9080/app/model/IridescenceDielectricSpheres
* test model readme 3 (dielectric spheres): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/IridescenceDielectricSpheres
* Local model 4 (lamp): http://127.0.0.1:9080/app/model/IridescenceLamp
* test model readme 4 (lamp): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/IridescenceLamp
* Local model 5 (metallic spheres): http://127.0.0.1:9080/app/model/IridescenceMetallicSpheres
* test model readme 5 (metallic spheres): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/IridescenceMetallicSpheres
* Local model 6 (suzanne): http://127.0.0.1:9080/app/model/IridescenceSuzanne
* test model readme 6 (suzanne): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/IridescenceSuzanne
* Local model 7 (dish with olives): http://127.0.0.1:9080/app/model/IridescenceDishWithOlives
* test model readme 7 (dish with olives): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/IridescentDishWithOlives

## Lights

* Reference page: https://github.com/KhronosGroup/glTF/blob/main/extensions/2.0/Khronos/KHR_lights_punctual/README.md
* Local model 1 (point light intensity): http://127.0.0.1:9080/app/model/PointLightIntensityTest 
* test model readme 1 (point light intensity): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/PointLightIntensityTest
* Local model 2 (playset): http://127.0.0.1:9080/app/model/PlaysetLightTest 
* test model readme 2 (playset): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/PlaysetLightTest
* Local model 3 (punctual lamp): http://127.0.0.1:9080/app/model/LightsPunctualLamp 
* test model readme 3 (punctual lamp): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/LightsPunctualLamp
* Local model 4 (directional light): http://127.0.0.1:9080/app/model/DirectionalLight
* test model readme 4 (directional light): https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/DirectionalLight

Notes:

* This is partially implemented, but hasn't been tested thoroughly, and some capabilities may be missing or wrong

# Updates

- [ ] TextureTransform
- [ ] Dispersion
- [ ] DiffuseTransmission
- [ ] Anisotropy
- [ ] Iridescence
- [ ] Lights
