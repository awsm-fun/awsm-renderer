#!/usr/bin/env python3
"""Generate the Jetpack Knockout arena source assets (deterministic, no deps).

Writes, next to itself:
  hex-floor.png      2048^2 RGBA albedo — polished hex-panel deck, alpha-masked
                     to a disc (the floor plane uses alpha_mode=mask), with a
                     danger band near the rim.
  hex-floor-emit.png 1024^2 RGB emissive — red rim band.
  starfield.ktx2     256^2 x6 RGBA8-SRGB cubemap, 5 mips — the space skybox
                     (also bound as the specular env: dark reflections with
                     star glints; mips are box-filtered so rough lobes darken).
  irradiance.ktx2    32^2 x6 RGBA8-SRGB cubemap, 1 mip — cool deep-space
                     ambient gradient (subtle blue from above, near-black up
                     from below).

Everything is seeded — re-running reproduces identical bytes.
"""
import math
import os
import random
import struct
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))


# ---------------------------------------------------------------- PNG writer
def write_png(path, w, h, rgba):
    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    raw = b"".join(b"\x00" + bytes(rgba[y * w * 4:(y + 1) * w * 4]) for y in range(h))
    png = (b"\x89PNG\r\n\x1a\n"
           + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(raw, 9))
           + chunk(b"IEND", b""))
    with open(path, "wb") as f:
        f.write(png)


# ------------------------------------------------------------- hex geometry
SQ3 = math.sqrt(3.0)


def hex_cell(x, y, s):
    """Axial hex cell id + distance (0..1) from the pixel to the cell edge
    (1 = center, 0 = on the seam). Pointy-top hexes of size s."""
    q = (SQ3 / 3 * x - 1 / 3 * y) / s
    r = (2 / 3 * y) / s
    # cube round
    cx, cz = q, r
    cy = -cx - cz
    rx, ry, rz = round(cx), round(cy), round(cz)
    dx, dy, dz = abs(rx - cx), abs(ry - cy), abs(rz - cz)
    if dx > dy and dx > dz:
        rx = -ry - rz
    elif dy > dz:
        ry = -rx - rz
    else:
        rz = -rx - ry
    # cell center in pixels
    ccx = s * (SQ3 * rx + SQ3 / 2 * rz)
    ccy = s * (1.5 * rz)
    # distance to edge: max over the 3 hex axes
    lx, ly = x - ccx, y - ccy
    d = max(abs(lx) * SQ3 / 2 + abs(ly) / 2, abs(ly))
    return (rx, rz), 1.0 - d / s


def hex_cell_m(x, y, a):
    """hex_cell in WORLD METERS (pointy-top, circumradius a). Returns
    (cell_id, edge) with edge in 0..1 (1 = center, 0 = on the seam) — the
    shared lattice for the full-floor maps AND the tileable detail pair, so
    the tile's grooves land exactly on the albedo's seams."""
    return hex_cell(x, y, a)


