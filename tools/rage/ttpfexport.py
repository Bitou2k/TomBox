"""Export textures from a texture pack (.ttpf) to PNG: untile mip 0 / slice 0 with the
console layout library, wrap it in a DDS, decode with Pillow.

Tile layout (chosen by comparing every layout value on sample textures, see the development
log): record field d1 bits 24-26 = 0 -> layout 0x2, 1-3 -> 0x105. Other values (cube maps have
4 plus higher bits) and array/cube records are rejected: no layout tried reproduces them.
Uncompressed R8G8B8A8 records (graphics pack) have not been confirmed visually.

usage: ttpfexport.py <pack.ttpf> <out dir> [limit] [first index] [stride]
"""
import sys, os, io, collections
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ttpf import Pack, CodecError
from texexport import dds, BC
from xgtry import computer
from PIL import Image


def untile(data, t, layout, mip=0):
    """Linear pixels (or blocks) of one mip level, size (w >> mip, h >> mip), at least 1."""
    w, h, fmt = t["w"], t["h"], t["fmt"]
    if mip >= max(t["mips"], 1):
        return None, f"mip {mip} not stored"
    c = computer(w, h, max(t["mips"], 1), fmt, layout, t["arr"])
    if not c:
        return None, "layout computer refused"
    if c["total"] > len(data):
        return None, f"layout needs {c['total']} bytes, pack span is {len(data)}"
    bpe = c["bpe"]
    blk = 4 if fmt in BC else 1
    mw, mh = max(w >> mip, 1), max(h >> mip, 1)
    we, he = (mw + blk - 1) // blk, (mh + blk - 1) // blk
    out = bytearray(we * he * bpe)
    off = c["off"]
    for y in range(he):
        for x in range(we):
            o = off(mip, x, y)
            out[(y * we + x) * bpe:(y * we + x + 1) * bpe] = data[o:o + bpe]
    return bytes(out), None


def _footprints(pack):
    """Sorted (start, end) byte ranges each texture of the pack actually occupies: its first
    page plus its layout's total size (or its page span when the layout is unknown)."""
    if getattr(pack, "_fp", None) is None:
        fp = []
        for t in pack.textures:
            lo = t["page"] << 12
            layout = layout_of(t)
            c = computer(t["w"], t["h"], max(t["mips"], 1), t["fmt"], layout, t["arr"]) if layout is not None else None
            fp.append((lo, lo + (c["total"] if c else t["bytes"]), t["index"]))
        fp.sort()
        pack._fp = fp
    return pack._fp


def decode(pack, t, max_side=1 << 30, overlap=False):
    """Image of the largest mip level of `t` whose side is <= max_side and whose bytes lie in
    the texture's own page span (up to the next texture's first page); falls back to smaller
    mips. Returns (image, mip); raises ValueError with the reason otherwise (CodecError for
    unsupported chunks).

    ~3.6% of records have a layout larger than their span: other textures start inside it.
    With overlap=True, bytes past the span are read when they avoid every other texture's
    footprint. That recovers many of them (small textures do sit in unused parts of larger
    layouts), but about a quarter of a visual sample came out garbled anyway, for a reason not
    found (no record field separates them), so it is off by default."""
    import bisect
    layout = layout_of(t)
    if layout is None:
        raise ValueError("tile layout unknown for this texture record (cube/array or unseen selector)")
    w, h, fmt, mips = t["w"], t["h"], t["fmt"], max(t["mips"], 1)
    c = computer(w, h, mips, fmt, layout, t["arr"])
    if not c:
        raise ValueError("layout computer refused")
    bpe, blk, off = c["bpe"], (4 if fmt in BC else 1), c["off"]
    lo = t["page"] << 12
    mip = 0
    while max(w, h) >> mip > max_side and mip + 1 < mips:
        mip += 1
    why = None
    for m in range(mip, mips):
        mw, mh = max(w >> m, 1), max(h >> m, 1)
        we, he = (mw + blk - 1) // blk, (mh + blk - 1) // blk
        offs = [off(m, x, y) for y in range(he) for x in range(we)]
        end = max(offs) + bpe
        if end > t["bytes"] and not overlap:
            why = f"mip {m} extends past the record's page span"
            continue
        if end > t["bytes"]:
            others = [(a - lo, b - lo) for a, b, i in _footprints(pack) if i != t["index"] and a < lo + end and b > lo]
            starts = sorted(o[0] for o in others)
            ends = {a: b for a, b in others}
            hit = False
            for o in offs:
                k = bisect.bisect_right(starts, o + bpe - 1) - 1
                if k >= 0 and ends[starts[k]] > o:
                    hit = True
                    break
            if hit:
                why = f"mip {m} overlaps another texture's data"
                continue
        data = pack.read(lo, end)
        out = bytearray(we * he * bpe)
        for i, o in enumerate(offs):
            out[i * bpe:(i + 1) * bpe] = data[o:o + bpe]
        return to_image(t, bytes(out), m), m
    raise ValueError(why or "no readable mip")


