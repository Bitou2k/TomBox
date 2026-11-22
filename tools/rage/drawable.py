"""Structure of the second title's model resources (virtual block), from pointer-following:
which geometry part uses which shader, each shader's textures, and each part's own buffers.
Pointers in the virtual block are 0x50000000-based offsets into it; GPU addresses are
0x60000000-based offsets into the GPU block (the file after it in the package).

Drawable (model resource, file offset 0, or inside a dictionary):
  +0x10 -> shader group:   +0x10 -> shader pointer array, +0x18 u16 count
  +0x50 -> model list:     +0x00 -> model pointer array, +0x08 u16 count
Shader:
  +0x10 -> texture-slot pointer array, +0x20 -> parameter table
  parameter table: u32 header (bits 8-15: number of resource parameters), u32 (low 16 bits:
  parameter count), then 8-byte (u32 joaat(name), u32 code) entries, the resources first. For
  a resource, code >> 2 indexes the slot array; a slot points to a binding whose +0x30 points
  16 bytes into a texture record (u32 6, u32 0, u32 joaat(texture name), u32 0, u64 pointer,
  u64 texture-pack key - the record described in modeltex.py).
Model:
  +0x08 -> geometry pointer array, +0x10 u16 count, +0x20 -> u16 shader index per geometry
Geometry:
  +0x40 -> {+0x00 -> meshlet buffer record (GPU address at +0x18); +0x08 u16 meshlet count,
            +0x0a u16 triangle count}
  +0x58 inline position buffer record (u32 size in dwords, u16 4, u16 9, ...), GPU address at
        +0x68
Fragment (name.#ft): +0x18 -> its drawable.
Dictionary (files carrying several low-detail models; no shader group at +0x10):
  +0x18 u64 1, +0x20 -> u32 name hashes, +0x28 u16 count, +0x30 -> drawable pointers, +0x38
  u16 count (same).

Evidence: on 4,000 sampled level files, every geometry's meshlet buffer has exactly the
meshlet and triangle counts its geometry states (9,137 parts), and every shader index is in
range; the DiffuseTex slot (joaat 0xc8e8c282) holds sRGB colour formats only (BC1/BC7/BC3
sRGB). Parameter names are stored as hashes only; DiffuseTex and SpecularTex are the two
matched so far.
"""
import struct

V, G = 0x50000000, 0x60000000
DIFFUSE = 0xC8E8C282  # joaat("DiffuseTex")
SPECULAR = 0xC6F46E79  # joaat("SpecularTex")
# Normal map slot: name not recovered; identified visually (blue-purple tangent-space normal
# maps in a sample of shaders, BC1 / BC7 linear formats).
NORMAL = 0x1E77BF4C
SRGB_FORMATS = {29, 72, 75, 78, 91, 99}


class FormatError(ValueError):
    pass


def _u16(b, o): return struct.unpack_from("<H", b, o)[0]
def _u32(b, o): return struct.unpack_from("<I", b, o)[0]
def _u64(b, o): return struct.unpack_from("<Q", b, o)[0]


def _ptr(b, o, need=True):
    if o is None or o < 0 or o + 8 > len(b):
        raise FormatError(f"pointer field at {o} outside the block")
    v = _u64(b, o)
    if V <= v < V + len(b):
        return v - V
    if v == 0 and not need:
        return None
    raise FormatError(f"value {v:#x} at {o:#x} is not a pointer into the block")


def shaders(b, d):
    sg = _ptr(b, d + 0x10)
    arr, n = _ptr(b, sg + 0x10, need=False), _u16(b, sg + 0x18)
    out = []
    for i in range(n if arr is not None else 0):
        s = _ptr(b, arr + 8 * i)
        slots, table = _ptr(b, s + 0x10, need=False), _ptr(b, s + 0x20)
        nres = (_u32(b, table) >> 8) & 0xFF
        tex = []
        for k in range(nres):
            h, code = struct.unpack_from("<II", b, table + 8 + 8 * k)
            key = tname = None
            bind = _ptr(b, slots + 8 * (code >> 2), need=False) if slots is not None else None
            if bind is not None:
                tp = _ptr(b, bind + 0x30, need=False)
                if tp is not None and tp >= 0x10 and _u32(b, tp - 0x10) == 6:
                    tname, key = _u32(b, tp - 8), _u64(b, tp + 8)
            tex.append(dict(param=h, key=key, name=tname))
        out.append(tex)
    return out


def geometries(b, d):
    """[(shader index, position GPU offset, meshlet GPU offset, meshlets, triangles)] per model."""
    ml = _ptr(b, d + 0x50)
    arr, n = _ptr(b, ml), _u16(b, ml + 8)
    models = []
    for i in range(n):
        m = _ptr(b, arr + 8 * i)
        garr, gn, mapping = _ptr(b, m + 8), _u16(b, m + 0x10), _ptr(b, m + 0x20)
        geos = []
        for j in range(gn):
            g = _ptr(b, garr + 8 * j)
            vs = _ptr(b, g + 0x40)
            mrec = _ptr(b, vs)
            geos.append(dict(shader=_u16(b, mapping + 2 * j), pos=_u64(b, g + 0x68) - G,
                             mesh=_u64(b, mrec + 0x18) - G, meshlets=_u16(b, vs + 8), triangles=_u16(b, vs + 0xA)))
        models.append(geos)
    return models


def drawables(b):
    """[(name hash or None, drawable offset)] of a model resource: one drawable at offset 0,
    or the members of a dictionary."""
    if len(b) < 0x60 or b[:8] != b"\0" * 8:
        raise FormatError("not a model resource")
    if _u64(b, 0x10) == 0 and _u64(b, 0x18) == 1:
        hs, n = _ptr(b, 0x20), _u16(b, 0x28)
        ps, n2 = _ptr(b, 0x30), _u16(b, 0x38)
        if n != n2:
            raise FormatError(f"dictionary counts differ ({n} names, {n2} drawables)")
        return [(_u32(b, hs + 4 * i), _ptr(b, ps + 8 * i)) for i in range(n)]
    try:
        shaders(b, 0)
        return [(None, 0)]
    except (FormatError, struct.error):
        pass
    # Fragment (name.#ft): its drawable is at the pointer at +0x18 (200 of 200 sampled
    # fragment files decode this way; +0x10 points elsewhere).
    return [(None, _ptr(b, 0x18))]


def normal_texture(tex):
    """The tangent-space normal map of a shader (slot NORMAL), or None. Placeholder 4x4
    textures are left to the caller (they decode to a flat colour)."""
    for t in tex:
        if t["param"] == NORMAL and t["key"] is not None:
            return t["key"]
    return None


def colour_texture(tex, fmt_of=None):
    """The texture shown as base colour: the DiffuseTex slot; failing that, when `fmt_of`
    (texture key -> DXGI format or None) is given, the first slot holding an sRGB-format
    texture (sRGB is used for colour data; on a 3,000-file sample this is layer 1 of the
    layered terrain shaders in 92% of cases). Returns the key or None."""
    for t in tex:
        if t["param"] == DIFFUSE and t["key"] is not None:
            return t["key"]
    if fmt_of:
        for t in tex:
            if t["key"] is not None and fmt_of(t["key"]) in SRGB_FORMATS:
                return t["key"]
    return None
