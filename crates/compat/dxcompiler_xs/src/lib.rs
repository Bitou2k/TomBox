//! `dxcompiler_xs.dll` compatibility shim — the Xbox DXC (DirectX Shader
//! Compiler) factory. `DxcCreateInstance` is the COM-style factory used for
//! runtime HLSL->DXIL compilation. Logging stub for now (E_NOTIMPL): the PC ships
//! a real `dxcompiler.dll` with the same `DxcCreateInstance`, so forwarding to
//! the host DXC is the obvious next step once we confirm when the game calls it.

use core::ffi::c_void;
use compat_common::{record, E_NOTIMPL};

const M: &str = "dxcompiler_xs";

#[no_mangle]
pub extern "system" fn DxcCreateInstance(
    _rclsid: *const c_void,
    _riid: *const c_void,
    _ppv: *mut *mut c_void,
) -> i32 {
    record(M, "DxcCreateInstance", "[STUB -> E_NOTIMPL] (candidate: forward to host dxcompiler.dll)");
    E_NOTIMPL
}