def gen_floor():
    """Hexagonal tech floor, three-map hybrid (2026-07-13b, hex replaced the
    square grid per review): FULL-FLOOR albedo (2048 — per-hex graphite tint,
    seam grooves, disc alpha mask; the red danger band is GONE — a glowing
    red base WALL replaced it) + emissive (1024, per-hex whisper glow), plus
    a TILEABLE detail pair (normal + metallic-roughness) covering exactly one
    hex-lattice period rect (sqrt(3)a x 3a) — sampler-tiled via the floor
    node's texture transforms (author.js). Same hex function drives all maps
    (hex_cell_m in world meters), so everything aligns by construction."""
    A = 1.25                 # hex circumradius, meters (~2.16 m across flats)
    GROOVE = 0.040           # edge < this => seam groove (edge is 0..1 of A)
    BEVEL = 0.150            # groove..bevel => beveled shoulder
    W = 2048
    M_PER_PX = 84.0 / W
    rng_master = random.Random(20260715)
    cell_val = {}
    cell_dark = {}

    def cell_props(cell):
        if cell not in cell_val:
            h = random.Random((cell[0] * 73856093) ^ (cell[1] * 19349663) ^ 20260715)
            cell_val[cell] = h.uniform(-0.038, 0.042)
            cell_dark[cell] = h.random() < 0.06   # occasional matte "vent" hex
        return cell_val[cell], cell_dark[cell]

    albedo = bytearray(W * W * 4)
    EW = 1024
    emit = bytearray(EW * EW * 4)
    cx = W / 2
    R = W / 2 - 14

    for y in range(W):
        for x in range(W):
            i = (y * W + x) * 4
            dx, dy = x - cx, y - cx
            rr = math.hypot(dx, dy)
            if rr > R:
                albedo[i:i + 4] = b"\x00\x00\x00\x00"
                continue
            xm, ym = x * M_PER_PX, y * M_PER_PX
            cell, edge = hex_cell_m(xm, ym, A)
            v, dark = cell_props(cell)
            base = (0.085 + v, 0.092 + v, 0.115 + v)
            col = [base[0], base[1], base[2]]
            if dark:
                col = [c * 0.55 for c in col]
            seam = edge < GROOVE
            if seam:
                col = [0.028, 0.032, 0.045]
            elif edge < BEVEL:
                t = (edge - GROOVE) / (BEVEL - GROOVE)
                col = [c + 0.022 * (1.0 - t) for c in col]
            a8 = 255
            if rr > R - 3:
                a8 = max(0, min(255, int((R - rr) / 3 * 255)))
            albedo[i] = min(255, int(col[0] * 255))
            albedo[i + 1] = min(255, int(col[1] * 255))
            albedo[i + 2] = min(255, int(col[2] * 255))
            albedo[i + 3] = a8

            if (x & 1) == 0 and (y & 1) == 0:
                ei = ((y >> 1) * EW + (x >> 1)) * 4
                r_ = g_ = b_ = 0
                if not seam:
                    g_scale = (1.0 + v * 10.0) * (0.4 if dark else 1.0)
                    r_ = max(0, int(7 * g_scale))
                    g_ = max(0, int(9 * g_scale))
                    b_ = max(0, int(14 * g_scale))
                emit[ei] = r_
                emit[ei + 1] = g_
                emit[ei + 2] = b_
                emit[ei + 3] = 255

    write_png(os.path.join(HERE, "hex-floor.png"), W, W, albedo)
    write_png(os.path.join(HERE, "hex-floor-emit.png"), EW, EW, emit)

    # ---- tileable detail pair over ONE lattice period rect --------------
    # period = (sqrt(3)A, 3A) meters; texture sized for ~isotropic texels.
    PW = SQ3 * A             # 2.165 m
    PH = 3.0 * A             # 3.75 m
    TU = 512
    TV = int(round(TU * PH / PW / 2) * 2)   # 886 — keep even
    pxm_u = TU / PW
    pxm_v = TV / PH

    trng = random.Random(20260716)
    # Waves HALVED + scratches REMOVED (2026-07-13d): per-pixel normal /
    # roughness noise on a glossy floor is ray-direction noise — across a
    # 30 m reflection path even sub-degree jitter displaces the hit by
    # meters, so the high-frequency detail aliased into a dark-sparkle
    # field over every far reflection (the red wall band especially).
    # Detail that can't survive the viewing distance doesn't get authored
    # at that frequency: the hex bevels carry the close-up read alone.
    # Waves REMOVED entirely (rev d2): even halved, the undulation showed up
    # as scattered white "eyelash" ticks on the near floor — SSR reflections
    # of the white-hot pad cores displaced by the per-pixel normal wobble.
    # A polished arena floor is FLAT: coherent reflections ARE the look; the
    # beveled hex edges alone carry the material read.
    waves = []
    scratches = []

    def height(px, py):
        # px/py in TILE pixels (may be out of range for gradient taps — wrap)
        px %= TU
        py %= TV
        xm = px / pxm_u
        ym = py / pxm_v
        _, edge = hex_cell_m(xm, ym, A)
        # Micro-bevel (rev d2: 10 mm -> 3 mm): a 10 mm bevel over the ~11 cm
        # shoulder tilts normals ~5 deg — enough for the glossy floor's SSR
        # to catch the white-hot pad cores from meters away as scattered
        # "eyelash" glints along every hex edge. 3 mm keeps a read at
        # walking distance; the dark groove line draws the panel anyway.
        if edge < GROOVE:
            h = -0.003
        elif edge < BEVEL:
            t = (edge - GROOVE) / (BEVEL - GROOVE)
            h = -0.003 * (1.0 - t) * (1.0 - t)
        else:
            h = 0.0
        for (wx, wy, ph, amp) in waves:
            h += amp * math.cos(2 * math.pi * (wx * px / TU + wy * py / TV) + ph)
        for (sx, sy, ca, sa, ln, dep) in scratches:
            rx, ry = px - sx, py - sy
            along = rx * ca + ry * sa
            perp = abs(-rx * sa + ry * ca)
            if abs(along) < ln and perp < 1.6:
                h -= dep * (1.0 - perp / 1.6)
        return h

    normal = bytearray(TU * TV * 4)
    mr = bytearray(TU * TV * 4)
    for y in range(TV):
        for x in range(TU):
            i = (y * TU + x) * 4
            hl = height(x - 1, y)
            hr = height(x + 1, y)
            hu = height(x, y - 1)
            hd = height(x, y + 1)
            nx = -(hr - hl) / (2.0 / pxm_u)
            ny = -(hd - hu) / (2.0 / pxm_v)
            nz = 1.0
            ln_ = math.sqrt(nx * nx + ny * ny + nz * nz)
            normal[i] = max(0, min(255, int((nx / ln_ * 0.5 + 0.5) * 255)))
            normal[i + 1] = max(0, min(255, int((ny / ln_ * 0.5 + 0.5) * 255)))
            normal[i + 2] = max(0, min(255, int((nz / ln_ * 0.5 + 0.5) * 255)))
            normal[i + 3] = 255

            xm = x / pxm_u
            ym = y / pxm_v
            _, edge = hex_cell_m(xm, ym, A)
            # MINIMAL variation (rev d3): under a bright-banded probe, any
            # sizeable roughness step is a BRIGHTNESS step — a rougher patch
            # samples a blurrier env mip whose average is LIFTED by the neon
            # bands, so satin bevel corners read as bright triangles at every
            # hex vertex (IBL, not SSR, not normals — survived both off).
            # Grooves get a small step only; metal stays uniform (constant
            # F0); the visual panel line lives in the ALBEDO.
            if edge < GROOVE:
                rough, metal = 0.30, 0.40
            else:
                rough, metal = 0.18, 0.45
                for (wx, wy, ph, amp) in waves:
                    rough += 8.0 * amp * math.cos(2 * math.pi * (wx * x / TU + wy * y / TV) + ph)
                for (sx, sy, ca, sa, ln2, dep) in scratches:
                    rx, ry = x - sx, y - sy
                    along = rx * ca + ry * sa
                    perp = abs(-rx * sa + ry * ca)
                    if abs(along) < ln2 and perp < 1.6:
                        rough += 0.14 * (1.0 - perp / 1.6)
            mr[i] = 255
            mr[i + 1] = max(0, min(255, int(rough * 255)))
            mr[i + 2] = max(0, min(255, int(metal * 255)))
            mr[i + 3] = 255

    write_png(os.path.join(HERE, "floor-tile-normal.png"), TU, TV, normal)
    write_png(os.path.join(HERE, "floor-tile-mr.png"), TU, TV, mr)
    print("hex floor v3: albedo/emit + tile normal/mr written (tile %dx%d)" % (TU, TV))


