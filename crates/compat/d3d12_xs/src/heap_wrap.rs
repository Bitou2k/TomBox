//! Wrapper for `ID3D12DescriptorHeap` that fixes the Xbox↔PC return-convention
//! mismatch for the descriptor-handle getters.
//!
//! On Xbox, `GetCPUDescriptorHandleForHeapStart` / `GetGPUDescriptorHandleForHeapStart`
//! return the 8-byte handle **in RAX**. On PC, `D3D12Core` returns it via a hidden
//! first pointer argument (sret: `fn(ret*, this)`). The Xbox game calls the vtable
//! slot expecting RAX, which makes PC `D3D12Core` write through a bogus pointer →
//! AV. We intercept slots 9/10 and bridge: call the real (sret) method into a local
//! and return the value in RAX. All other heap methods forward unchanged.

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use crate::device_wrap::Guid;
use compat_common::record;

const S_OK: i32 = 0;
const E_POINTER: i32 = 0x8000_4003u32 as i32;
const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;

const IID_IUNKNOWN: Guid = Guid { d1: 0, d2: 0, d3: 0, d4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };
// ID3D12DescriptorHeap
const IID_ID3D12DESCRIPTORHEAP: Guid = Guid {
    d1: 0x8efb_471d, d2: 0x616c, d3: 0x4f49, d4: [0x90, 0xf7, 0x12, 0x7b, 0xb7, 0x63, 0xfa, 0x51],
};

#[repr(C)]
struct HeapWrapper {
    vtable: *const usize,
    real: *mut c_void,
    refs: AtomicU32,
}

unsafe fn real_vtbl(real: *mut c_void) -> *const usize {
    unsafe { *(real as *const *const usize) }
}

unsafe extern "system" fn qi(this: *mut HeapWrapper, riid: *const Guid, ppv: *mut *mut c_void) -> i32 {
    if ppv.is_null() || riid.is_null() {
        return E_POINTER;
    }
    let g = unsafe { &*riid };
    if *g == IID_ID3D12DESCRIPTORHEAP || *g == IID_IUNKNOWN {
        unsafe {
            *ppv = this as *mut c_void;
            (*this).refs.fetch_add(1, Ordering::SeqCst);
        }
        return S_OK;
    }
    // Forward other IIDs (e.g. ID3D12Object, device-child) to the real heap.
    let real = unsafe { (*this).real };
    let f: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*real_vtbl(real)) };
    let hr = unsafe { f(real, riid, ppv) };
    if hr == S_OK {
        unsafe { *ppv = core::ptr::null_mut() };
        return E_NOINTERFACE; // don't hand out the unwrapped heap
    }
    hr
}

unsafe extern "system" fn addref(this: *mut HeapWrapper) -> u32 {
    unsafe { (*this).refs.fetch_add(1, Ordering::SeqCst) + 1 }
}

unsafe extern "system" fn release(this: *mut HeapWrapper) -> u32 {
    let n = unsafe { (*this).refs.fetch_sub(1, Ordering::SeqCst) } - 1;
    if n == 0 {
        unsafe {
            let real = (*this).real;
            let rel: unsafe extern "system" fn(*mut c_void) -> u32 =
                core::mem::transmute(*real_vtbl(real).add(2));
            rel(real);
            drop(Box::from_raw(this));
        }
    }
    n
}

/// slot 9: GetCPUDescriptorHandleForHeapStart.
/// Xbox: returns the 8-byte handle in RAX (what the game expects).
/// PC: `fn(this [rcx], Handle* retval [rdx]) -> Handle*` — writes *retval.
unsafe extern "system" fn get_cpu_handle(this: *mut HeapWrapper) -> u64 {
    let real = unsafe { (*this).real };
    let f: unsafe extern "system" fn(*mut c_void, *mut u64) -> *mut u64 =
        unsafe { core::mem::transmute(*real_vtbl(real).add(9)) };
    let mut out: u64 = 0;
    unsafe { f(real, &mut out) };
    out
}

/// slot 10: GetGPUDescriptorHandleForHeapStart (same ABI bridge as slot 9).
unsafe extern "system" fn get_gpu_handle(this: *mut HeapWrapper) -> u64 {
    let real = unsafe { (*this).real };
    let f: unsafe extern "system" fn(*mut c_void, *mut u64) -> *mut u64 =
        unsafe { core::mem::transmute(*real_vtbl(real).add(10)) };
    let mut out: u64 = 0;
    unsafe { f(real, &mut out) };
    out
}

// Forwarding thunks for slots 3..=8 (ID3D12Object / device-child / GetDesc).
macro_rules! fwd {
    ($name:ident, $slot:expr) => {
        #[unsafe(naked)]
        unsafe extern "system" fn $name() {
            core::arch::naked_asm!("mov rcx,[rcx+8]", "mov rax,[rcx]", "jmp qword ptr [rax + {o}]", o = const ($slot * 8usize));
        }
    };
}
fwd!(h3, 3); fwd!(h4, 4); fwd!(h5, 5); fwd!(h6, 6); fwd!(h7, 7); fwd!(h8, 8);

fn vtable() -> *const usize {
    static VT: OnceLock<usize> = OnceLock::new();
    *VT.get_or_init(|| {
        let v: Vec<usize> = vec![
            qi as usize, addref as usize, release as usize,
            h3 as usize, h4 as usize, h5 as usize, h6 as usize, h7 as usize, h8 as usize,
            get_cpu_handle as usize,  // 9
            get_gpu_handle as usize,  // 10
        ];
        Box::leak(v.into_boxed_slice()).as_ptr() as usize
    }) as *const usize
}

pub fn wrap(real: *mut c_void) -> *mut c_void {
    let vt = vtable();
    let w = Box::new(HeapWrapper { vtable: vt, real, refs: AtomicU32::new(1) });
    let p = Box::into_raw(w);
    record("d3d12_xs", "heap.wrap", &format!("real={real:p} wrapper={p:p} vtable={vt:p} cpu@slot9={:#x} gpu@slot10={:#x}",
        unsafe { *vt.add(9) }, unsafe { *vt.add(10) }));
    p as *mut c_void
}
