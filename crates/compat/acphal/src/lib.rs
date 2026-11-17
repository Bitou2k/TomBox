//! `AcpHal.dll` compatibility shim — Xbox Audio Processor (ACP) HAL / APU memory.
//!
//! These drive the console's dedicated audio hardware (SHAPE/ACP) and its APU
//! memory heaps — there is no desktop equivalent. Audio is not needed to reach a
//! first frame, so every export is a fail-loud logging stub returning E_NOTIMPL.
//! `ApuAlloc`/`ApuFree` follow the GDK convention of returning an HRESULT and
//! yielding the block via an out-parameter, so returning failure without writing
//! the out-param is safe (the caller checks the HRESULT before dereferencing).

use core::ffi::c_void;
use compat_common::{record, E_NOTIMPL};

const M: &str = "acphal";

#[no_mangle]
pub extern "system" fn AcpHalCreate(_params: *const c_void, _out: *mut *mut c_void) -> i32 {
    record(M, "AcpHalCreate", "[STUB -> E_NOTIMPL]");
    E_NOTIMPL
}

#[no_mangle]
pub extern "system" fn AcpHalAllocateShapeContexts(_a: *mut c_void, _b: u32, _c: *mut c_void) -> i32 {
    record(M, "AcpHalAllocateShapeContexts", "[STUB -> E_NOTIMPL]");
    E_NOTIMPL
}

#[no_mangle]
pub extern "system" fn ApuAlloc(_ppblock: *mut *mut c_void, _pphys: *mut u32, _size: u32, _align: u32) -> i32 {
    record(M, "ApuAlloc", "[STUB -> E_NOTIMPL]");
    E_NOTIMPL
}

#[no_mangle]
pub extern "system" fn ApuFree(_block: *mut c_void) -> i32 {
    record(M, "ApuFree", "[STUB -> E_NOTIMPL]");
    E_NOTIMPL
}

#[no_mangle]
pub extern "system" fn ApuCreateHeap(_size: u32, _flags: u32, _out: *mut *mut c_void) -> i32 {
    record(M, "ApuCreateHeap", "[STUB -> E_NOTIMPL]");
    E_NOTIMPL
}

#[no_mangle]
pub extern "system" fn ApuHeapGetState(_heap: *mut c_void, _out: *mut c_void) -> i32 {
    record(M, "ApuHeapGetState", "[STUB -> E_NOTIMPL]");
    E_NOTIMPL
}
