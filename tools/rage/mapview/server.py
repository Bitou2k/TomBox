"""Map viewer for the second title's level package, served on 127.0.0.1 only.

Nothing is extracted: the server loads the mount index built by `rpf-index` (files.tsv,
entities.bin) and, when the browser asks for a model, reads that file's streams from the
original levels.rpf by offset, inflates them in memory and decodes the meshlet geometry
(meshnp.py). The page (index.html) places models by the placement records.

What is shown and what is not (see docs/TECHNICAL_DEBT.md, "Second title asset tools"):
  * positions, rotations (inverse quaternion convention) and scales come from the records;
  * a model is its named resource (name.#dr or name.#ft) plus the file right after it (GPU
    block), paired as tools/rage/modelexport.py does; parts that do not pair are skipped;
  * placements whose model name is not carried by any file in levels.rpf are drawn as points;
  * files whose placements all lie near the origin (presumably interiors in local space)
    are left out: their placement in the world is not decoded;
  * no textures: which texture goes on which part is not decoded;
  * no level-of-detail selection: the LOD hierarchy is not decoded, so near and far versions
    of the same object can overlap.

usage: server.py <levels.rpf> <index dir> [port]
"""
import sys, os, json, struct, zlib, threading, collections, math
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler
from urllib.parse import urlparse, parse_qs
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
from modelexport import meshes, records  # noqa: E402
import meshnp  # noqa: E402

ENT = np.dtype([("file", "<u4"), ("at", "<u4"), ("id", "<u8"), ("q", "<f4", 4), ("p", "<f4", 3),
                ("name", "<u4"), ("flags", "<u4"), ("s", "<f4", 2), ("idx", "<u2"), ("par", "<u2"),
                ("lod", "<f4"), ("pad", "<u4")])
TILE = 250.0  # metres per tile side


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
        self.in_dictionary = set()  # hashes named only inside files of several models
        for line in open(os.path.join(index_dir, "files.tsv"), encoding="utf-8"):
            r = line.rstrip("\n").split("\t")
            if r[0] == "index":
                continue
            self.files.append((int(r[1], 16), int(r[2], 16), int(r[4])))
            ns = [n for n in (r[7].split(";") if len(r) > 7 and r[7] else []) if n.endswith(".#dr") or n.endswith(".#ft")]
            if len(ns) == 1:
                # A file carrying one name is that model (97% of named files in a 4 GiB sample).
                self.model_file.setdefault(joaat(ns[0][:-4]), (len(self.files) - 1, ns[0]))
            else:
                # Several low-detail models in one file: which parts belong to which name is not
                # decoded, so these are not drawn.
                self.in_dictionary.update(joaat(n[:-4]) for n in ns)
        self.in_dictionary -= set(self.model_file)
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
        tx = np.floor(e["p"][:, 0] / TILE).astype(np.int64)
        ty = np.floor(e["p"][:, 1] / TILE).astype(np.int64)
        self.tiles = collections.defaultdict(list)
        for i, k in enumerate(zip(tx.tolist(), ty.tolist())):
            self.tiles[k].append(i)
        self.cache = collections.OrderedDict()

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
        if h in self.cache:
            self.cache.move_to_end(h)
            return self.cache[h]
        if h not in self.model_file:
            raise KeyError(f"no file in the package carries a model named with hash {h:#010x}")
        fi, name = self.model_file[h]
        b, g = self.read_file(fi), self.read_file(fi + 1)
        _, ms = meshes(b, g, name)
        mls = [p for p, _ in records(b) if p + 12 <= len(g) and struct.unpack_from("<I", g, p)[0] & 0x80000000]
        parts, blobs, errors = [], [], []
        for k, r in ms:
            if isinstance(r, str):
                errors.append(f"part {k}: {r}")
                continue
            try:
                p, n, uv, t = meshnp.mesh(g, r[0], mls[k])
            except ValueError as ex:
                errors.append(f"part {k}: {ex}")
                continue
            parts.append(dict(nv=len(p), nt=len(t), normals=n is not None))
            blobs += [p.astype("<f4").tobytes()]
            if n is not None:
                blobs.append(n.astype("<f4").tobytes())
            blobs.append(t.astype("<u4").tobytes())
        head = json.dumps(dict(name=name, parts=parts, errors=errors)).encode()
        head += b" " * (-len(head) % 4)
        out = struct.pack("<I", len(head)) + head + b"".join(blobs)
        self.cache[h] = out
        if len(self.cache) > 2000:
            self.cache.popitem(last=False)
        return out

    def tile(self, tx, ty):
        idx = self.tiles.get((tx, ty), [])
        e = self.ents[idx]
        res = self.resolved[idx]
        a = np.zeros((len(idx), 12), "<f4")
        a[:, 0:3] = e["p"]
        a[:, 3:7] = e["q"]
        a[:, 7:9] = e["s"]
        a[:, 9] = e["lod"]
        hdr = struct.pack("<I", len(idx))
        return hdr + a.tobytes() + e["name"].astype("<u4").tobytes() + res.astype(np.uint8).tobytes()

    def overview(self, step):
        p = self.ents["p"][::step]
        return struct.pack("<I", len(p)) + p.astype("<f4").tobytes()

    def info(self):
        p = self.ents["p"]
        in_dict = int(np.isin(self.ents["name"], np.array(sorted(self.in_dictionary), np.uint32)).sum())
        return dict(entities=int(len(self.ents)), resolved=int(self.resolved.sum()), models=len(self.model_file),
                    in_dictionary=in_dict, local_left_out=self.local_count,
                    tile=TILE, min=p.min(0).tolist(), max=p.max(0).tolist(),
                    tiles=[[x, y, len(v)] for (x, y), v in self.tiles.items()])


def handler(pkg):
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
                    self.send(200, pkg.tile(int(q["x"][0]), int(q["y"][0])), "application/octet-stream")
                elif u.path.startswith("/api/model/"):
                    self.send(200, pkg.model(int(u.path.rsplit("/", 1)[1], 16)), "application/octet-stream")
                else:
                    self.send(404, b"not found", "text/plain")
            except KeyError as ex:
                self.send(404, str(ex).encode(), "text/plain")
            except (ValueError, struct.error, zlib.error, IndexError) as ex:
                self.send(422, f"{type(ex).__name__}: {ex}".encode(), "text/plain")
    return H


if __name__ == "__main__":
    pkg = Package(sys.argv[1], sys.argv[2])
    port = int(sys.argv[3]) if len(sys.argv) > 3 else 8765
    print(f"{len(pkg.ents)} placements ({int(pkg.resolved.sum())} with a model in the package), "
          f"{len(pkg.model_file)} models; http://127.0.0.1:{port}/", flush=True)
    ThreadingHTTPServer(("127.0.0.1", port), handler(pkg)).serve_forever()