# ------------------------------------------------------------- KTX2 writer
def write_ktx2(path, size, face_mips, vk_format=None):
    if vk_format is None:
        vk_format = VK_R8G8B8A8_SRGB
    """face_mips: list of levels; each level = list of 6 face bytearrays
    (RGBA8, tight). Levels ordered mip0..mipN-1 (largest first)."""
    VK_R8G8B8A8_SRGB = 43
    levels = len(face_mips)
    ident = b"\xabKTX 20\xbb\r\n\x1a\n"
    # basic DFD for RGBA8 SRGB: 24-byte block header + 4 samples x 16 bytes
    dfd_block = struct.pack("<I", 0)                 # vendorId + descriptorType
    dfd_block += struct.pack("<HH", 0, 24 + 16 * 4)  # version, descriptorBlockSize
    dfd_block += bytes([1, 1, 2, 0])                 # RGBSDA, BT709, SRGB, straight-alpha
    dfd_block += bytes([0, 0, 0, 0])                 # texelBlockDimension (1x1x1x1)
    dfd_block += bytes([4, 0, 0, 0, 0, 0, 0, 0])     # bytesPlane0 = 4
    for ch, off in ((0, 0), (1, 8), (2, 16), (15, 24)):  # R,G,B,A channels
        dfd_block += struct.pack("<HBB", off, 7, ch)     # bitOffset, bitLength-1, type
        dfd_block += bytes([0, 0, 0, 0])                 # samplePosition0..3
        dfd_block += struct.pack("<II", 0, 255)          # sampleLower/Upper
    dfd = struct.pack("<I", 4 + len(dfd_block)) + dfd_block

    header_size = 80
    index_size = 24 * levels
    dfd_off = header_size + index_size
    kvd_off = dfd_off + len(dfd)
    data_off = (kvd_off + 15) // 16 * 16

    # level data: spec stores SMALLEST mip first in the file
    offsets = []
    blobs = []
    pos = data_off
    for lv in reversed(range(levels)):
        blob = b"".join(bytes(f) for f in face_mips[lv])
        pos = (pos + 15) // 16 * 16
        offsets.append((lv, pos, len(blob)))
        blobs.append((pos, blob))
        pos += len(blob)

    level_index = [None] * levels
    for lv, off, ln in offsets:
        level_index[lv] = struct.pack("<QQQ", off, ln, ln)

    hdr = ident
    hdr += struct.pack("<IIIIIIII",
                       vk_format, 1, size, size, 0, 0, 6, levels)
    hdr += struct.pack("<I", 0)  # supercompression none
    hdr += struct.pack("<II", dfd_off, len(dfd))
    hdr += struct.pack("<II", 0, 0)  # kvd
    hdr += struct.pack("<QQ", 0, 0)  # sgd
    out = bytearray(hdr)
    out += b"".join(level_index)
    out += dfd
    while len(out) < data_off:
        out += b"\x00"
    for off, blob in blobs:
        while len(out) < off:
            out += b"\x00"
        out += blob
    with open(path, "wb") as f:
        f.write(out)


