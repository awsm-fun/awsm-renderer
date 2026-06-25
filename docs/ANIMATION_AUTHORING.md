# Animation Authoring over MCP

How to build keyframe animation — clips, tracks, keyframes — and play it back.
This complements the tool list in [`MCP.md`](MCP.md) and the
[`AGENT_GUIDE.md`](AGENT_GUIDE.md) loop.

## Model

- A **clip** is a named, timed container of tracks (`add_clip` → returns a clip
  id). Clip-level props: `set_clip_duration { clip, value }` (seconds),
  `set_clip_speed { clip, value }`, `set_clip_loop { clip, loop_style }`
  (`once | loop | ping_pong`).
- A **track** animates **one target** over time (`add_track { clip, target }`).
  Tracks are referenced afterward by their **index** within the clip (0-based,
  in the order added).
- A **keyframe** is a `(time, value)` sample on a track (`add_keyframe { clip,
  track, t, value }`). Edit one with `set_keyframe { clip, track, index, t?,
  value?, interp? }`; remove with `delete_keyframe`.
- Read a track back with `get_track_data { clip, track }` to verify.

### Track targets (`target`)

`target.kind` selects what's animated; other fields depend on the kind:

| kind | fields | animates |
|---|---|---|
| `transform` | `node`, `prop` = `translation` \| `rotation` \| `scale` | a node's TRS |
| `uniform` | `material`, `name` | a custom material's uniform slot |
| `builtin_param` | `node`, `param` (e.g. `base_color`, `emissive`, `metallic`) | a built-in PBR param |
| `light` | `node`, `param` (e.g. `intensity`, `color`, `range`, `inner_angle`, `outer_angle`) | a light param |
| `camera` | `node`, `param` (e.g. `fov_y`) | a camera param |
| `morph` | `node`, `index` | a morph-target weight |

### Keyframe values (`value`)

`value.kind` + `value.value` (array of floats):

| kind | array | used for |
|---|---|---|
| `vec3` | `[x, y, z]` | translation, scale, color params |
| `quat` | `[x, y, z, w]` | rotation (xyzw) |
| `scalar` | `[v]` | uniforms / single-float params (intensity, fov_y, metallic…) |

