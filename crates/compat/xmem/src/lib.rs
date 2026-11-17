//! `xmem.dll` compatibility shim.
//!
//! The documented GDK `XMemAlloc`/`XMemFree`/`XMemVirtualAlloc` family is backed
//! here by the real Windows heap / `VirtualAlloc` — a genuine (if partial)
//! implementation, not a fake. The *physical page* APIs (GPU-visible unified
//! memory on console) have no direct desktop equivalent and are logged stubs for
//! now; they must be routed through the graphics backend's resource model later.
//!
//! x64 Windows uses one calling convention, and the callee does not pop args, so
//! declaring a shim with fewer parameters than the real export is ABI-safe.

use core::ffi::c_void;
use compat_common::record;

const M: &str = "xmem";

extern "system" {
    fn GetProcessHeap() -> *mut c_void;
    fn HeapAlloc(heap: *mut c_void, flags: u32, bytes: usize) -> *mut c_void;
    fn HeapFree(heap: *mut c_void, flags: u32, mem: *mut c_void) -> i32;
    fn VirtualAlloc(addr: *mut c_void, size: usize, alloc_type: u32, protect: u32) -> *mut c_void;
}

// ---- Heap-backed (real) implementations ----

#[no_mangle]
pub extern "system" fn XMemAlloc(size: usize, attributes: u64) -> *mut c_void {
    let p = unsafe { HeapAlloc(GetProcessHeap(), 0, size.max(1)) };
    record(M, "XMemAlloc", &format!("size={size} attrs={attributes:#x} -> {p:p}"));
    p
}

#[no_mangle]
pub extern "system" fn XMemAllocDefault(size: usize, attributes: u64) -> *mut c_void {
    let p = unsafe { HeapAlloc(GetProcessHeap(), 0, size.max(1)) };
    record(M, "XMemAllocDefault", &format!("size={size} attrs={attributes:#x} -> {p:p}"));
    p
}

#[no_mangle]
pub extern "system" fn XMemFree(address: *mut c_void, attributes: u64) {
    record(M, "XMemFree", &format!("addr={address:p} attrs={attributes:#x}"));
    if !address.is_null() {
        unsafe { HeapFree(GetProcessHeap(), 0, address) };
    }
}

#[no_mangle]
pub extern "system" fn XMemFreeDefault(address: *mut c_void, attributes: u64) {
    record(M, "XMemFreeDefault", &format!("addr={address:p} attrs={attributes:#x}"));
    if !address.is_null() {
        unsafe { HeapFree(GetProcessHeap(), 0, address) };
    }
}

#[no_mangle]
pub extern "system" fn XMemVirtualAlloc(
    address: *mut c_void,
    size: usize,
    alloc_type: u32,
    protect: u32,
    attributes: u64,
) -> *mut c_void {
    let p = unsafe { VirtualAlloc(address, size, alloc_type, protect) };
    record(
        M,
        "XMemVirtualAlloc",
        &format!("addr={address:p} size={size} type={alloc_type:#x} prot={protect:#x} attrs={attributes:#x} -> {p:p}"),
    );
    p
}

// ---- Physical-page / unified-memory APIs: logged stubs (need graphics backend) ----

#[no_mangle]
pub extern "system" fn XMemAllocatePhysicalPages(_flags: u32, size: usize, _attrs: u64) -> *mut c_void {
    record(M, "XMemAllocatePhysicalPages", &format!("size={size} [STUB -> null]"));
    core::ptr::null_mut()
}

#[no_mangle]
pub extern "system" fn XMemFreePhysicalPages(address: *mut c_void, _size: usize) -> i32 {
    record(M, "XMemFreePhysicalPages", &format!("addr={address:p} [STUB]"));
    0
}

#[no_mangle]
pub extern "system" fn XMemMapPhysicalPages(_a: *mut c_void, _b: usize, _c: u32, _d: *mut c_void) -> *mut c_void {
    record(M, "XMemMapPhysicalPages", "[STUB -> null]");
    core::ptr::null_mut()
}

#[no_mangle]
pub extern "system" fn XMemTransferMemory(_src: *mut c_void, _dst: *mut c_void, _size: usize) -> i32 {
    record(M, "XMemTransferMemory", "[STUB]");
    compat_common::E_NOTIMPL
}

#[no_mangle]
pub extern "system" fn XMemGetWorkingSetStatistics(_out: *mut c_void) -> i32 {
    record(M, "XMemGetWorkingSetStatistics", "[STUB]");
    compat_common::E_NOTIMPL
}
