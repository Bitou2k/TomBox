import struct, sys, os, glob
def rd(b,o,f): return struct.unpack_from(f,b,o)
def cstr(b,o):
    e=b.find(b"\0",o); return b[o:e].decode("latin1") if e>o else ""
def parse(b):
    """Texture dictionary in a RAGE resource system block (base 0x50000000)."""
    V=lambda p: p-0x50000000 if 0x50000000<=p<0x50000000+len(b) else None
    ptrarr, cnt, cap = rd(b,0x30,"<QHH")
    if V(ptrarr) is None or cnt==0 or cnt>4096: return None
    out=[]
    for i in range(cnt):
        t=V(rd(b,V(ptrarr)+8*i,"<Q")[0])
        if t is None or t+0x48>len(b): return None
        phys=rd(b,t+0x18,"<Q")[0]; namep=V(rd(b,t+0x30,"<Q")[0])
        w,h=rd(b,t+0x38,"<HH"); d0,d1,arr,fmt=rd(b,t+0x3c,"<BBBB"); m0,m1,mips,fl=rd(b,t+0x40,"<BBBB")
        name=cstr(b,namep+8) if namep is not None else "?"
        out.append(dict(name=name,w=w,h=h,fmt=fmt,mips=mips,phys=phys,flags=fl,tile=rd(b,t+0x08,"<HHHH"),arr=arr,depth=d0))
    return out
if __name__=="__main__":
    n=0; fmts={}
    for f in sorted(glob.glob(sys.argv[1]+"/*.bin")):
        b=open(f,"rb").read()
        if len(b)>1<<20: continue
        r=parse(b)
        if not r: continue
        n+=1
        for t in r: fmts[t["fmt"]]=fmts.get(t["fmt"],0)+1
        if n<=3:
            print(os.path.basename(f), len(r))
            for t in r: print("   ", t)
    print("dictionaries", n, "formats", sorted(fmts.items(), key=lambda x:-x[1]))