Interpolation per keyframe: `interp` = `step` | `linear` | `cubic` (set via
`add`'s default = linear, or `set_keyframe { interp }`).

## Worked example — a spinning cube

```jsonc
insert_primitive { "shape": "box" }                 // → <node>
add_clip                                            // → <clip>
set_clip_duration { "clip": <clip>, "value": 2.0 }
set_clip_loop     { "clip": <clip>, "loop_style": "loop" }
add_track { "clip": <clip>, "target": { "kind":"transform", "node":<node>, "prop":"rotation" } }
// → track index 0. Rotation is a quaternion; slerp takes the SHORTEST path,
//   so a full turn needs intermediate keys (0° → 180° → 360° about Y):
add_keyframe { "clip":<clip>, "track":0, "t":0.0, "value":{ "kind":"quat", "value":[0,0,0,1] } }
add_keyframe { "clip":<clip>, "track":0, "t":1.0, "value":{ "kind":"quat", "value":[0,1,0,0] } }      // 180° about Y
add_keyframe { "clip":<clip>, "track":0, "t":2.0, "value":{ "kind":"quat", "value":[0,0,0,1] } }      // back to identity
set_current_clip { "clip": <clip> }
set_playing { "on": true }
```

Quaternion cheat-sheet (axis-angle → xyzw): for angle θ about a unit axis
`(ax,ay,az)`, `xyzw = (ax·sin(θ/2), ay·sin(θ/2), az·sin(θ/2), cos(θ/2))`. So
180° about Y = `[0, 1, 0, 0]`; 90° about Y = `[0, 0.7071, 0, 0.7071]`. Prefer
`set_rotation_euler` for static poses; use quats only in keyframes.

### Shortcut — `add_spin_track` (wheels / rotors / fans)
The whole spin above collapses to ONE call: it generates a rotation track with
evenly-spaced, hemisphere-continuous quaternion keyframes for you (no hand-authored
quats):
```jsonc
add_spin_track { "clip":<clip>, "node":<node>, "axis":[0,1,0], "turns":1.0,
                 "duration":2.0, "keys_per_turn":4 }   // 4 keys/rev = 90° steps (default 4)
```
`turns` may be fractional (`0.25` = a quarter turn) or negative (reverse). Play it
back faster/slower or reversed with `set_clip_speed` / `set_clip_direction`. Undo
removes the one track.

## Worked example — pulse a material's emissive color

```jsonc
// material <mat> is a custom material with a vec3 uniform "color"
add_clip                                            // → <clip>
set_clip_duration { "clip":<clip>, "value":1.0 }
set_clip_loop     { "clip":<clip>, "loop_style":"ping_pong" }
add_track   { "clip":<clip>, "target":{ "kind":"uniform", "material":<mat>, "name":"color" } }   // → track 0
add_keyframe { "clip":<clip>, "track":0, "t":0.0, "value":{ "kind":"vec3", "value":[0.1,0.1,0.2] } }
add_keyframe { "clip":<clip>, "track":0, "t":1.0, "value":{ "kind":"vec3", "value":[0.2,0.8,1.0] } }
set_current_clip { "clip":<clip> }
set_playing { "on": true }
```

## Deterministic capture

Playback advances with wall-clock time. For a reproducible screenshot of a
specific moment: `set_playing { on:false }`, `set_playhead { t:<seconds> }`,
`wait_render_settled`, `screenshot_scene`. (For temporal *materials*, also pin
`set_frame_time`; see [`TEMPORAL_SHADERS.md`](TEMPORAL_SHADERS.md).)

## Verify

`get_track_data { clip, track }` returns the keyframes (times, values, interp,
tangents) so you can confirm what you wrote. `get_snapshot` lists clips + the
current clip.

## Track flags & transport

Typed tools (each undoable except transport):

```
delete_track       { "clip":<clip>, "track":0 }
set_track_mute     { "clip":<clip>, "track":0, "mute":true }   // muted → doesn't contribute to the pose
set_track_solo     { "clip":<clip>, "track":0, "solo":true }   // any solo → only soloed tracks contribute
set_track_sampler  { "clip":<clip>, "track":0, "sampler":"linear" }  // step | linear | cubic
step_playhead      { "kind":"next" }   // home | prev | next | end  (transport, not undoable)
```

## NLA mixer (layers & strips)

The mixer composes whole clips as time-placed *strips* on weighted *layers* (an
NLA stack). These are reachable via `dispatch_command` (escape hatch — pass the
`cmd` tag); each is undoable. Layer/strip indices are positions in the mixer
(see `get_snapshot`'s animation section).

```
dispatch_command { "command": { "cmd":"add_layer" } }                 // appends a Replace, weight-1 layer
dispatch_command { "command": { "cmd":"set_layer_weight", "layer":0, "weight":0.5 } }
dispatch_command { "command": { "cmd":"set_layer_mode",   "layer":0, "mode":"add" } }   // replace | add
dispatch_command { "command": { "cmd":"set_layer_mask",   "layer":0, "mask":{ ... } } } // per-bone mask
dispatch_command { "command": { "cmd":"add_strip",    "layer":0, "clip":<clip>, "start":0.0, "len":2.0 } }
dispatch_command { "command": { "cmd":"move_strip",   "layer":0, "strip":0, "start":1.0 } }
dispatch_command { "command": { "cmd":"trim_strip",   "layer":0, "strip":0, "start":0.5, "len":1.0 } }
dispatch_command { "command": { "cmd":"set_strip_repeat", "layer":0, "strip":0, "repeat":2.0 } }
dispatch_command { "command": { "cmd":"delete_strip", "layer":0, "strip":0 } }
dispatch_command { "command": { "cmd":"delete_layer", "layer":0 } }
```

(The common track-flag + transport ops above have dedicated typed tools; the
NLA layer/strip family stays on `dispatch_command` — lower-traffic, richer
shapes. All are validated by the editor-protocol wire round-trip tests.)
