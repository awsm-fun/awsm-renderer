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

Two ways, pick by need:

### a) Two-color sky gradient — no hosting, instant
`set_environment { zenith:[r,g,b], nadir:[r,g,b] }` (linear RGB). Drives BOTH the
skybox and the IBL from the same gradient. Great for dusk / overcast / studio.

### b) KTX2 cubemaps by URL — full HDRI / studio lighting / chrome
A `.ktx2` cubemap is **baked offline** from an `.hdr`/`.exr` and served. You author
the panorama however you like — including generating an **equirectangular** one
procedurally (numpy → a flat-RGBE Radiance `.hdr`) — then project it to a cubemap
**offline** with filament `cmgen` (this is where equirect→cubemap happens; there is
no runtime equirect projection). Pipeline (full flags in `docs/DEVELOPMENT.md`):

1. Make an `.hdr`/`.exr` (procedural or a real HDRI).
2. `cmgen` → three face sets:
   - skybox faces:      `cmgen -s 2048 -f exr -x skybox my.hdr`
   - prefiltered spec:  `cmgen -s 512 -f exr --ibl-ld=ibl-env my.hdr` (6 roughness mips)
   - irradiance:        `cmgen -s 64 -f exr --ibl-irradiance=ibl-irradiance my.hdr`
3. `ktx create --cubemap --format B10G11R11_UFLOAT_PACK32 …` → `skybox.ktx2`,
   `env.ktx2` (prefiltered, `--levels 6`, all mip faces), `irradiance.ktx2`.
4. Serve them, then:
   `set_environment { skybox:"<url>/skybox.ktx2", ibl_prefiltered:"<url>/env.ktx2", ibl_irradiance:"<url>/irradiance.ktx2" }`

Notes:
- **KTX IBL needs BOTH** `ibl_prefiltered` AND `ibl_irradiance`.
- The **skybox can differ from the IBL** — a clean grey skybox + a studio IBL gives
  a chrome ball its reflections while the punctual (point/dir) lights own the primary
  specular hotspot. (`zenith`/`nadir` forces skybox==IBL; to get a gradient *look* with
  a separate KTX IBL, bake the gradient into a skybox `.ktx2` too.)
- Each of `skybox` / `ibl_prefiltered` / `ibl_irradiance` also accepts `"builtin"`
  (or omit) for the default, or an existing KTX texture-asset UUID.

---

## 2. Textures → built-in PBR material slots

1. Author + host the image (PNG/JPEG). Generate normal/roughness/occlusion maps as
   needed (they're `linear` data, not sRGB).
2. `import_texture_from_url { url }` → returns a texture asset id. (Persists on Save.)
3. Assign it to a slot on a mesh's **built-in** material:
   `set_node_texture { node, slot: base_color|metallic_roughness|normal|occlusion|emissive, texture: <id> }`
   (writes the node's per-mesh inline slot). In the editor UI, the mesh inspector's
   material section now shows a picker for all five core slots (empty or not).
4. Tune sampling: `set_node_texture_transform { node, slot, offset|scale|rotation|flow|
   wrap_u|wrap_v|mag_filter|min_filter|mipmap_filter|uv_set }` (patch-style; slot must
   already have a texture bound).
5. Scalars/colors: `set_builtin_param { node, param: base_color|metallic|roughness|
   emissive|normal_scale|occlusion_strength|…, value:[…] }`;
   `set_builtin_alpha_mode` for opaque/mask/blend.

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
