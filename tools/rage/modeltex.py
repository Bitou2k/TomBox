"""Texture references of a carved drawable, resolved to texture-pack entries.

A drawable's virtual block holds 32-byte texture records: u32 6, u32 0, u32 joaat(name),
u32 0, u64 pointer (record + 5), u64 key. The key equals an entry of a texture pack's sorted
u64 hash table (observed: every texture of the minigame's own map pack is referenced this way,
and 6,615 of 7,361 distinct minigame keys resolve to some pack)."""
import sys, os, io, json, glob, struct
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

SRGB = {29, 72, 75, 78, 91, 99}


def refs(b):
    out = []
    for o in range(0, len(b) - 32, 8):
        k, z, h, z2, p, key = struct.unpack_from("<IIIIQQ", b, o)
        if k == 6 and z == 0 and z2 == 0 and h and p - 0x50000000 == o + 5:
            out.append((o, h, key))
    return out


def build_index(game_dir, path):
    from ttpf import Pack
    idx = {}
    pats = ["data/ttdb/*/*.ttpf", "pack/*/data/ttdb/*/*.ttpf", "pack/*/data/ttdb/*.ttpf"]
    for pat in pats:
        for pk in sorted(glob.glob(os.path.join(game_dir, pat))):
            p = Pack(pk)
            for h, t in zip(p.hashes, p.textures):
                idx.setdefault(str(h), [pk, t["index"], t["fmt"], t["w"], t["h"], t["codecs"]])
    json.dump(idx, open(path, "w"))
    return idx


_packs = {}


def texture_png(entry, out_path):
    """Decode one indexed texture (mip 0) to PNG. Returns None or a failure reason."""
    from ttpf import Pack, CodecError
    from ttpfexport import untile, layout_of, to_image
    pk, i = entry[0], entry[1]
    if pk not in _packs:
        _packs[pk] = Pack(pk)
    p = _packs[pk]
    t = p.textures[i]
    try:
        data = p.texture_bytes(t)
    except CodecError:
        return "chunk codec not supported"
    px, why = untile(data, t, layout_of(t))
    if px is None:
        return why
    try:
        img = to_image(t, px)
    except (NotImplementedError, ValueError, OSError) as e:
        return f"decode: {e}"
    img.save(out_path)
    return None
