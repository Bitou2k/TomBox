//! Fixes the Xbox↔PC return-convention mismatch for the descriptor-handle getters
//! **without wrapping the heap object** (a wrapper would leak into real D3D12 calls
//! like `SetDescriptorHeaps`, crashing the driver).
//!
//! Instead we patch the real `ID3D12DescriptorHeap` vtable in place: slots 9/10
//! (`GetCPU/GPUDescriptorHandleForHeapStart`) are redirected to small bridges.
//! All descriptor heaps share one vtable, so patching once (on the first heap)
//! fixes them all, and the heap objects stay fully real.
//!
//! Xbox callers expect the 8-byte handle in RAX. PC `D3D12Core` implements these
//! as `fn(this, Handle* retval) -> Handle*` (writes *retval). Each bridge calls the
//! saved original with a local and returns the value in RAX.

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use compat_common::record;

extern "system" {
    fn VirtualProtect(addr: *mut c_void, size: usize, new_protect: u32, old_protect: *mut u32) -> i32;
}
const PAGE_READWRITE: u32 = 0x04;

static PATCHED: AtomicBool = AtomicBool::new(false);
static ORIG_CPU: AtomicUsize = AtomicUsize::new(0);
static ORIG_GPU: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn bridge_cpu(heap: *mut c_void) -> u64 {
    let orig: unsafe extern "system" fn(*mut c_void, *mut u64) -> *mut u64 =
        unsafe { core::mem::transmute(ORIG_CPU.load(Ordering::Relaxed)) };
    let mut out: u64 = 0;
    unsafe { orig(heap, &mut out) };
    out
}

unsafe extern "system" fn bridge_gpu(heap: *mut c_void) -> u64 {
    let orig: unsafe extern "system" fn(*mut c_void, *mut u64) -> *mut u64 =
        unsafe { core::mem::transmute(ORIG_GPU.load(Ordering::Relaxed)) };
    let mut out: u64 = 0;
    unsafe { orig(heap, &mut out) };
    out
}

/// Patch the shared descriptor-heap vtable (once), using a real heap instance.
pub fn ensure_patched(real_heap: *mut c_void) {
    if real_heap.is_null() || PATCHED.swap(true, Ordering::SeqCst) {
        return;
    }
    unsafe {
        let vtbl = *(real_heap as *const *mut usize); // mutable vtable array
        ORIG_CPU.store(*vtbl.add(9), Ordering::Relaxed);
        ORIG_GPU.store(*vtbl.add(10), Ordering::Relaxed);
        let addr = vtbl.add(9) as *mut c_void;
        let mut old: u32 = 0;
        if VirtualProtect(addr, 16, PAGE_READWRITE, &mut old) != 0 {
            *vtbl.add(9) = bridge_cpu as usize;
            *vtbl.add(10) = bridge_gpu as usize;
            let mut tmp: u32 = 0;
            VirtualProtect(addr, 16, old, &mut tmp);
            record("d3d12_xs", "heap.patch", &format!("descriptor-heap vtable {vtbl:p}: slots 9/10 bridged for Xbox RAX return"));
        } else {
            record("d3d12_xs", "heap.patch", "VirtualProtect failed");
        }
    }
}
