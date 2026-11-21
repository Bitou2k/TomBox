"""Export every meshlet model found in carved resource blocks (virtual block followed by its
GPU block) to OBJ. Buffer records in the virtual block: u32 size in dwords, u16 4, u16 8|9,
u32, u64 GPU address (0x60000000-based) - inferred from the sample model, validated per model
by the meshlet checks in meshlets.py and by the exact vertex-count pairing below."""
import sys, os, re, glob, struct
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from meshlets import positions, meshlets, vertex_count, normals, uvs

def records(b):
    out = []
    for o in range(0, len(b) - 24, 8):
        n, a, c = struct.unpack_from("<IHH", b, o); p = struct.unpack_from("<Q", b, o + 16)[0]
        if a == 4 and c in (8, 9) and n and 0x60000000 <= p < 0x70000000: out.append((p - 0x60000000, n * 4))
    return out

def export(vfile, gfile, out):
    b = open(vfile, "rb").read()
    if b[:8] != b"\0" * 8: return []
    recs = records(b)
    if not recs: return []
    g = open(gfile, "rb").read()
    m = re.search(rb"([A-Za-z0-9_]+)\.#d", b); name = m.group(1).decode() if m else os.path.basename(vfile)[:-4]
    pos = [(p, sz) for p, sz in recs if p + 0x80 <= len(g) and g[p:p + 2] == b"B@"]
    mls = [(p, sz) for p, sz in recs if p + 12 <= len(g) and struct.unpack_from("<I", g, p)[0] & 0x80000000]
    res = []
    for k, (mp, _) in enumerate(mls):
        try:
            tris, n = meshlets(g, mp)
        except Exception as e:
            res.append((name, k, f"meshlets: {e}")); continue
        if not tris: continue
        nv = max(max(t) for t in tris) + 1 if tris else 0
        cand = [pp for pp, _ in pos if vertex_count(g, pp) == nv]
        if len(cand) == 0:
            cand = [pp for pp, _ in pos if (vertex_count(g, pp) or 0) > nv]  # shared buffer
        if len(cand) > 1 and k < len(pos) and pos[k][0] in cand:
            cand = [pos[k][0]]  # same vertex count: buffers are listed in the same order (inferred)
        if len(cand) != 1:
            res.append((name, k, f"{len(cand)} position buffers for {nv} vertices")); continue
        get, off, st = positions(g, cand[0])
        path = f"{out}/{name}_{os.path.basename(vfile)[:-4]}_{k}.obj"
        with open(path, "w") as f:
            nrm, uv = normals(g, cand[0], get, tris), uvs(g, cand[0])
            for i in range(nv): f.write("v %.5f %.5f %.5f\n" % get(i))
            if nrm:
                for i in range(nv): f.write("vn %.4f %.4f %.4f\n" % nrm(i))
            if uv:
                for i in range(nv):
                    u = uv(i); f.write("vt %.5f %.5f\n" % (u[0], 1 - u[1]))
            for t in tris:
                a, b, c = (x + 1 for x in t)
                if uv and nrm: f.write(f"f {a}/{a}/{a} {b}/{b}/{b} {c}/{c}/{c}\n")
                elif nrm: f.write(f"f {a}//{a} {b}//{b} {c}//{c}\n")
                elif uv: f.write(f"f {a}/{a} {b}/{b} {c}/{c}\n")
                else: f.write(f"f {a} {b} {c}\n")
        res.append((name, k, f"ok {nv} verts {len(tris)} tris"))
    return res

if __name__ == "__main__":
    src, out = sys.argv[1], sys.argv[2]; os.makedirs(out, exist_ok=True)
    files = sorted(glob.glob(src + "/*.bin")); ok = bad = 0; why = {}
    for i, f in enumerate(files[:-1]):
        if os.path.getsize(f) > 4 << 20: continue
        try: r = export(f, files[i + 1], out)
        except Exception as e: r = [("?", 0, f"error {e}")]
        for name, k, s in r:
            if s.startswith("ok"): ok += 1
            else: bad += 1; why[s.split(":")[0][:40]] = why.get(s.split(":")[0][:40], 0) + 1
    print("exported", ok, "failed", bad); print(sorted(why.items(), key=lambda x: -x[1])[:10])
