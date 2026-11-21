"""Vectorised (numpy) form of meshlets.py, for decoding models on demand in the map viewer.
Same layouts and the same normal-encoding choice as meshlets.py; check_against_scalar()
compares the two on real data (tools/rage/meshnp.py <carved dir> runs it)."""
import struct
import numpy as np
from meshlets import _stream


def triangles(g, base):
    """Triangle vertex indices (n, 3) uint32 of a meshlet buffer."""
    if base < 0 or base + 12 > len(g):
        raise ValueError("meshlet buffer outside the GPU block")
    h0, ntri, nref = struct.unpack_from("<III", g, base)
    if not h0 & 0x80000000:
        raise ValueError("not a meshlet buffer")
    n = h0 & 0x1FFFF
    if base + 12 + 16 * n > len(g):
        raise ValueError("meshlet records beyond the data")
    rec = np.frombuffer(g, "<u4", 4 * n, base + 12).reshape(n, 4)
    a, b = rec[:, 0].astype(np.int64), rec[:, 1].astype(np.int64)
    nv, npr = (a & 0x7F) + 1, ((a >> 7) & 0x7F) + 1
    vb = ((a << 2) & 0x10000) | (b & 0xFFFF)
    pb = ((a << 1) & 0x10000) | (b >> 16)
    tri0 = base + 12 + 16 * n
    ref0 = tri0 + 4 * ntri
    if ref0 + 2 * nref > len(g) or (pb + npr).max(initial=0) > ntri or (vb + nv).max(initial=0) > nref:
        raise ValueError("meshlet ranges beyond the data")
    td = np.frombuffer(g, "<u4", ntri, tri0).astype(np.int64)
    refs = np.frombuffer(g, "<u2", nref, ref0).astype(np.uint32)
    # every triangle dword with the meshlet it belongs to
    m = np.repeat(np.arange(n), npr)
    t = td[np.concatenate([np.arange(p, p + c) for p, c in zip(pb, npr)]) if n else np.zeros(0, np.int64)]
    loc = np.stack([t & 0x7F, (t >> 7) & 0x7F, (t >> 14) & 0x7F], 1)
    if (loc >= nv[m][:, None]).any():
        raise ValueError("local index out of range")
    return refs[vb[m][:, None] + loc]


def positions(g, base, nv):
    if base < 0 or base + 0x80 > len(g):
        raise ValueError("position buffer outside the GPU block")
    d0 = struct.unpack_from("<I", g, base)[0]
    stride, off = (d0 >> 3) & 60, (d0 >> 7) & 0x1FFFFFC
    if (d0 & 0xF) == 3 and stride == 12:
        # Plain float x, y, z (header "c0"): flat proxy parts whose extents equal the
        # drawable's bounding box on the samples checked.
        if base + off + 12 * nv > len(g):
            raise ValueError("vertex index beyond the data")
        return np.frombuffer(g, "<f4", 3 * nv, base + off).reshape(nv, 3).copy()
    bias = np.array(struct.unpack_from("<3f", g, base + 0x60))
    order = struct.unpack_from("<I", g, base + 0x6C)[0]
    scale = np.array(struct.unpack_from("<3f", g, base + 0x70))
    if stride != 8 or order > 2:
        raise ValueError(f"unsupported position stream: stride {stride} order {order}")
    if base + off + 8 * nv > len(g):
        raise ValueError("vertex index beyond the data")
    w = np.frombuffer(g, "<u4", 2 * nv, base + off).reshape(nv, 2).astype(np.int64)
    lo, hi = w[:, 0], w[:, 1]
    q = np.stack([lo >> 8, (lo & 0xFF) | ((hi & 0xFFFF) << 8), hi >> 16], 1).astype(np.float64)
    v = q * scale + bias
    return v[:, {2: [0, 1, 2], 1: [1, 2, 0], 0: [2, 1, 0]}[order]].astype(np.float32)


def _oct(x, y):
    z = 1.0 - np.abs(x) - np.abs(y)
    t = np.maximum(-z, 0.0)
    x = x + np.where(x >= 0, -t, t)
    y = y + np.where(y >= 0, -t, t)
    n = np.sqrt(x * x + y * y + z * z)
    n[n == 0] = 1
    return np.stack([x / n, y / n, z / n], 1)


