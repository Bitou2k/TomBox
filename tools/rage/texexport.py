"""Export textures from carved texture-dictionary blocks: untile with the title's own
xg_xs.dll layout computer, write DDS, convert mip 0 to PNG with Pillow."""
import sys, os, glob, struct, io, collections
sys.path.insert(0, os.path.dirname(__file__))
from txd import parse
from xgtry import computer
from PIL import Image
LAYOUTS = (0x105,)
BC = {70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,94,95,96,97,98,99}
def dds(w, h, fmt, data):
    hd = bytearray(128 + 20)
    struct.pack_into("<4sIIIIIII", hd, 0, b"DDS ", 124, 0x1007 | 0x80000, h, w, len(data), 0, 1)
    struct.pack_into("<II4s", hd, 76, 32, 4, b"DX10")
    struct.pack_into("<I", hd, 108, 0x1000)
    struct.pack_into("<IIIII", hd, 128, fmt, 3, 0, 1, 0)
    return bytes(hd) + data
def untile(g, base, t, L):
    w, h, fmt = t["w"], t["h"], t["fmt"]
    c = computer(w, h, t["mips"], fmt, L, max(t["depth"], 1))
    if not c: return None
    bpe = c["bpe"]; blk = 4 if fmt in BC else 1
    we, he = (w + blk - 1) // blk, (h + blk - 1) // blk
    if base + c["total"] > len(g): return None
    out = bytearray(we * he * bpe); off = c["off"]
    for y in range(he):
        for x in range(we):
            o = base + off(0, x, y)
            out[(y*we+x)*bpe:(y*we+x+1)*bpe] = g[o:o+bpe]
    return bytes(out)
def score(img):
    im = img.convert("RGB").resize((min(img.width, 256), min(img.height, 256)))
    px = im.tobytes(); w = im.width; s = 0; n = 0
    for y in range(0, im.height, 2):
        r = px[y*w*3:(y+1)*w*3]
        s += sum(abs(r[i] - r[i+3]) for i in range(0, len(r) - 3)); n += len(r)
    return s / max(n, 1)
def main(src, out, limit, only=None):
    os.makedirs(out, exist_ok=True)
    files = sorted(glob.glob(src + "/*.bin")); win = collections.Counter(); done = 0; fails = collections.Counter()
    for i, f in enumerate(files):
        if os.path.getsize(f) > 1 << 20 or i + 1 >= len(files): continue
        r = parse(open(f, "rb").read())
        if not r: continue
        g = open(files[i + 1], "rb").read()
        for t in r:
            if done >= limit: break
            if only and t["fmt"] not in only: continue
            base = t["phys"] - 0x60000000
            best = None
            for L in LAYOUTS:
                data = untile(g, base, t, L)
                if data is None: continue
                try: img = Image.open(io.BytesIO(dds(t["w"], t["h"], t["fmt"], data))); img.load()
                except Exception as e: fails[f"fmt {t['fmt']}: {str(e)[:60]}"] += 1; break
                s = score(img)
                if best is None or s < best[0]: best = (s, L, img, data)
            if not best: fails["no layout"] += 1; continue
            s, L, img, data = best
            win[(t["tile"][1], t["fmt"], hex(L))] += 1
            name = f"{t['name']}_{os.path.basename(f)[:-4]}"
            (img.convert("RGB") if t["fmt"] in (95, 96) or img.mode not in ("RGB", "RGBA", "L") else img).save(f"{out}/{name}.png")
            done += 1
    print("exported", done); print("layout wins", win.most_common(20)); print("failures", fails.most_common(10))
if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2], int(sys.argv[3]), set(map(int, sys.argv[4].split(","))) if len(sys.argv) > 4 else None)
