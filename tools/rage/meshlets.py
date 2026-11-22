"""Decode the second title's packed meshlet geometry (position + triangles) to OBJ.

Layout taken from its position-only mesh shader (MS_VS_Transform_PositionOnly, RDNA2
disassembly), not guessed:
  meshlet buffer: dword0 = 0x80000000 | meshlet count (low 17 bits), dword1 = triangle dwords
    total, dword2 = vertex-reference total; meshlet records of 16 bytes start at byte 12.
    Record dword a: bits 0-6 vertex count-1, bits 7-13 triangle count-1, bit 14 -> vertex
    base bit 16, bit 15 -> triangle base bit 16. Record dword b: low 16 = vertex base,
    high 16 = triangle base. Triangle dwords start at 12 + 16*count; each holds three 7-bit
    local indices (bits 0-6, 7-13, 14-20). Vertex references (u16) follow all triangle dwords.
  position buffer ("B@"): dword0: stride = (d0 >> 3) & 60, data offset = (d0 >> 7) & 0x1fffffc.
    Element: 64 bits, a = low dword >> 8 (24 bit), b = low byte | (high dword & 0xffff) << 8
    (24 bit), c = high dword >> 16 (16 bit). value = q * scale + bias, bias = 3 floats at
    +0x60, scale = 3 floats at +0x70, axis order code u32 at +0x6c:
    2 -> (a,b,c), 1 -> (b,c,a), 0 -> (c,b,a)   [as the shader assembles x,y,z].
"""
import struct, sys

def positions(g, base, count=None):
    d0 = struct.unpack_from("<I", g, base)[0]
    stride, off = (d0 >> 3) & 60, (d0 >> 7) & 0x1fffffc
    bias = struct.unpack_from("<3f", g, base + 0x60); order = struct.unpack_from("<I", g, base + 0x6c)[0]
    scale = struct.unpack_from("<3f", g, base + 0x70)
    if stride != 8 or order > 2: raise ValueError(f"unsupported position stream: stride {stride} order {order}")
    def get(i):
        lo, hi = struct.unpack_from("<II", g, base + off + i * stride)
        a = lo >> 8; b = (lo & 0xff) | ((hi & 0xffff) << 8); c = hi >> 16
        v = (a * scale[0] + bias[0], b * scale[1] + bias[1], c * scale[2] + bias[2])
        return {2: (v[0], v[1], v[2]), 1: (v[1], v[2], v[0]), 0: (v[2], v[1], v[0])}[order]
    return get, off, stride

def _stream(g, base, word):
    w = struct.unpack_from("<I", g, base + 4 * word)[0]
    return (None if not w else ((w >> 7) & 0x1fffffc, (w >> 3) & 60, w & 15))

def _oct(x, y):
    z = 1.0 - abs(x) - abs(y); t = max(-z, 0.0)
    x += -t if x >= 0 else t; y += -t if y >= 0 else t
    n = (x * x + y * y + z * z) ** 0.5 or 1.0
    return (x / n, y / n, z / n)