def normals(g, base, pos, tris):
    """As meshlets.normals: 10:10 or 8:8 octahedral, chosen by agreement with face normals."""
    st = _stream(g, base, 2)
    nv = len(pos)
    if st and st[1] == 12 and st[2] == 3 and base + st[0] + 12 * nv <= len(g):
        # Float x, y, z normals (with the float position format); kept only if unit length.
        n = np.frombuffer(g, "<f4", 3 * nv, base + st[0]).reshape(nv, 3).astype(np.float64)
        l = np.linalg.norm(n, axis=1)
        return n.astype(np.float32) if np.abs(l - 1).mean() < 0.05 else None
    if not st or st[1] != 4 or base + st[0] + 4 * nv > len(g):
        return None
    d = np.frombuffer(g, "<u4", nv, base + st[0]).astype(np.int64)
    d10 = _oct((d & 0x3FF) / 511.5 - 1, ((d >> 10) & 0x3FF) / 511.5 - 1)
    d8 = _oct((d & 0xFF) / 127.5 - 1, ((d >> 8) & 0xFF) / 127.5 - 1)
    if not len(tris):
        return d10.astype(np.float32)
    s = tris[:: max(1, len(tris) // 2000)].astype(np.int64)
    p = pos.astype(np.float64)
    fn = np.cross(p[s[:, 1]] - p[s[:, 0]], p[s[:, 2]] - p[s[:, 0]])
    l = np.linalg.norm(fn, axis=1)
    keep = l > 0
    if not keep.any():
        return None
    # meshlets.normals keys by first vertex (last face wins); reproduce that
    first = s[keep, 0]
    fn = fn[keep] / l[keep, None]
    _, last = np.unique(first[::-1], return_index=True)
    idx = len(first) - 1 - last
    v, fn = first[idx], fn[idx]
    fits = [(np.sum(c[v] * fn, 1).mean(), c) for c in (d10, d8)]
    best = max(fits, key=lambda f: f[0])
    return best[1].astype(np.float32) if best[0] >= 0.5 else None


def uvs(g, base, nv):
    st = _stream(g, base, 3)
    if not st:
        return None
    off, stride, kind = st
    if not stride or base + off + stride * nv > len(g):
        return None
    k = struct.unpack_from("<f", g, base + 0x7C)[0]
    if kind == 2:
        a = np.frombuffer(g, np.uint8, stride * nv, base + off).reshape(nv, stride)[:, :8].copy().view("<f4")
        return a.astype(np.float32)
    a = np.frombuffer(g, np.uint8, stride * nv, base + off).reshape(nv, stride)[:, :4].copy().view("<i2").astype(np.float64)
    return (np.maximum(a / 32767, -1.0) * k).astype(np.float32)


def mesh(g, pos_base, ml_base):
    """(positions, normals or None, uvs or None, triangle indices) of one model part."""
    t = triangles(g, ml_base)
    nv = int(t.max()) + 1 if len(t) else 0
    p = positions(g, pos_base, nv)
    return p, normals(g, pos_base, p, t), uvs(g, pos_base, nv), t


def check_against_scalar(vfile, gfile):
    import meshlets as M
    from modelexport import meshes
    b, g = open(vfile, "rb").read(), open(gfile, "rb").read()
    _, ms = meshes(b, g)
    n = 0
    for k, r in ms:
        if isinstance(r, str):
            continue
        base, tris, nv = r
        ml_base = [p for p, _ in __import__("modelexport").records(b) if p + 12 <= len(g) and struct.unpack_from("<I", g, p)[0] & 0x80000000][k]
        p, nr, uv, t = mesh(g, base, ml_base)
        assert t.tolist() == [list(x) for x in tris], "triangles differ"
        get = M.positions(g, base)[0]
        assert np.allclose(p, [get(i) for i in range(nv)], atol=1e-3), "positions differ"
        sn = M.normals(g, base, get, tris)
        assert (sn is None) == (nr is None), "normal choice differs"
        if sn is not None:
            assert np.allclose(nr, [sn(i) for i in range(nv)], atol=1e-4), "normals differ"
        su = M.uvs(g, base)
        assert (su is None) == (uv is None)
        if su is not None:
            assert np.allclose(uv, [su(i) for i in range(nv)], atol=1e-4), "uvs differ"
        n += 1
    return n


if __name__ == "__main__":
    import sys, glob, os, random
    files = sorted(glob.glob(os.path.join(sys.argv[1], "*.bin")))
    random.seed(1)
    picks = random.sample(range(len(files) - 1), min(int(sys.argv[2]) if len(sys.argv) > 2 else 400, len(files) - 1))
    parts = 0
    for i in picks:
        if os.path.getsize(files[i]) < 4 << 20:
            parts += check_against_scalar(files[i], files[i + 1])
    print("parts identical to the scalar decoder:", parts)