def downsample(face, size):
    half = size // 2
    out = bytearray(half * half * 4)
    for y in range(half):
        for x in range(half):
            o = (y * half + x) * 4
            for c in range(4):
                s_ = (face[(2 * y * size + 2 * x) * 4 + c]
                      + face[(2 * y * size + 2 * x + 1) * 4 + c]
                      + face[((2 * y + 1) * size + 2 * x) * 4 + c]
                      + face[((2 * y + 1) * size + 2 * x + 1) * 4 + c])
                out[o + c] = s_ // 4
    return out


def gen_starfield():
    S = 256
    rng = random.Random(777)
    bg = (4, 5, 12)
    faces = []
    for _ in range(6):
        face = bytearray(S * S * 4)
        for i in range(0, S * S * 4, 4):
            face[i], face[i + 1], face[i + 2], face[i + 3] = *bg, 255
        # stars
        for _ in range(150):
            x, y = rng.uniform(2, S - 3), rng.uniform(2, S - 3)
            r = rng.uniform(0.5, 1.5)
            big = rng.random() < 0.06
            if big:
                r = rng.uniform(1.8, 2.6)
            lum = rng.uniform(0.35, 1.0)
            warm = rng.random() < 0.3
            tint = (1.0, 0.92, 0.82) if warm else (0.85, 0.9, 1.0)
            rad = int(r * 3) + 1
            for py in range(int(y) - rad, int(y) + rad + 1):
                for px in range(int(x) - rad, int(x) + rad + 1):
                    if not (0 <= px < S and 0 <= py < S):
                        continue
                    d2 = (px - x) ** 2 + (py - y) ** 2
                    g = math.exp(-d2 / (2 * r * r)) * lum
                    if g < 0.01:
                        continue
                    o = (py * S + px) * 4
                    for c in range(3):
                        v = face[o + c] + int(g * 255 * tint[c])
                        face[o + c] = min(255, v)
        faces.append(face)
    mips = [faces]
    sz = S
    while sz > 16:
        mips.append([downsample(f, sz) for f in mips[-1]])
        sz //= 2
    # Attenuate the coarser mips (the specular-IBL roughness lobes): a plain
    # box filter preserves the stars' energy, so mid-rough surfaces pick up
    # sparkly star glints. Real prefiltering would spread that energy over
    # the GGX lobe; darkening ~45%/level approximates it and kills the
    # speckle while the mip-0 skybox stays untouched.
    for lv in range(1, len(mips)):
        k = 0.55 ** lv
        for f in mips[lv]:
            for i in range(0, len(f), 4):
                f[i] = int(f[i] * k)
                f[i + 1] = int(f[i + 1] * k)
                f[i + 2] = int(f[i + 2] * k)
    write_ktx2(os.path.join(HERE, "starfield.ktx2"), S, mips)
    print(f"starfield.ktx2 written ({len(mips)} mips)")