def normals(g, base, pos=None, tris=None):
    """Header word 2: one dword per vertex, octahedral, 10:10 bits (bits 0-9, 10-19,
    * 2/1023 - 1) or 8:8 bits (bytes 0,1 * 2/255 - 1). The mesh shader picks by the top bit,
    but on the sample terrain the bit is clear and 10:10 is what matches the geometry (mean
    dot with face normals 0.986 vs 0.07), so the encoding is chosen per model by that test
    when positions and triangles are given; normals are dropped if neither fits (< 0.5)."""
    st = _stream(g, base, 2)
    if not st or st[1] != 4: return None
    off = st[0]
    word = lambda i: struct.unpack_from("<I", g, base + off + 4 * i)[0]
    d10 = lambda i: (lambda d: _oct((d & 0x3ff) / 511.5 - 1, ((d >> 10) & 0x3ff) / 511.5 - 1))(word(i))
    d8 = lambda i: (lambda d: _oct((d & 0xff) / 127.5 - 1, ((d >> 8) & 0xff) / 127.5 - 1))(word(i))
    if pos is None or not tris: return d10
    acc = {}
    for t in tris[::max(1, len(tris) // 2000)]:
        p = [pos(i) for i in t]
        u = [p[1][k] - p[0][k] for k in range(3)]; w = [p[2][k] - p[0][k] for k in range(3)]
        n = (u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0])
        l = sum(x * x for x in n) ** 0.5
        if l: acc[t[0]] = [x / l for x in n]
    if not acc: return None
    fit = {f: sum(sum(a * b for a, b in zip(f(i), n)) for i, n in acc.items()) / len(acc) for f in (d10, d8)}
    best = max(fit, key=fit.get)
    return best if fit[best] >= 0.5 else None

def uvs(g, base):
    """Header word 3 (textured MS_VS_Transform variant): low 4 bits == 2 -> two f32;
    otherwise 16_16_SNORM (the shader's tbuffer format) times the float at +0x7c."""
    st = _stream(g, base, 3)
    if not st: return None
    off, stride, kind = st
    k = struct.unpack_from("<f", g, base + 0x7c)[0]
    if kind == 2:
        return lambda i: struct.unpack_from("<2f", g, base + off + stride * i)
    def get(i):
        a, b = struct.unpack_from("<2h", g, base + off + stride * i)
        return (max(a / 32767, -1.0) * k, max(b / 32767, -1.0) * k)
    return get

def vertex_count(g, base):
    """Vertices in a position buffer: its header lists up to 8 stream words (offset, stride);
    the positions run from their offset to the nearest other stream."""
    words = [struct.unpack_from("<I", g, base + 4 * i)[0] for i in range(8)]
    offs = [(w >> 7) & 0x1fffffc for w in words if w]
    off, stride = offs[0], (words[0] >> 3) & 60
    nxt = min((o for o in offs[1:] if o > off), default=None)
    return None if nxt is None or (nxt - off) % stride else (nxt - off) // stride

def meshlets(g, base):
    h0, ntri, nref = struct.unpack_from("<III", g, base)
    if not h0 & 0x80000000: raise ValueError("not a meshlet buffer")
    n = h0 & 0x1ffff
    tri0 = base + 12 + 16 * n; ref0 = tri0 + 4 * ntri
    tris = []
    for m in range(n):
        a, b = struct.unpack_from("<II", g, base + 12 + 16 * m)
        nv, npr = (a & 0x7f) + 1, ((a >> 7) & 0x7f) + 1
        vb = ((a << 2) & 0x10000) | (b & 0xffff); pb = ((a << 1) & 0x10000) | (b >> 16)
        refs = [struct.unpack_from("<H", g, ref0 + 2 * (vb + k))[0] for k in range(nv)]
        for p in range(npr):
            t = struct.unpack_from("<I", g, tri0 + 4 * (pb + p))[0]
            i0, i1, i2 = t & 0x7f, (t >> 7) & 0x7f, (t >> 14) & 0x7f
            if max(i0, i1, i2) >= nv: raise ValueError(f"meshlet {m}: local index out of range")
            tris.append((refs[i0], refs[i1], refs[i2]))
    return tris, n

def to_obj(g, pos_base, ml_base, path):
    get, off, stride = positions(g, pos_base)
    tris, n = meshlets(g, ml_base)
    nv = max(max(t) for t in tris) + 1
    if pos_base + off + nv * stride > len(g): raise ValueError("vertex index beyond the data")
    with open(path, "w") as f:
        for i in range(nv): f.write("v %.5f %.5f %.5f\n" % get(i))
        for t in tris: f.write("f %d %d %d\n" % (t[0] + 1, t[1] + 1, t[2] + 1))
    return nv, len(tris), n
