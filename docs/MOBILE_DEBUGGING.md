Example prompt:


After each code change, validate with:

`task debug-mobile:chrome-check`

That command reloads the real Android Chrome tab through CDP, captures console/runtime logs, and exits nonzero if the WebGPU/Vulkan renderer error still appears.
