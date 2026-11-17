import struct, sys
data = open(sys.argv[1],'rb').read()
assert data[:4]==b'DXBC', data[:4]
digest = data[4:20]
ver, total, nchunks = struct.unpack('<III', data[20:32])
print(f"container: digest={digest.hex()} ver={ver} total={total} nchunks={nchunks} filelen={len(data)}")
print(f"digest nonzero={any(digest)}")
offs = struct.unpack('<%dI'%nchunks, data[32:32+4*nchunks])
for i,o in enumerate(offs):
    fourcc = data[o:o+4]; (csize,) = struct.unpack('<I', data[o+4:o+8])
    print(f"  chunk[{i}] @ {o:#x} {fourcc} size={csize}")
    if fourcc==b'DXIL' or fourcc==b'SHEX' or fourcc==b'SHDR':
        body = data[o+8:o+8+csize]
        # DXIL program header: Version(4), Size(4) then... actually: 
        # ProgramVersion(4: minor:4 major:4 in low byte, type in high), Size(4=dwords), then 'DXIL'(4), DxilVersion(4), BitcodeOffset(4), BitcodeSize(4)
        pv, psize = struct.unpack('<II', body[:8])
        ptype = (pv>>16)&0xffff; major=(pv>>4)&0xf; minor=pv&0xf
        print(f"    programVersion raw={pv:#x} type={ptype} SM={major}.{minor} progSizeDwords={psize}")
        dxilmagic = body[8:12]
        dxver, bcoff, bcsize = struct.unpack('<III', body[12:24])
        print(f"    dxilMagic={dxilmagic} dxilVer={dxver:#x} bitcodeOff={bcoff} bitcodeSize={bcsize}")
        bc = body[8+bcoff:8+bcoff+8]
        print(f"    bitcode first8={bc.hex()} (LLVM 'BC\xc0\xde'=42 43 c0 de)")
