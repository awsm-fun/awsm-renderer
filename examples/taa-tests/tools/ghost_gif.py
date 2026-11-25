#!/usr/bin/env python3
"""Side-by-side crops and GIFs from ghost-track dumps.

ghost_gif.py <store-root> <scene> <name> <k_center> <k_from> <k_to> <cx> <cy> <cw> <ch> <out-dir> [label]

Reads <store-root>/ghost/<scene>/k<k>-{taa,msaa,oracle,ref,class}.png and writes
  <out-dir>/<name>-k<k_center>-x4.png      panels at 4x, the centre frame
  <out-dir>/<name>-x1.gif / -x4.gif         frames k_from..k_to, 1x and 4x
Panels: TAA | MSAA 4x | ORACLE | REFERENCE | class map (red = ghost (a),
blue = current-frame (b), yellow = oracle-level (c), grey = no-AA-level (d)).
With BEFORE_ROOT=<dir> set (a copy of an earlier `ghost/` tree), the first
panels are "TAA before" (<dir>/<scene>/k<k>-taa.png) and "TAA after".
Needs Pillow.
"""
import os
import sys

from PIL import Image, ImageDraw, ImageFont

PANELS = [("taa", "TAA"), ("msaa", "MSAA 4x"), ("oracle", "ORACLE"), ("ref", "REFERENCE"), ("class", "class (T=16)")]


def font(size):
    for p in ["/System/Library/Fonts/Menlo.ttc", "/System/Library/Fonts/Helvetica.ttc"]:
        try:
            return ImageFont.truetype(p, size)
        except OSError:
            pass
    return ImageFont.load_default()


BEFORE = os.environ.get("BEFORE_ROOT")


def frame(root, scene, k, box, scale, label):
    tiles = []
    panels = list(PANELS)
    if BEFORE:
        panels = [("taa@before", "TAA before"), ("taa", "TAA after")] + panels[1:]
    for key, title in panels:
        if key == "taa@before":
            p = os.path.join(BEFORE, scene, f"k{k:03d}-taa.png")
        else:
            p = os.path.join(root, "ghost", scene, f"k{k:03d}-{key}.png")
        im = Image.open(p).convert("RGB").crop(box)
        im = im.resize((im.width * scale, im.height * scale), Image.NEAREST)
        tiles.append((title, im))
    pad, head = 4, 18 if scale == 1 else 26
    w = sum(t.width for _, t in tiles) + pad * (len(tiles) - 1)
    h = tiles[0][1].height + head
    out = Image.new("RGB", (w, h), (20, 20, 20))
    d = ImageDraw.Draw(out)
    f = font(11 if scale == 1 else 16)
    x = 0
    for title, t in tiles:
        out.paste(t, (x, head))
        d.text((x + 3, 2), title, fill=(235, 235, 235), font=f)
        x += t.width + pad
    d.text((w - (70 if scale == 1 else 110), 2), f"k{k}", fill=(255, 200, 0), font=f)
    return out


def main():
    root, scene, name = sys.argv[1], sys.argv[2], sys.argv[3]
    kc, k0, k1, cx, cy, cw, ch = map(int, sys.argv[4:11])
    outd = sys.argv[11]
    label = sys.argv[12] if len(sys.argv) > 12 else ""
    os.makedirs(outd, exist_ok=True)
    box = (cx - cw // 2, cy - ch // 2, cx - cw // 2 + cw, cy - ch // 2 + ch)
    frame(root, scene, kc, box, 4, label).save(os.path.join(outd, f"{name}-k{kc}-x4.png"))
    # 1x: three times the area around the same centre (context), clamped.
    bw, bh = min(cw * 3, 320), min(ch * 3, 240)
    bx = max(0, min(960 - bw, cx - bw // 2))
    by = max(0, min(720 - bh, cy - bh // 2))
    big = (bx, by, bx + bw, by + bh)
    for scale, tag, b in [(1, "x1", big), (4, "x4", box)]:
        frames = []
        for k in range(k0, k1 + 1):
            if os.path.exists(os.path.join(root, "ghost", scene, f"k{k:03d}-taa.png")):
                frames.append(frame(root, scene, k, b, scale, label))
        if frames:
            frames[0].save(
                os.path.join(outd, f"{name}-{tag}.gif"),
                save_all=True,
                append_images=frames[1:],
                duration=200,
                loop=0,
            )


if __name__ == "__main__":
    main()
