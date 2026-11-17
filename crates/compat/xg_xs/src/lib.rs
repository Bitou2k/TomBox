//! `xg_xs.dll` compatibility shim.
//!
//! The Xbox "XG" library computes texture/resource memory layout, tiling and
//! sizes for the GPU. `XGCreateTextureComputer` returns an object the RHI uses
//! to size/tile textures. This is a logging stub (returns E_NOTIMPL) until the
//! graphics backend defines the resource model — faking a computer would produce
//! silently wrong texture layouts, so we fail loud instead.

use core::ffi::c_void;
use compat_common::{record, E_NOTIMPL};

const M: &str = "xg_xs";

#[no_mangle]
pub extern "system" fn XGCreateTextureComputer(
    _desc: *const c_void,
    _ppcomputer: *mut *mut c_void,
) -> i32 {
    record(M, "XGCreateTextureComputer", "[STUB -> E_NOTIMPL] (no resource model yet)");
    E_NOTIMPL
}
