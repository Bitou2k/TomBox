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
    fn GetLastError() -> u32;
}

// Windows VirtualAlloc allocation-type bits. The Xbox `XMemVirtualAlloc`
// allocation-type parameter is NOT bit-compatible with Windows: it carries
// console-specific bits (e.g. 0x20000000) that Windows would misread as
// MEM_LARGE_PAGES (privileged) and reject. We pass through only the bits Windows
// actually defines and drop the rest.
const MEM_COMMIT: u32 = 0x0000_1000;
const MEM_RESERVE: u32 = 0x0000_2000;
const MEM_RESET: u32 = 0x0008_0000;
const MEM_TOP_DOWN: u32 = 0x0010_0000;
const MEM_WRITE_WATCH: u32 = 0x0020_0000;
const MEM_RESET_UNDO: u32 = 0x0100_0000;
const WIN_ALLOC_TYPE_MASK: u32 =
    MEM_COMMIT | MEM_RESERVE | MEM_RESET | MEM_TOP_DOWN | MEM_WRITE_WATCH | MEM_RESET_UNDO;
const PAGE_READWRITE: u32 = 0x04;

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
    // Keep only Windows-defined allocation-type bits; drop Xbox-only bits.
    let mut win_type = alloc_type & WIN_ALLOC_TYPE_MASK;
    if win_type == 0 {
        // No recognizable intent: default to a reserve+commit.
        win_type = MEM_RESERVE | MEM_COMMIT;
    }
    // A zero protection is invalid for Windows; default to RW. (On Xbox, 0 can
    // mean "default".) Non-zero protections (NOACCESS=1, RW=4, etc.) match.
    let win_protect = if protect == 0 { PAGE_READWRITE } else { protect };

    let p = unsafe { VirtualAlloc(address, size, win_type, win_protect) };
    let err = if p.is_null() { unsafe { GetLastError() } } else { 0 };
    record(
        M,
        "XMemVirtualAlloc",
        &format!(
            "addr={address:p} size={size} xbox_type={alloc_type:#x}->win_type={win_type:#x} prot={protect:#x}->{win_protect:#x} attrs={attributes:#x} -> {p:p}{}",
            if p.is_null() { format!(" (GetLastError={err})") } else { String::new() }
        ),
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
