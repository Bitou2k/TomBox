"""Turn the title's symbol database (analysis/fortnite_symbols.decompressed.bin: u32 count at
0x2c, 12-byte {rva, size, ?} records from 0x30, then a u32 count and length-prefixed names)
into a PDB that IDA / WinDbg / other tools can load, via LLVM's `llvm-pdbutil yaml2pdb`.

The PDB carries the executable's CodeView identity (GUID + age from its RSDS record), its
section headers, and one S_PUB32 public symbol per name (flagged as a function when it lies
in an executable section). No types, no line numbers - names and addresses only.

usage: python syms2pdb.py <symbols.bin> <title.exe> <out.pdb> [llvm-pdbutil path]
"""
import bisect, os, struct, subprocess, sys, tempfile, uuid


def load_symbols(path):
    b = open(path, "rb").read()
    u32 = lambda o: struct.unpack_from("<I", b, o)[0]
    cnt = u32(0x2C)
    syms = [[u32(0x30 + i * 12), u32(0x34 + i * 12), ""] for i in range(cnt)]
    so = 0x30 + cnt * 12 + 4
    for s in syms:
        n = u32(so)
        so += 4
        if n == 0:
            break
        s[2] = b[so:so + n - 1].decode("utf-8", "replace")
        so += n
    return syms


def pe_info(path):
    f = open(path, "rb")
    d = f.read(0x1000)
    pe = struct.unpack_from("<I", d, 0x3C)[0]
    n = struct.unpack_from("<H", d, pe + 6)[0]
    opt = struct.unpack_from("<H", d, pe + 20)[0]
    secs = []
    for i in range(n):
        o = pe + 24 + opt + 40 * i
        vs, va, rs, ra, pr, pl, nr, nl, ch = struct.unpack_from("<IIIIIIHHI", d, o + 8)
        secs.append(dict(name=d[o:o + 8].rstrip(b"\0").decode(), vs=vs, va=va, rs=rs, ra=ra, pr=pr, pl=pl, nr=nr, nl=nl, ch=ch))

    def read(rva, size):
        for s in secs:
            if s["va"] <= rva < s["va"] + s["vs"]:
                f.seek(s["ra"] + rva - s["va"])
                return f.read(size)
        return b""

    # Debug directory (data directory 6) -> CodeView RSDS record.
    magic = struct.unpack_from("<H", d, pe + 24)[0]
    dd = pe + 24 + (112 if magic == 0x20B else 96) + 6 * 8
    drva, dsize = struct.unpack_from("<II", d, dd)
    dbg = read(drva, dsize)
    for k in range(0, len(dbg), 28):
        ty, sz, addr = struct.unpack_from("<I", dbg, k + 12)[0], struct.unpack_from("<I", dbg, k + 16)[0], struct.unpack_from("<I", dbg, k + 20)[0]
        if ty == 2:
            r = read(addr, sz)
            if r[:4] == b"RSDS":
                return secs, uuid.UUID(bytes_le=r[4:20]), struct.unpack_from("<I", r, 20)[0]
    raise SystemExit("no CodeView RSDS record in the executable")


def main():
    syms_path, exe, out = sys.argv[1:4]
    tool = sys.argv[4] if len(sys.argv) > 4 else r"C:\Program Files\LLVM\bin\llvm-pdbutil.exe"
    syms = load_symbols(syms_path)
    secs, guid, age = pe_info(exe)
    starts = [s["va"] for s in secs]
    q = lambda s: "'" + s.replace("'", "''") + "'"
    yml = os.path.join(tempfile.mkdtemp(), "syms.yaml")
    kept = 0
    with open(yml, "w", encoding="utf-8", newline="\n") as o:
        w = o.write
        w(f"---\nPdbStream:\n  Age: {age}\n  Guid: '{{{str(guid).upper()}}}'\n  Signature: 0\n  Version: VC70\n")
        w(f"DbiStream:\n  VerHeader: V70\n  Age: {age}\n  MachineType: Amd64\n  SectionHeaders:\n")
        for s in secs:
            w(f"    - Name: {q(s['name'])}\n      VirtualSize: {s['vs']}\n      VirtualAddress: {s['va']}\n"
              f"      SizeOfRawData: {s['rs']}\n      PointerToRawData: {s['ra']}\n      PointerToRelocations: {s['pr']}\n"
              f"      PointerToLinenumbers: {s['pl']}\n      NumberOfRelocations: {s['nr']}\n      NumberOfLinenumbers: {s['nl']}\n"
              f"      Characteristics: {s['ch']}\n")
        w("TpiStream:\n  Version: VC80\n  Records: []\nIpiStream:\n  Version: VC80\n  Records: []\nPublicsStream:\n  Records:\n")
        for rva, _size, name in syms:
            i = bisect.bisect_right(starts, rva) - 1
            if i < 0 or not name or rva >= secs[i]["va"] + max(secs[i]["vs"], secs[i]["rs"]):
                continue
            name = "".join(c for c in name if c >= " " and c != "\x7f")
            fn = "Function" if secs[i]["ch"] & 0x20000000 else ""
            w(f"    - Kind: S_PUB32\n      PublicSym32:\n        Flags: [ {fn} ]\n        Offset: {rva - secs[i]['va']}\n"
              f"        Segment: {i + 1}\n        Name: {q(name)}\n")
            kept += 1
        w("...\n")
    subprocess.run([tool, "yaml2pdb", f"--pdb={out}", yml], check=True)
    os.remove(yml)
    print(f"{out}: {kept} of {len(syms)} symbols, GUID {{{str(guid).upper()}}} age {age}")


if __name__ == "__main__":
    main()