def gen_interior():
    """interior.ktx2 — the SPECULAR env slot, now HDR (rev e:
    VK_FORMAT_E5B9G9R9_UFLOAT_PACK32 -> WebGPU rgb9e5ufloat, filterable,
    SAME 4 bytes/px as the old sRGB8 — the renderer's KTX2 loader already
    maps it). The probe is authored in TRUE LINEAR RADIANCE: ring bands at
    their actual neon emissive (~2.6), the red base wall at ~1.5, the top
    rim ~3.1, and near-black space between — the contrast an 8-bit LDR map
    physically could not hold (a band at sRGB 52 is 0.036 linear = 40x too
    dim, which is why every SSR->fallback hand-off read as a fade). The
    fallback now BLOOMS like the real reflection it stands in for.
    Band elevations mapped FOR BOX PROJECTION (dirs re-aimed relative to
    the probe center [0,13,0]): v = 0.5 - 0.5*(h - CY)/HX."""
    S = 256
    VK_E5B9G9R9 = 123
    bg = (0.0015, 0.002, 0.005)          # deep space, linear
    floor_tone = (0.003, 0.004, 0.006)   # dark floor, linear
    # ATMOSPHERIC HAZE (rev f — the near-field fix): steep reflection rays
    # from mid-arena point ABOVE the ring stack where the scene has nothing,
    # so near-field floor reflections died to black while the far field
    # blazed — a brightness cliff that sweeps with the camera ("the fade").
    # A real arena's AIR glows: a faint cool neon-lit haze above the rim
    # gives steep reflections a soft ambient sheen to land in. Radiance is
    # deliberately whisper-level (~0.02): sheen, not fog.
    haze = (0.016, 0.019, 0.028)
    RINGS = [
        (50, 100, 255), (0, 220, 235), (50, 235, 90), (255, 220, 30),
        (255, 125, 0), (255, 65, 70), (255, 55, 200), (165, 75, 255),
    ]
    CY, HX = 13.0, 42.0

    def band_v(h):
        return 0.5 - 0.5 * (h - CY) / HX

    def srgb_to_lin(c):
        c = c / 255.0
        return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4

    # (v, linear rgb radiance, sigma px) — ENERGY-CONSERVED peaks: a probe
    # band must carry the same total energy as the real feature, i.e.
    # peak = true_radiance * (true_angular_width / band_width). A ring tube
    # (0.6 m at ~42 m) subtends ~0.25 px on a 128 face; painting it at full
    # radiance over a 1.6 px-sigma band overrepresents its energy ~10x —
    # the first HDR attempt turned the fallback into a blinding rainbow fan
    # BRIGHTER than the truth. The red WALL is genuinely wide (2.45 m ->
    # ~2.4 px), so IT keeps near-true radiance — which is exactly why the
    # wall was the fade's dominant mismatch all along.
    bands = []
    for i, c in enumerate(RINGS):
        lin = tuple(srgb_to_lin(ch) * 1.1 * 2.4 * 0.065 for ch in c)
        bands.append((band_v(2.6 + i * 2.9), lin, 3.2))
    bands.append((band_v(26.2), (0.31, 0.31, 0.31), 4.8))       # top rim
    bands.append((band_v(1.2), (1.30, 0.10, 0.07), 6.4))        # red base wall

    def side_face_f():
        face = [0.0] * (S * S * 3)
        v_floor = band_v(-1.0)
        v_rim = band_v(26.2)
        for y in range(S):
            vy = y / (S - 1)
            base = floor_tone if vy > v_floor else bg
            r0, g0, b0 = base
            r, g, b = r0, g0, b0
            if vy < v_rim:
                # above the top rim: haze ramps in toward the zenith
                t = min(1.0, (v_rim - vy) / max(v_rim, 1e-4))
                r += haze[0] * (0.3 + 0.7 * t)
                g += haze[1] * (0.3 + 0.7 * t)
                b += haze[2] * (0.3 + 0.7 * t)
            for (bv, c, sig) in bands:
                d = (vy - bv) * S
                gsn = math.exp(-d * d / (2.0 * sig * sig))
                r += c[0] * gsn
                g += c[1] * gsn
                b += c[2] * gsn
            for x in range(S):
                o = (y * S + x) * 3
                face[o] = r
                face[o + 1] = g
                face[o + 2] = b
        return face

    def flat_face_f(color):
        face = [0.0] * (S * S * 3)
        for i in range(0, S * S * 3, 3):
            face[i], face[i + 1], face[i + 2] = color
        return face

    def downsample_f(face, size):
        half = size // 2
        out = [0.0] * (half * half * 3)
        for y in range(half):
            for x in range(half):
                o = (y * half + x) * 3
                for c in range(3):
                    out[o + c] = (face[(2 * y * size + 2 * x) * 3 + c]
                                  + face[(2 * y * size + 2 * x + 1) * 3 + c]
                                  + face[((2 * y + 1) * size + 2 * x) * 3 + c]
                                  + face[((2 * y + 1) * size + 2 * x + 1) * 3 + c]) / 4.0
        return out

    def pack_rgb9e5(face, size):
        out = bytearray(size * size * 4)
        for i in range(size * size):
            r = max(0.0, min(65408.0, face[i * 3]))
            g = max(0.0, min(65408.0, face[i * 3 + 1]))
            b = max(0.0, min(65408.0, face[i * 3 + 2]))
            m = max(r, g, b)
            if m < 1e-8:
                word = 0
            else:
                e = max(-16, math.floor(math.log2(m))) + 1
                e = max(-15, min(16, e))
                denom = 2.0 ** (e - 9)
                if round(m / denom) >= 512:
                    e += 1
                    denom *= 2.0
                rm = min(511, int(round(r / denom)))
                gm = min(511, int(round(g / denom)))
                bm = min(511, int(round(b / denom)))
                word = rm | (gm << 9) | (bm << 18) | ((e + 15) << 27)
            out[i * 4:i * 4 + 4] = struct.pack("<I", word)
        return out

    def up_face_f():
        # zenith haze: brightest at the horizon ring (near the walls' glow),
        # dimming toward straight-up space.
        face = [0.0] * (S * S * 3)
        for y in range(S):
            for x in range(S):
                dx = (x / (S - 1)) * 2.0 - 1.0
                dy = (y / (S - 1)) * 2.0 - 1.0
                rr = min(1.0, math.hypot(dx, dy))
                t = 0.25 + 0.75 * rr
                o = (y * S + x) * 3
                face[o] = bg[0] + haze[0] * t
                face[o + 1] = bg[1] + haze[1] * t
                face[o + 2] = bg[2] + haze[2] * t
        return face

    side = side_face_f()
    faces_f = [side, list(side), up_face_f(), flat_face_f(floor_tone),
               list(side), list(side)]
    mips_f = [faces_f]
    sz = S
    while sz > 8:
        mips_f.append([downsample_f(f, sz) for f in mips_f[-1]])
        sz //= 2
    mips = []
    sz = S
    for level in mips_f:
        mips.append([pack_rgb9e5(f, sz) for f in level])
        sz //= 2
    write_ktx2(os.path.join(HERE, "interior.ktx2"), S, mips, vk_format=VK_E5B9G9R9)
    print(f"interior.ktx2 written HDR rgb9e5 ({len(mips)} mips)")


