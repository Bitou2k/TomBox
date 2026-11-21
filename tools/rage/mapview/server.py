"""Map viewer for the second title's level package, served on 127.0.0.1 only.

Nothing is extracted: the server loads the mount index built by `rpf-index` (files.tsv,
entities.bin) and, when the browser asks for a model, reads that file's streams from the
original levels.rpf by offset, inflates them in memory and decodes it (drawable.py for the
structure, meshnp.py for the geometry). Textures are read on request from the texture packs
(.ttpf) through a key index (modeltex.build_index) and sent as PNG at most 512 px wide.
The page (index.html) places models by the placement records.

What is shown and what is not (see docs/TECHNICAL_DEBT.md, "Second title asset tools"):
  * positions, rotations (inverse quaternion convention) and scales come from the records;
  * a model is its named resource (name.#dr / name.#ft, or a member of a multi-model file)
    plus the file right after it (GPU block); each geometry part is decoded from its own
    buffer pointers and drawn with its shader's DiffuseTex texture when that decodes (else
    the first sRGB-format texture of the shader, e.g. layer 1 of layered terrain);
  * placements whose model name is not carried by any file in levels.rpf are drawn as points;
  * files whose placements all lie near the origin (presumably interiors in local space)
    are left out: their placement in the world is not decoded;
  * only the colour texture: other maps (normal, specular, masks) and shader parameters are
    not applied, and layered materials show their DiffuseTex layer only;
  * level of detail is approximated: the game's parent links are not decoded, so the page
    shows full-detail models in near tiles, "_lod" models in middle tiles and "_slod" models
    in far tiles (by model name), never two levels in one tile.

usage: server.py <levels.rpf> <index dir> <ttpf_index.json> [port]
"""
import sys, os, io, json, struct, zlib, threading, collections
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler
from urllib.parse import urlparse, parse_qs
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
import drawable  # noqa: E402
import meshnp  # noqa: E402

ENT = np.dtype([("file", "<u4"), ("at", "<u4"), ("id", "<u8"), ("q", "<f4", 4), ("p", "<f4", 3),
                ("name", "<u4"), ("flags", "<u4"), ("s", "<f4", 2), ("idx", "<u2"), ("par", "<u2"),
                ("lod", "<f4"), ("pad", "<u4")])
TILE = 250.0  # metres per tile side
TEX_MAX = 512  # largest texture side sent to the page


def joaat(s):
    h = 0
    for c in s.lower().encode():
        h = (h + c) & 0xFFFFFFFF
        h = (h + (h << 10)) & 0xFFFFFFFF
        h ^= h >> 6
    h = (h + (h << 3)) & 0xFFFFFFFF
    h ^= h >> 11
    return (h + (h << 15)) & 0xFFFFFFFF


def inflate_streams(raw):
    """Concatenated output of the zlib streams in `raw` (gaps of a few bytes between them)."""
    out = bytearray()
    i = 0
    while i + 2 <= len(raw):
        if raw[i] != 0x78 or ((raw[i] << 8) | raw[i + 1]) % 31:
            i += 1
            continue
        d = zlib.decompressobj()
        try:
            out += d.decompress(raw[i:])
        except zlib.error:
            i += 1
            continue
        if not d.eof:
            raise ValueError("stream does not end inside the file's extent")
        i = len(raw) - len(d.unused_data)
    return bytes(out)


class Textures:
    def __init__(self, index_path):
        self.idx = json.load(open(index_path))
        self.packs = {}
        self.lock = threading.Lock()
        self.cache = collections.OrderedDict()

    def png(self, key):
        """PNG of the texture with this pack key, at the largest stored mip <= TEX_MAX."""
        from ttpf import Pack, CodecError
        from ttpfexport import untile, layout_of, to_image
        if key in self.cache:
            return self.cache[key]
        e = self.idx.get(str(key))
        if not e:
            raise KeyError(f"texture key {key:#x} is in no indexed texture pack")
        with self.lock:
            p = self.packs.get(e[0]) or self.packs.setdefault(e[0], Pack(e[0]))
            t = p.textures[e[1]]
            try:
                data = p.texture_bytes(t)
            except CodecError as ex:
                raise ValueError(f"texture data uses an unsupported chunk codec ({ex})")
        mip = 0
        while max(t["w"], t["h"]) >> mip > TEX_MAX and mip + 1 < max(t["mips"], 1):
            mip += 1
        layout = layout_of(t)
        if layout is None:
            raise ValueError("tile layout unknown for this texture record (cube/array)")
        px, why = untile(data, t, layout, mip)
        if px is None:
            raise ValueError(why)
        try:
            img = to_image(t, px, mip)
        except (NotImplementedError, OSError) as ex:
            raise ValueError(f"format {t['fmt']}: {ex}")
        buf = io.BytesIO()
        img.save(buf, "PNG", compress_level=1)
        out = buf.getvalue()
        self.cache[key] = out
        if len(self.cache) > 1500:
            self.cache.popitem(last=False)
        return out


