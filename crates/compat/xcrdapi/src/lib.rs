//! `XCRDAPI.dll` compatibility shim — Xbox DirectStorage entry `DStorageGetFactory`.
//!
//! The Xbox IoStore backend (`FXSXFileIoStoreImpl`) calls
//! `DStorageGetFactory(IID_IDStorageFactory_Xbox, &factory)` and dereferences the
//! result. We forward to the real PC DirectStorage (`dstorage.dll`, staged beside
//! the EXE). The Xbox factory IID `{fb3b54ac-bf82-463a-b777-73195837864d}` differs
//! from the PC IID `{6924ea0c-c3cd-4826-b10a-f64f4ed927c1}`, so we substitute the
//! PC IID when asking the real DirectStorage, then hand the PC factory back. This
//! works iff the two `IDStorageFactory` vtables are layout-compatible — if a later
//! factory method mismatches, the crash site will show which method and we build a
//! proper translating shim for it.

use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Once;
use compat_common::{record, E_NOTIMPL};

const M: &str = "xcrdapi";

type HMODULE = *mut c_void;
extern "system" {
    fn LoadLibraryW(p: *const u16) -> HMODULE;
    fn GetProcAddress(h: HMODULE, n: *const u8) -> *mut c_void;
    fn VirtualProtect(addr: *mut c_void, size: usize, new_protect: u32, old_protect: *mut u32) -> i32;
}
const PAGE_READWRITE: u32 = 0x04;

/// PC `IID_IDStorageFactory` = {6924ea0c-c3cd-4826-b10a-f64f4ed927c1}, LE layout.
static PC_IID_IDSTORAGEFACTORY: [u8; 16] = [
    0x0c, 0xea, 0x24, 0x69, // Data1 = 0x6924EA0C
    0xcd, 0xc3, // Data2 = 0xC3CD
    0x26, 0x48, // Data3 = 0x4826
    0xb1, 0x0a, 0xf6, 0x4f, 0x4e, 0xd9, 0x27, 0xc1, // Data4
];

/// PC `IID_IDStorageFile` = {5de95e7b-955a-4868-a73c-243b29f4b8da}, LE layout.
static PC_IID_IDSTORAGEFILE: [u8; 16] = [
    0x7b, 0x5e, 0xe9, 0x5d, // Data1 = 0x5DE95E7B
    0x5a, 0x95, // Data2 = 0x955A
    0x68, 0x48, // Data3 = 0x4868
    0xa7, 0x3c, 0x24, 0x3b, 0x29, 0xf4, 0xb8, 0xda, // Data4
];

static ORIG_OPENFILE: AtomicUsize = AtomicUsize::new(0);

/// Factory slot 4 = OpenFile(this, PCWSTR path, REFIID, void** ppv). The Xbox
/// IoStore passes the Xbox IDStorageFile IID, which PC OpenFile rejects; swap it
/// for the PC IID so the real DirectStorage hands back an IDStorageFile.
unsafe extern "system" fn open_file_shim(
    this: *mut c_void, path: *const u16, _riid: *const c_void, ppv: *mut *mut c_void,
) -> i32 {
    let orig: unsafe extern "system" fn(*mut c_void, *const u16, *const c_void, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(ORIG_OPENFILE.load(Ordering::Relaxed)) };
    let hr = unsafe { orig(this, path, PC_IID_IDSTORAGEFILE.as_ptr() as *const c_void, ppv) };
    let out = if ppv.is_null() { core::ptr::null_mut() } else { unsafe { *ppv } };
    record(M, "OpenFile", &format!("(PC file IID) -> hr={hr:#010x} file={out:p}"));
    hr
}

/// Patch the real factory's vtable slot 4 (OpenFile) once.
fn patch_factory(factory: *mut c_void) {
    if factory.is_null() || ORIG_OPENFILE.load(Ordering::Relaxed) != 0 {
        return;
    }
    unsafe {
        let vtbl = *(factory as *const *mut usize);
        let slot = vtbl.add(4);
        ORIG_OPENFILE.store(*slot, Ordering::Relaxed);
        let mut old: u32 = 0;
        if VirtualProtect(slot as *mut c_void, 8, PAGE_READWRITE, &mut old) != 0 {
            *slot = open_file_shim as usize;
            let mut tmp: u32 = 0;
            VirtualProtect(slot as *mut c_void, 8, old, &mut tmp);
            record(M, "patch_factory", &format!("vtable {vtbl:p} slot 4 OpenFile -> IID-substituting shim"));
        }
    }
}

static REAL: AtomicUsize = AtomicUsize::new(0);
static INIT: Once = Once::new();

fn real_get_factory() -> usize {
    INIT.call_once(|| {
        let name: Vec<u16> = "dstorage.dll\0".encode_utf16().collect();
        let h = unsafe { LoadLibraryW(name.as_ptr()) };
        let p = if h.is_null() {
            0
        } else {
            unsafe { GetProcAddress(h, b"DStorageGetFactory\0".as_ptr()) as usize }
        };
        record(M, "init", &format!("real dstorage.dll DStorageGetFactory = {p:#x} (0 = not found)"));
        REAL.store(p, Ordering::Relaxed);
    });
    REAL.load(Ordering::Relaxed)
}

#[no_mangle]
pub extern "system" fn DStorageGetFactory(_riid: *const c_void, ppv: *mut *mut c_void) -> i32 {
    let p = real_get_factory();
    if p == 0 {
        record(M, "DStorageGetFactory", "[real dstorage.dll not available -> E_NOTIMPL]");
        return E_NOTIMPL;
    }
    // Ask the real DirectStorage for a *PC* IDStorageFactory (ignore the Xbox riid).
    let f: extern "system" fn(*const c_void, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(p) };
    let hr = f(PC_IID_IDSTORAGEFACTORY.as_ptr() as *const c_void, ppv);
    let out = if ppv.is_null() { core::ptr::null_mut() } else { unsafe { *ppv } };
    if hr == 0 {
        patch_factory(out);
    }
    record(M, "DStorageGetFactory", &format!("forwarded (PC IID) -> hr={hr:#010x} factory={out:p}"));
    hr
}