def gen_irradiance():
    S = 32
    # face order: +X -X +Y -Y +Z -Z ; direction per texel -> vertical gradient
    zen = (26, 30, 48)    # cool blue overhead
    hor = (14, 15, 26)
    nad = (5, 6, 10)
    faces = []
    for fi in range(6):
        face = bytearray(S * S * 4)
        for y in range(S):
            for x in range(S):
                u, v = (x + 0.5) / S * 2 - 1, (y + 0.5) / S * 2 - 1
                d = {
                    0: (1, -v, -u), 1: (-1, -v, u),
                    2: (u, 1, v), 3: (u, -1, -v),
                    4: (u, -v, 1), 5: (-u, -v, -1),
                }[fi]
                n = math.sqrt(d[0] ** 2 + d[1] ** 2 + d[2] ** 2)
                ny = d[1] / n
                if ny >= 0:
                    c = [hor[i] + (zen[i] - hor[i]) * ny for i in range(3)]
                else:
                    c = [hor[i] + (nad[i] - hor[i]) * -ny for i in range(3)]
                o = (y * S + x) * 4
                face[o], face[o + 1], face[o + 2], face[o + 3] = int(c[0]), int(c[1]), int(c[2]), 255
        faces.append(face)
    write_ktx2(os.path.join(HERE, "irradiance.ktx2"), S, [faces])
    print("irradiance.ktx2 written")


if __name__ == "__main__":
    gen_floor()
    gen_starfield()
    gen_interior()
    gen_irradiance()
