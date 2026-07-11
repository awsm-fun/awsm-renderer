#!/usr/bin/env python3
"""Generate open-sheet.glb — the open-boundary cluster-LOD fixture.

A dense wavy grid sheet (192x192 quads ~= 73k tris) with two punched
rectangular holes: a mesh with GENUINE open boundaries (outer rim + two hole
rims), the input class the cluster bake must stay crack-free on (A2 — see
lod-bake dag.rs `open_mesh_cut_preserves_authored_boundaries_only`). The waves
make simplification non-trivial so coarse cuts are visually distinct.

Deterministic (no RNG): re-running reproduces the same bytes, so the .glb can
be regenerated instead of versioned if it ever needs to change.

Usage: python3 gen-open-sheet.py  (writes open-sheet.glb next to itself)
"""
import json
import math
import os
import struct

N = 192               # quads per side
SIZE = 4.0            # world size of the sheet (meters)
AMP = 0.35            # wave amplitude
HOLES = [              # punched quad ranges (x0, x1, y0, y1) in quad coords
    (48, 80, 48, 80),
    (120, 160, 100, 132),
]


def in_hole(x, y):
    return any(x0 <= x < x1 and y0 <= y < y1 for (x0, x1, y0, y1) in HOLES)


def main():
    step = SIZE / N
    verts = []
    for y in range(N + 1):
        for x in range(N + 1):
            px = x * step - SIZE / 2
            pz = y * step - SIZE / 2
            py = AMP * math.sin(px * 3.1) * math.cos(pz * 2.3)
            verts.append((px, py, pz))

    def vid(x, y):
        return y * (N + 1) + x

    indices = []
    for y in range(N):
        for x in range(N):
            if in_hole(x, y):
                continue
            a, b = vid(x, y), vid(x + 1, y)
            c, d = vid(x + 1, y + 1), vid(x, y + 1)
            indices += [a, c, b, a, d, c]  # CCW when viewed from +Y

    # Smooth analytic normals of y = AMP*sin(3.1x)*cos(2.3z).
    normals = []
    for (px, _, pz) in verts:
        dx = AMP * 3.1 * math.cos(px * 3.1) * math.cos(pz * 2.3)
        dz = -AMP * 2.3 * math.sin(px * 3.1) * math.sin(pz * 2.3)
        ln = math.sqrt(dx * dx + 1 + dz * dz)
        normals.append((-dx / ln, 1 / ln, -dz / ln))

    pos_bytes = b"".join(struct.pack("<3f", *v) for v in verts)
    nrm_bytes = b"".join(struct.pack("<3f", *n) for n in normals)
    idx_bytes = b"".join(struct.pack("<I", i) for i in indices)
    while len(idx_bytes) % 4:
        idx_bytes += b"\0"
    bin_blob = pos_bytes + nrm_bytes + idx_bytes

    mins = [min(v[i] for v in verts) for i in range(3)]
    maxs = [max(v[i] for v in verts) for i in range(3)]
    gltf = {
        "asset": {"version": "2.0", "generator": "gen-open-sheet.py"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0, "name": "open_sheet"}],
        "meshes": [{
            "name": "open_sheet",
            "primitives": [{
                "attributes": {"POSITION": 0, "NORMAL": 1},
                "indices": 2,
                "material": 0,
            }],
        }],
        "materials": [{
            "name": "sheet",
            "pbrMetallicRoughness": {
                "baseColorFactor": [0.75, 0.55, 0.35, 1.0],
                "metallicFactor": 0.0,
                "roughnessFactor": 0.6,
            },
            "doubleSided": True,
        }],
        "buffers": [{"byteLength": len(bin_blob)}],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": len(pos_bytes)},
            {"buffer": 0, "byteOffset": len(pos_bytes), "byteLength": len(nrm_bytes)},
            {"buffer": 0, "byteOffset": len(pos_bytes) + len(nrm_bytes),
             "byteLength": len(indices) * 4},
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": len(verts),
             "type": "VEC3", "min": mins, "max": maxs},
            {"bufferView": 1, "componentType": 5126, "count": len(verts),
             "type": "VEC3"},
            {"bufferView": 2, "componentType": 5125, "count": len(indices),
             "type": "SCALAR"},
        ],
    }

    js = json.dumps(gltf, separators=(",", ":")).encode()
    while len(js) % 4:
        js += b" "
    total = 12 + 8 + len(js) + 8 + len(bin_blob)
    out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "open-sheet.glb")
    with open(out, "wb") as f:
        f.write(struct.pack("<4sII", b"glTF", 2, total))
        f.write(struct.pack("<I4s", len(js), b"JSON"))
        f.write(js)
        f.write(struct.pack("<I4s", len(bin_blob), b"BIN\0"))
        f.write(bin_blob)
    print(f"wrote {out}: {len(verts)} verts, {len(indices)//3} tris, {total} bytes")


if __name__ == "__main__":
    main()
