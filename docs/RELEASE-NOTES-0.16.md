# Release notes — 0.16.x

## BREAKING: exposure is now EV stops

`PostProcessConfig::exposure` (and the `set_post_process { exposure }`
editor/MCP command) is interpreted as **EV stops**: the renderer scales
scene luminance by `2^exposure`. `0.0` is neutral, `+1.0` doubles,
`-1.0` halves.

Projects authored before the change treated the value as a **linear
multiplier**. A saved value that used to mean "dim to 30%" (`0.3`) now
means "1.23× brighter" — roughly a **4× brightness jump**, which on
metallic/emissive content reads as blown-white highlights under
ACES + bloom.

There is no version marker in `project.toml` to key an automatic
migration on, so re-author the value by hand:

```
ev = log2(old_linear_multiplier)
0.3  (linear)  →  -1.74 EV
0.5  (linear)  →  -1.00 EV
1.0  (linear)  →   0.00 EV
2.0  (linear)  →  +1.00 EV
```

Real-world example: the DANCE-OFF stage scene was authored at linear
`0.3` for its dark-stage look; on 0.16 it renders correctly at
`-1.74 EV`. If a previously-moody scene suddenly renders bright with
white-hot speculars after upgrading, this is why — it is not a shadow
or material regression (we spent a while proving that).