class Package:
    def __init__(self, pkg, index_dir):
        info = dict(l.rstrip("\n").split("\t", 1) for l in open(os.path.join(index_dir, "index.txt")))
        st = os.stat(pkg)
        if int(info["length"]) != st.st_size:
            raise SystemExit(f"index was built for a package of {info['length']} bytes; {pkg} has {st.st_size}")
        self.f = open(pkg, "rb")
        self.lock = threading.Lock()
        self.files = []  # (offset, end, size)
        self.model_file = {}  # name hash -> (file index, name)
        for line in open(os.path.join(index_dir, "files.tsv"), encoding="utf-8"):
            r = line.rstrip("\n").split("\t")
            if r[0] == "index":
                continue
            self.files.append((int(r[1], 16), int(r[2], 16), int(r[4])))
            for n in (r[7].split(";") if len(r) > 7 and r[7] else []):
                if n.endswith(".#dr") or n.endswith(".#ft"):
                    # one name: the file is that model; several: a multi-model file whose
                    # members are found by name hash (drawable.drawables)
                    self.model_file.setdefault(joaat(n[:-4]), (len(self.files) - 1, n))
        e = np.fromfile(os.path.join(index_dir, "entities.bin"), ENT)
        e = e[np.isin(e["flags"] >> 16, [0x18, 0x1C])]  # flag forms seen on verified records
        # Files whose placements all lie within 250 m of the origin (5,351 files, ~400k
        # records): presumably interiors in their own local space. Where they go in the world
        # is not decoded, so they are left out of the map.
        far = (np.abs(e["p"][:, :2]) >= 250).any(1)
        world_files = np.unique(e["file"][far])
        local = ~np.isin(e["file"], world_files)
        self.local_count = int(local.sum())
        e = e[~local]
        self.ents = e
        known = np.array(sorted(self.model_file), np.uint32)
        self.resolved = np.isin(e["name"], known)
        # Detail level from the model's own name: 2 = "slod", 1 = "lod", 0 = anything else.
        # The game's parent links (placement +0x58/+0x5a) are not decoded, so the page uses
        # this with distance bands instead (an approximation of the game's switching).
        lv = {h: (2 if "slod" in n.lower() else 1 if "lod" in n.lower() else 0) for h, (_, n) in self.model_file.items()}
        self.level = np.array([lv.get(int(h), 0) for h in e["name"]], np.uint8)
        tx = np.floor(e["p"][:, 0] / TILE).astype(np.int64)
        ty = np.floor(e["p"][:, 1] / TILE).astype(np.int64)
        self.tiles = collections.defaultdict(list)
        for i, k in enumerate(zip(tx.tolist(), ty.tolist())):
            self.tiles[k].append(i)
        self.cache = collections.OrderedDict()
        self.fmt_of = None

    def read_file(self, i):
        off, end, size = self.files[i]
        with self.lock:
            self.f.seek(off)
            raw = self.f.read(end - off)
        data = inflate_streams(raw)
        if len(data) != size:
            raise ValueError(f"file {i} inflated to {len(data)} bytes, index says {size}")
        return data

    def model(self, h):
        """Binary model: u32 header length, JSON header (name, parts, errors), then per part
        positions f32x3, [normals f32x3], [uvs f32x2], indices u32x3."""
        if h in self.cache:
            self.cache.move_to_end(h)
            return self.cache[h]
        if h not in self.model_file:
            raise KeyError(f"no file in the package carries a model named with hash {h:#010x}")
        fi, name = self.model_file[h]
        b, g = self.read_file(fi), self.read_file(fi + 1)
        ds = drawable.drawables(b)
        d = next((o for k, o in ds if k is None or k == h), None)
        if d is None:
            raise ValueError(f"{name}: not among the members of its multi-model file")
        parts, blobs, errors = [], [], []
        try:
            S = drawable.shaders(b, d)
            M = drawable.geometries(b, d)
        except (drawable.FormatError, struct.error) as ex:
            raise ValueError(f"{name}: {ex}")
        for mi, geos in enumerate(M):
            for gi, geo in enumerate(geos):
                try:
                    t = meshnp.triangles(g, geo["mesh"])
                    if len(t) != geo["triangles"]:
                        raise ValueError(f"{len(t)} triangles decoded, geometry says {geo['triangles']}")
                    nv = int(t.max()) + 1
                    p = meshnp.positions(g, geo["pos"], nv)
                    n = meshnp.normals(g, geo["pos"], p, t)
                    uv = meshnp.uvs(g, geo["pos"], nv)
                except (ValueError, struct.error) as ex:
                    errors.append(f"model {mi} part {gi}: {ex}")
                    continue
                key = drawable.colour_texture(S[geo["shader"]], self.fmt_of) if geo["shader"] < len(S) else None
                parts.append(dict(nv=nv, nt=len(t), normals=n is not None, uvs=uv is not None,
                                  tex=f"{key:016x}" if key is not None and uv is not None else None))
                blobs.append(p.astype("<f4").tobytes())
                if n is not None:
                    blobs.append(n.astype("<f4").tobytes())
                if uv is not None:
                    blobs.append(uv.astype("<f4").tobytes())
                blobs.append(t.astype("<u4").tobytes())
        head = json.dumps(dict(name=name, parts=parts, errors=errors)).encode()
        head += b" " * (-len(head) % 4)
        out = struct.pack("<I", len(head)) + head + b"".join(blobs)
        self.cache[h] = out
        if len(self.cache) > 2000:
            self.cache.popitem(last=False)
        return out

    def tile(self, tx, ty, level=None):
        idx = self.tiles.get((tx, ty), [])
        if level is not None:
            idx = [i for i in idx if self.level[i] == level]
        e = self.ents[idx]
        res = self.resolved[idx]
        a = np.zeros((len(idx), 12), "<f4")
        a[:, 0:3] = e["p"]
        a[:, 3:7] = e["q"]
        a[:, 7:9] = e["s"]
        a[:, 9] = e["lod"]
        hdr = struct.pack("<I", len(idx))
        return (hdr + a.tobytes() + e["name"].astype("<u4").tobytes() + res.astype(np.uint8).tobytes()
                + self.level[idx].tobytes())

    def overview(self, step):
        p = self.ents["p"][::step]
        return struct.pack("<I", len(p)) + p.astype("<f4").tobytes()

    def info(self):
        p = self.ents["p"]
        return dict(entities=int(len(self.ents)), resolved=int(self.resolved.sum()), models=len(self.model_file),
                    local_left_out=self.local_count, tile=TILE, min=p.min(0).tolist(), max=p.max(0).tolist(),
                    tiles=[[x, y] + np.bincount(self.level[v], minlength=3)[:3].tolist() for (x, y), v in self.tiles.items()])