# Formats Pillow lacks, decoded as the format with the same bit layout (sRGB only changes how
# the sampler converts the value).
SAME_BITS = {29: 28, 72: 71, 75: 74, 78: 77, 99: 98, 91: 87, 93: 87}


def to_image(t, px, mip=0):
    fmt = SAME_BITS.get(t["fmt"], t["fmt"])
    if fmt in (61, 65):  # R8_UNORM, A8_UNORM: one byte per pixel, shown as grey
        return Image.frombytes("L", (max(t["w"] >> mip, 1), max(t["h"] >> mip, 1)), px)
    if fmt == 87:  # B8G8R8A8: swap to R8G8B8A8
        a = bytearray(px)
        a[0::4], a[2::4] = px[2::4], px[0::4]
        px, fmt = bytes(a), 28
    img = Image.open(io.BytesIO(dds(max(t["w"] >> mip, 1), max(t["h"] >> mip, 1), fmt, px)))
    img.load()
    return img if img.mode in ("RGB", "RGBA", "L") else img.convert("RGB")


def layout_of(t):
    """Layout value for the console layout library, or None if not known for this record."""
    sel = (t["d1"] >> 24) & 7
    if t["arr"] > 1 or t["d1"] >> 27:
        return None
    return {0: 0x2, 1: 0x105, 2: 0x105, 3: 0x105}.get(sel)


def export(pack_path, out, limit=1 << 30, first=0, stride=1):
    os.makedirs(out, exist_ok=True)
    p = Pack(pack_path)
    stem = os.path.splitext(os.path.basename(pack_path))[0]
    ok = 0
    fails = collections.Counter()
    for t in p.textures[first::stride][:limit]:
        try:
            data = p.texture_bytes(t)
        except CodecError:
            fails["chunk not zlib (unsupported codec)"] += 1
            continue
        except ValueError as e:
            fails[f"read: {str(e)[:50]}"] += 1
            continue
        layout = layout_of(t)
        if layout is None:
            fails[f"layout unknown (d1 {t['d1'] >> 24:#x}, array {t['arr']})"[:40]] += 1
            continue
        px, why = untile(data, t, layout)
        if px is None:
            fails[why.split(",")[0]] += 1
            continue
        try:
            img = to_image(t, px)
        except (NotImplementedError, ValueError, OSError) as e:
            fails[f"fmt {t['fmt']}: {str(e)[:50]}"] += 1
            continue
        img.save(f"{out}/{stem}_{t['index']:05d}_{t['w']}x{t['h']}_f{t['fmt']}.png")
        ok += 1
    print(stem, "textures", len(p.textures), "exported", ok, "failed", sum(fails.values()), fails.most_common(8))
    return ok


if __name__ == "__main__":
    a = sys.argv
    export(a[1], a[2], int(a[3]) if len(a) > 3 else 1 << 30, int(a[4]) if len(a) > 4 else 0, int(a[5]) if len(a) > 5 else 1)
