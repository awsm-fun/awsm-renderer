# verify: env-bc6h-spheres  (BC6H environment cubemaps + skybox + roughness ladder)

Layer-A only, like `aa-edges`: this scene's subject is the *skybox render path*
and *BC6H environment loading*, and it doubles as the MSAA silhouette probe —
none of which is player-bundle state. There is no `bundle/` and no golden.

## Prerequisites

Environment assets served at `http://localhost:9083/cyber_bc6h/`
(`skybox.ktx2`, `env.ktx2`, `irradiance.ktx2`) — i.e. present in the
`test-assets` repo, which `task mcp-dev` serves on :9083.

Rebake from a source HDRI with:

```sh
cmgen -s 64   -f exr --ibl-irradiance=ibl-irradiance HDRi_Scifi_011_4K.hdr
cmgen -s 512  -f exr --ibl-ld=ibl-env                HDRi_Scifi_011_4K.hdr
cmgen -s 1024 -f exr -x skybox                       HDRi_Scifi_011_4K.hdr

cargo run --release -p awsm-renderer-env-bake-cli -- \
    --skybox-faces     skybox/HDRi_Scifi_011_4K \
    --specular-faces   ibl-env/HDRi_Scifi_011_4K \
    --irradiance-faces ibl-irradiance/HDRi_Scifi_011_4K \
    --out <test-assets>/cyber_bc6h --format bc6h
```

Skybox cube size = source equirect width / 4 (a cube face spans 90° of a 360°
panorama), so a 4K source gives 1024. Going higher only interpolates. See
`docs/DEVELOPMENT.md`.

## drive

1. Replay `author.js` (5 spheres + floor, BC6H env on all three slots, camera
   pinned at yaw 2.35 so the dense skyline sits behind the silhouettes).
2. `wait_render_settled`, then screenshot.

## expect

- **Skybox varies by view direction.** Orbit the camera: the background tracks.
  A single flat colour at every angle is the reverse-Z NaN-ray regression
  (`sample_skybox` unprojecting the far plane, which under infinite-far
  reverse-Z is at infinity → `w == 0` → NaN → one implementation-defined texel).
- **Roughness ladder reads monotonically** across the three metals, right to
  left: chrome resolves individual windows and signs → brushed smears them into
  streaks → satin-metal blurs to broad tonal blocks. That walks mip 0 → ~2 of
  cmgen's 6 `--ibl-ld` bands via `roughness * max_mip`. Non-monotonic, or all
  three identical, means the specular mip selection or band count is wrong
  (`env.ktx2` must have exactly 6 levels).
- **matte-red picks up a warm bounce** from the orange city lights rather than
  reading flat grey — `irradiance.ktx2` doing its job (sampled at mip 0, ×π).
- **All three slots report `kind:"ktx"`** in `snapshot.project.environment`.
- Console shows `Using compressed texture format Bc6hRgbUfloat` **3 times**,
  with no texture errors.

## MSAA silhouette probe

The framing (high-frequency neon directly behind every silhouette) makes this
the sharpest available probe for the MSAA edge path. Two checks, both verified
on this scene:

**Stability** — static camera, 3 consecutive live-canvas captures, pixel-diff:
**0 changed pixels** with MSAA 4x. Any nonzero count in the geometry↔sky band
means an edge-accumulator regression (see
`wgsl_validation::skybox_edge_accumulator_stride_tracks_the_ssr_axis`).

**Quality** — against a supersampled ground truth
(`set_view_options { render_scale: 2.0, msaa: false }`), RMS luma error over
silhouette pixels:

| camera | RMS no-AA | RMS MSAA 4x | error reduction |
|---|---|---|---|
| yaw 2.35 / pitch 0.0 | 38.14 | 17.22 | 54.8% |
| yaw 0.9 / pitch 0.45 | 35.30 | 15.54 | 56.0% |

~55% is the expected band for 4x MSAA. Measure with a smooth `sky_gradient`
environment, not the HDRI — a detailed sky is full of high-contrast *texture*
edges that MSAA does not antialias, and they swamp the metric (an
11%-of-frame mask reads as "MSAA barely helps", which is a mask artifact).

Flip any structural view option (`msaa`, `render_scale`) with
`wait_render_settled` after — they recompile pipelines / rebuild targets, and
fixed sleeps race the rebuild.

## fail

- Flat single-colour background at all camera angles → skybox ray reconstruction.
- Flicker along geometry↔sky silhouettes with a static camera → edge accumulator.
- All metals identical → specular mip selection / `env.ktx2` level count.
- `Bc6hRgbUfloat` absent from the console → BC6H not loading; check the device
  reports `texture-compression-bc`.