def handler(pkg, tex):
    class H(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def send(self, code, body, ctype):
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            u = urlparse(self.path)
            q = parse_qs(u.query)
            try:
                if u.path in ("/", "/index.html"):
                    self.send(200, open(os.path.join(HERE, "index.html"), "rb").read(), "text/html; charset=utf-8")
                elif u.path == "/api/info":
                    self.send(200, json.dumps(pkg.info()).encode(), "application/json")
                elif u.path == "/api/overview":
                    self.send(200, pkg.overview(int(q.get("step", ["4"])[0])), "application/octet-stream")
                elif u.path == "/api/tile":
                    lv = int(q["level"][0]) if "level" in q else None
                    self.send(200, pkg.tile(int(q["x"][0]), int(q["y"][0]), lv), "application/octet-stream")
                elif u.path.startswith("/api/model/"):
                    self.send(200, pkg.model(int(u.path.rsplit("/", 1)[1], 16)), "application/octet-stream")
                elif u.path.startswith("/api/tex/"):
                    self.send(200, tex.png(int(u.path.rsplit("/", 1)[1], 16)), "image/png")
                else:
                    self.send(404, b"not found", "text/plain")
            except KeyError as ex:
                self.send(404, str(ex).encode(), "text/plain")
            except (ValueError, struct.error, zlib.error, IndexError) as ex:
                self.send(422, f"{type(ex).__name__}: {ex}".encode(), "text/plain")
    return H


if __name__ == "__main__":
    pkg = Package(sys.argv[1], sys.argv[2])
    tex = Textures(sys.argv[3])
    pkg.fmt_of = lambda key: (tex.idx.get(str(key)) or [None, None, None])[2]
    port = int(sys.argv[4]) if len(sys.argv) > 4 else 8765
    print(f"{len(pkg.ents)} placements ({int(pkg.resolved.sum())} with a model in the package), "
          f"{len(pkg.model_file)} models, {len(tex.idx)} textures; http://127.0.0.1:{port}/", flush=True)
    ThreadingHTTPServer(("127.0.0.1", port), handler(pkg, tex)).serve_forever()
