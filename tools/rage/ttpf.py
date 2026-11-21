"""Read the second title's texture packs (data/ttdb/**/*.ttpf). Not encrypted.

Layout, inferred from the files (fields cross-checked against inflated sizes, see
docs/DEVELOPMENT_LOG.md 2026-11-21):
  +0x00 'RDS5'
  +0x04 u32 chunk-table entries (the last ones may be zero)
  +0x08 u32 virtual size in bytes (header pages + texture pages)
  +0x0c u32 unknown (0x16000000 / 0x16000001)
  +0x10 u32 offset / 256 of the 'ETPF' block
  +0x14 u32 unknown
  +0x18 u64 chunk entries:
        bits 0-25   file offset / 256 of the chunk
        bits 26-29  codec: 8 = zlib; 0, 1, 3, 4, 5, 6, 7 = another, not yet identified codec
                    (the values track BC formats; possibly the console's BC-aware compressor)
        bits 30-51  virtual page (4 KiB) where the decoded chunk starts
        bits 52-63  unknown
  'ETPF' block: 'ETPF', u32 0x27a (version?), u32 count, u32 unknown, then 16-byte records:
        d0: bits 0-13 width-1, bits 14-27 height-1, bits 28-31 mip count
        d1: bits 0-2 unknown (0/1/3), bits 3-9 DXGI format, bits 10-15 array size - 1 (6 on cube
            maps); higher bits unknown
        d2: bits 0-19 first virtual page; higher bits unknown
        d3: unknown (non-zero on some mipmapped textures)
  then a sorted table of u64 name hashes, one per record (names themselves are not stored).
"""
import bisect, struct, zlib

ZLIB = 8


class CodecError(Exception):
    pass


class Pack:
    def __init__(self, path):
        self.path = path
        self.f = open(path, "rb")
        head = self.f.read(0x18)
        if head[:4] != b"RDS5":
            raise ValueError("not an RDS5 texture pack")
        n, self.vsize, _, etpf = struct.unpack_from("<IIII", head, 4)
        self.chunks = []  # (virtual offset, file offset, codec)
        for q in struct.unpack("<%dQ" % n, self.f.read(8 * n)):
            if q:
                self.chunks.append((((q >> 30) & 0x3FFFFF) << 12, (q & 0x3FFFFFF) << 8, (q >> 26) & 0xF))
        self.chunks.sort()
        self.starts = [c[0] for c in self.chunks]
        self.f.seek(etpf << 8)
        e = self.f.read(16)
        if e[:4] != b"ETPF":
            raise ValueError(f"no ETPF block at {etpf << 8:#x}")
        cnt = struct.unpack_from("<I", e, 8)[0]
        recs = self.f.read(16 * cnt)
        self.hashes = struct.unpack("<%dQ" % cnt, self.f.read(8 * cnt))
        self.textures = []
        for i in range(cnt):
            d0, d1, d2, d3 = struct.unpack_from("<IIII", recs, 16 * i)
            self.textures.append(dict(
                index=i, w=(d0 & 0x3FFF) + 1, h=((d0 >> 14) & 0x3FFF) + 1, mips=d0 >> 28,
                fmt=(d1 >> 3) & 0x7F, lo=d1 & 7, arr=((d1 >> 10) & 0x3F) + 1, d1=d1,
                page=d2 & 0xFFFFF, d2=d2, d3=d3))
        pages = sorted({t["page"] for t in self.textures}) + [self.vsize >> 12]
        nxt = {p: pages[i + 1] for i, p in enumerate(pages[:-1])}
        for t in self.textures:
            t["bytes"] = (nxt[t["page"]] - t["page"]) << 12
            t["codecs"] = sorted({self.chunks[i][2] for i in self._span(t["page"] << 12, t["bytes"])})
        self.cache = {}

    def _span(self, vaddr, size):
        i = bisect.bisect_right(self.starts, vaddr) - 1
        j = bisect.bisect_left(self.starts, vaddr + size)
        return range(max(i, 0), j)

    def _chunk(self, i):
        if i not in self.cache:
            if len(self.cache) > 64:
                self.cache.clear()
            v, off, codec = self.chunks[i]
            if codec != ZLIB:
                raise CodecError(f"chunk {i} uses codec {codec}, not zlib (not supported)")
            end = self.chunks[i + 1][0] if i + 1 < len(self.chunks) else self.vsize
            self.f.seek(off)
            d = zlib.decompressobj()
            out = bytearray()
            while len(out) < end - v:
                blk = self.f.read(1 << 20)
                if not blk:
                    break
                out += d.decompress(blk)
                if d.eof:
                    break
            if len(out) != end - v:
                raise ValueError(f"chunk {i} inflated to {len(out):#x} bytes, table says {end - v:#x}")
            self.cache[i] = (v, bytes(out))
        return self.cache[i]

    def read(self, vaddr, size):
        """Bytes [vaddr, vaddr+size) of the decoded virtual space."""
        out = bytearray()
        while size > 0:
            i = bisect.bisect_right(self.starts, vaddr) - 1
            if i < 0:
                raise ValueError(f"address {vaddr:#x} before the first chunk")
            v, data = self._chunk(i)
            part = data[vaddr - v:vaddr - v + size]
            if not part:
                raise ValueError(f"address {vaddr:#x} past chunk {i}")
            out += part
            vaddr += len(part)
            size -= len(part)
        return bytes(out)

    def texture_bytes(self, t):
        return self.read(t["page"] << 12, t["bytes"])
