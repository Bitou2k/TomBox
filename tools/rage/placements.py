"""Find map placement (entity) records in carved level resources.

Record layout, 0x90 bytes, inferred from map resources in levels.rpf (South Beach area 38
file: fountain, pool cabanas 01-12, railings, jetties) and checked by the validation below:
  +0x04 u64   unknown id (differs per record)
  +0x0c f32x4 quaternion (x, y, z, w), unit length: the INVERSE of the object's rotation
              (world = conj(q) * v * q + pos). Checked on a hotel facade: with the conjugate the
              glass panels form level floor bands; with q as-is they zigzag.
  +0x34 f32x3 position (world units)
  +0x40 u32   joaat(model name) - equals the name of a drawable resource
  +0x44 u32   flags (0x00180090 / 0x00180094 seen)
  +0x50 f32   scale XY, +0x54 f32 scale Z
  +0x58 u16   index, +0x5a u16 parent (0xffff = none)
  +0x5c f32   draw distance (-1 = default)
  +0x70 u64   same value in every record of a file (unknown)
Validation: the positions of all records in a 4 GiB slice of levels.rpf plot the city
(street grid, coastline, islands); balusters placed by them form continuous balustrades.
Records are found by scanning, not through the resource's own header (not decoded yet): a
record is accepted when its quaternion is unit length, its scales are positive and plausible,
and the record 0x90 bytes before or after it passes the same checks.
"""
import math, struct, sys, os, glob, re, json


def joaat(s):
    h = 0
    for c in s.lower().encode():
        h = (h + c) & 0xFFFFFFFF
        h = (h + (h << 10)) & 0xFFFFFFFF
        h ^= h >> 6
    h = (h + (h << 3)) & 0xFFFFFFFF
    h ^= h >> 11
    return (h + (h << 15)) & 0xFFFFFFFF


STRIDE = 0x90


def _record(b, o):
    if o < 0 or o + STRIDE > len(b):
        return None
    q = struct.unpack_from("<4f", b, o + 0x0C)
    if not all(math.isfinite(x) for x in q) or abs(sum(x * x for x in q) - 1) > 2e-3:
        return None
    p = struct.unpack_from("<3f", b, o + 0x34)
    if not all(math.isfinite(x) and abs(x) < 50000 for x in p):
        return None
    sxy, sz = struct.unpack_from("<2f", b, o + 0x50)
    if not (0.001 < sxy < 1000 and 0.001 < sz < 1000):
        return None
    name, flags = struct.unpack_from("<II", b, o + 0x40)
    idx, parent = struct.unpack_from("<HH", b, o + 0x58)
    lod = struct.unpack_from("<f", b, o + 0x5C)[0]
    if not math.isfinite(lod):
        return None
    return dict(off=o, name=name, flags=flags, pos=p, quat=q, scale=(sxy, sz), index=idx, parent=parent, lod=lod)


def _candidates(b):
    """4-byte offsets whose quaternion and scale fields pass a vectorised pre-check."""
    import numpy as np
    n = (len(b) - STRIDE) // 4
    if n <= 0:
        return []
    a = np.frombuffer(b[:(len(b) // 4) * 4], dtype=np.float32).astype(np.float64)
    with np.errstate(all="ignore"):
        q = a[3:3 + n] ** 2 + a[4:4 + n] ** 2 + a[5:5 + n] ** 2 + a[6:6 + n] ** 2
        sxy, sz = a[20:20 + n], a[21:21 + n]
        ok = (np.abs(q - 1) < 2e-3) & (sxy > 0.001) & (sxy < 1000) & (sz > 0.001) & (sz < 1000)
    return (np.nonzero(ok)[0] * 4).tolist()


def records(b):
    out = []
    seen = set()
    for o in _candidates(b):
        if o in seen:
            continue
        r = _record(b, o)
        if not r:
            continue
        if not (_record(b, o + STRIDE) or _record(b, o - STRIDE)):
            continue
        # walk the run
        k = o
        while True:
            r = _record(b, k)
            if not r:
                break
            out.append(r)
            seen.add(k)
            k += STRIDE
    return out


if __name__ == "__main__":
    src, out = sys.argv[1], sys.argv[2]
    names = {}
    files = sorted(glob.glob(os.path.join(src, "*.bin")))
    for f in files:
        b = open(f, "rb").read(1 << 14)
        for m in re.findall(rb"([A-Za-z0-9_]{3,})\.#dr", b):
            names[joaat(m.decode())] = (m.decode(), os.path.basename(f))
    ents = []
    for f in files:
        b = open(f, "rb").read()
        if b[16:20] != b"0DRP":
            continue
        for r in records(b):
            r["file"] = os.path.basename(f)
            n = names.get(r["name"])
            r["model"] = n[0] if n else None
            r["model_file"] = n[1] if n else None
            ents.append(r)
    json.dump(ents, open(out, "w"))
    resolved = sum(1 for e in ents if e["model"])
    print("entities", len(ents), "in files", len({e["file"] for e in ents}), "names resolved to drawables in this slice", resolved)
