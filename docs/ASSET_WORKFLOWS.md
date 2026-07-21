# Asset workflows (environment · textures · materials · displacement · purge)

**Golden rule: assets come from URLs (or the editor's file picker) — never inline
base64.** There is no `create_texture` tool and no `equirect` environment param.
Anything you author (a texture, a heightmap, a panorama) you generate, **host at an
`http(s)` URL**, then reference by URL. The editor (WASM) fetches it at apply time
and, on Save, embeds the bytes into the project so it survives reload with no server.

Serving locally: any static server with permissive CORS works, e.g.
`python3 -m http.server 8000` (add CORS headers if cross-origin) — a
`http://127.0.0.1:PORT/...` URL loads fine from the localhost editor.

---

## 1. Environment (skybox + IBL)

The environment has **three independent slots**, each set separately:
- **skybox** — the background cubemap the camera sees.
- **specular** — the prefiltered / roughness-mipped IBL map that drives reflections
  ("prefiltered env" and "specular" are the same thing).
- **irradiance** — the diffuse-convolved IBL map that drives ambient light.

Omit a slot to leave it unchanged; pass `"builtin"` (aliases: `"builtin_default"`, `"built_in_default"` — the form the exporter serializes) to reset it to the default sky.
Two ways, pick by need:

### a) Two-color sky gradient — no hosting, instant
`set_environment { zenith:[r,g,b], nadir:[r,g,b] }` (linear RGB). Sets ALL THREE
slots from the same gradient. Great for dusk / overcast / studio.

### b) KTX2 cubemaps by URL — full HDRI / studio lighting / chrome
A `.ktx2` cubemap is **baked offline** from an `.hdr`/`.exr` and served. You author
the panorama however you like — including generating an **equirectangular** one
procedurally (numpy → a flat-RGBE Radiance `.hdr`) — then project it to a cubemap
**offline** with filament `cmgen` (this is where equirect→cubemap happens; there is
no runtime equirect projection). Pipeline (full flags in `docs/DEVELOPMENT.md`):

1. Make an `.hdr`/`.exr` (procedural or a real HDRI). **Check it actually has HDR
   range** — a `.hdr` extension only means the container is float, and plenty of
   "HDRIs" are tonemapped LDR re-saved as RGBE (max channel ≈ 1.0). Those bake
   into flat IBL no matter how carefully you pack them.
2. `cmgen` → three face sets:
   - skybox faces:      `cmgen -s 2048 -f exr -x skybox my.hdr`
   - prefiltered spec:  `cmgen -s 512 -f exr --ibl-ld=ibl-env my.hdr` (6 roughness mips)
   - irradiance:        `cmgen -s 64 -f exr --ibl-irradiance=ibl-irradiance my.hdr`
3. `awsm-renderer-env-bake --skybox-faces … --specular-faces … --irradiance-faces …
   --out … --format bc6h` → `skybox.ktx2`, `env.ktx2`, `irradiance.ktx2`.
   BC6H stays block-compressed in VRAM at 1 byte/texel vs `B10G11R11`'s 4 (a 4x
   saving), under the `texture-compression-bc` feature the renderer already
   requests. `--format rg11b10` gives the uncompressed fallback; the raw
   `ktx create --cubemap --format B10G11R11_UFLOAT_PACK32 …` recipes still work
   for that variant but cannot produce BC6H.
   **Never `--encode uastc` / `--encode basis-lz` for these cubemaps** — both
   write a supercompressed KTX2, which the cubemap loader rejects outright, and
   both are LDR codecs that would clip everything above 1.0. `KHR_texture_basisu`
   transcoding is for glTF *material* textures, not environment maps.
4. Serve them, then:
   `set_environment { skybox:"<url>/skybox.ktx2", specular:"<url>/env.ktx2", irradiance:"<url>/irradiance.ktx2" }`

Notes:
- **Slots are fully independent.** Set only the ones you want; the rest stay put.
  E.g. keep the default-sky irradiance and override *only* `specular`, or set a
  clean `skybox:"builtin"` while pointing `specular`/`irradiance` at a studio KTX.
- The **skybox can differ from the IBL** — a clean grey skybox + a studio specular/
  irradiance gives a chrome ball its reflections while the punctual (point/dir)
  lights own the primary specular hotspot. (`zenith`/`nadir` sets all three slots
  to the same gradient; to get a gradient *look* with a separate KTX IBL, bake the
  gradient into a skybox `.ktx2` too.)
