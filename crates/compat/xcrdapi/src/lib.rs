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
}

/// PC `IID_IDStorageFactory` = {6924ea0c-c3cd-4826-b10a-f64f4ed927c1}, LE layout.
static PC_IID_IDSTORAGEFACTORY: [u8; 16] = [
    0x0c, 0xea, 0x24, 0x69, // Data1 = 0x6924EA0C
    0xcd, 0xc3, // Data2 = 0xC3CD
    0x26, 0x48, // Data3 = 0x4826
    0xb1, 0x0a, 0xf6, 0x4f, 0x4e, 0xd9, 0x27, 0xc1, // Data4
];

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
    record(M, "DStorageGetFactory", &format!("forwarded (PC IID) -> hr={hr:#010x} factory={out:p}"));
    hr
}
