import ctypes, struct, sys
from ctypes import c_void_p, c_uint32, c_uint64, POINTER, byref, c_int32
import os
# The console texture-layout library shipped with a title (runs natively on x64 Windows).
XG = ctypes.WinDLL(os.environ.get("TOMBOX_XG_DLL", os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "input", "extracted", "Mount", "FortniteGame", "Binaries", "XSX", "xg_xs.dll")))
create = XG.XGCreateTextureComputer; create.restype = c_int32; create.argtypes = [c_void_p, POINTER(c_void_p)]
def computer(w, h, mips, fmt, layout, arr=1):
    d = bytearray(0x38)
    struct.pack_into("<I", d, 0, 3); struct.pack_into("<Q", d, 0x10, w); struct.pack_into("<I", d, 0x18, h)
    struct.pack_into("<HH", d, 0x1c, arr, mips); struct.pack_into("<II", d, 0x20, fmt, 1); struct.pack_into("<I", d, 0x2c, layout)
    buf = (ctypes.c_ubyte * 0x38).from_buffer(d); comp = c_void_p()
    hr = create(ctypes.addressof(buf), byref(comp))
    if hr < 0 or not comp.value: return None
    vt = ctypes.cast(ctypes.cast(comp, POINTER(c_void_p))[0], POINTER(c_void_p))
    lay = (ctypes.c_ubyte * 0x4000)()
    get = ctypes.WINFUNCTYPE(c_int32, c_void_p, c_void_p)(vt[2])
    if get(comp, lay) < 0: return None
    total, align, nm, planes = struct.unpack_from("<QQII", bytes(lay[:24]))
    bpe = struct.unpack_from("<I", bytes(lay[0x18+0x28:0x18+0x2c]))[0]
    off = ctypes.WINFUNCTYPE(c_uint64, c_void_p, c_uint32, c_uint32, c_uint64, c_uint32, c_uint32, c_uint32, c_void_p)(vt[6])
    return dict(comp=comp, total=total, bpe=bpe, off=lambda m, x, y, s=0: off(comp, 0, m, x, y, s, 0, None))