- Each of `skybox` / `specular` / `irradiance` also accepts `"builtin"` / `"builtin_default"` / `"built_in_default"` (or omit)
  for the default sky, an existing KTX cubemap asset UUID, or a `https://` `.ktx2` URL.
- Read what's set via `get_snapshot` → `project.environment` (per-slot kind + asset).

---

## 2. Textures → built-in PBR material slots

The rule: **the material asset owns the pipeline, the node owns data.** Texture
binds are pure data: every core slot's sampling code is always compiled (an
unbound slot samples a shared 1×1 neutral — glTF's defined no-texture result),
so binding an image to any slot on any node never recompiles anything.

1. Author + host the image (PNG/JPEG). Generate normal/roughness/occlusion maps as
   needed (they're `linear` data, not sRGB).
2. `import_texture_from_url { url }` → returns a texture asset id. (Persists on Save.)
3. Bind per-node (any of the five slots, freely):
   `set_node_texture { node, slot: base_color|metallic_roughness|normal|occlusion|emissive, texture: <id> }`
   (writes the node's per-mesh inline slot). Or bind a default image on the
   library material (`update_builtin_material`) to give every user of the
   material a starting texture that nodes can override. The mesh inspector
   shows pickers for all five core slots.
4. Tune sampling: `set_node_texture_transform { node, slot, offset|scale|rotation|flow|
   wrap_u|wrap_v|mag_filter|min_filter|mipmap_filter|uv_set }` (patch-style; slot must
   already have a texture bound).
5. Scalars/colors (per node): `set_builtin_param { node, param: base_color|metallic|roughness|
   emissive|normal_scale|occlusion_strength|…, value:[…] }`.
   Alpha MODE (opaque/mask/blend) is pipeline routing and lives on the MATERIAL:
   `set_builtin_alpha_mode { material, mode }` — per glTF, opaque ignores base-color
   alpha (no silent blend promotion); the mask cutoff VALUE stays per-node tunable.
   Extension ENABLES likewise live on the material (`update_builtin_material`);
   per-node inline extensions only carry parameters.

---

## 3. Custom (dynamic WGSL) materials

`add_custom_material` → `set_material_wgsl` / `set_material_layout` (declare uniform /
texture / buffer slots) / `set_material_includes` / `set_material_fragment_inputs`.
Read `get_material_contract` FIRST and check `get_material_diagnostics` after editing.
See resources `awsm://docs/material-contract-*` and `awsm://docs/material-recipes`.

- Shared defaults: `set_material_uniform` (asset-wide), `set_material_double_sided`,
  `set_material_alpha_mode`.
- Per-mesh overrides on a node assigned a custom material:
  `set_material_texture { node, slot, texture }` (bind a texture slot),
  `set_material_buffer { node, slot, … }`,
  `set_node_material_uniform { node, name, value:{kind,value} }` (per-mesh uniform,
  distinct from the shared `set_material_uniform`).

---

## 4. Mesh displacement from a heightmap

`displace_from_texture { node, url, strength }` — a **mesh edit** (§16), not a
material. Author a heightmap PNG (eroded terrain, a stamped logo, fbm…), host it,
pass the URL. Each vertex moves along its normal by `luminance(heightmap @ UV) *
strength`. Needs a UV-mapped, sufficiently **tessellated** mesh (subdivide a plane
via `set_mesh_modifiers` first — displacement only moves existing verts). Undoable;
verify with `get_mesh_stats` (bbox grows) or a screenshot.

---

## 5. Purge unused assets

`purge_unused` — deletes every texture / material / mesh / buffer NOT referenced by
the live scene (node bindings, environment KTX, animation targets — transitively).
One undoable step; an in-use asset is never removed. Run it after swapping assets to
drop orphans. (Editor UI: hamburger menu → "Purge unused assets".) Verify with
`get_snapshot`.

---

## 6. Advanced / long-tail material fields (escape hatch)

Uncommon built-in fields have no dedicated tool but are still reachable:

- **Per-node inline fields** (any field on the node's `Mesh`/material instance —
  KHR extension factors like ior/specular/transmission/clearcoat/sheen, extension
  texture slots, flipbook grid, shading-model switch, double_sided, vertex_colors):
  `patch_kind { node, patch:{…} }` (RFC-7386 merge-patch over the node's kind JSON).
  Discover the exact shape with `get_kind_schema` first, and read the current values
  with `get_node_details`.
- **Shared material variant defaults**: `update_builtin_material` (full `MaterialDef`
  JSON on the asset) or `dispatch_command` for anything without a typed tool.

Prefer the typed tools above for the common path; reach for `patch_kind` /
`get_kind_schema` only for the tail.
