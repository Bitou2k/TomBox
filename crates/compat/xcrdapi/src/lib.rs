//! `XCRDAPI.dll` compatibility shim.
//!
//! Exports `DStorageGetFactory` — the Xbox DirectStorage entry point used for
//! fast asset streaming from the `.ucas`/`.utoc` IoStore containers. The PC has a
//! real DirectStorage (`dstorage.dll`) that also exports `DStorageGetFactory`, so
//! this is a strong candidate to *forward* to the host DirectStorage later. For
//! now it is a logging stub returning E_NOTIMPL (fail-loud) so we can see when and
//! how the game requests the factory during startup.

use core::ffi::c_void;
use compat_common::{record, E_NOTIMPL};

const M: &str = "xcrdapi";

#[no_mangle]
pub extern "system" fn DStorageGetFactory(_riid: *const c_void, _ppv: *mut *mut c_void) -> i32 {
    record(M, "DStorageGetFactory", "[STUB -> E_NOTIMPL] (candidate: forward to host dstorage.dll)");
    E_NOTIMPL
}
