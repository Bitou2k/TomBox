//! Xbox D3D12.x `ID3D12GraphicsCommandList` vtable translation.
//!
//! Reverse-engineered from the Fortnite RHI call sites, the Xbox command-list
//! vtable differs from PC by **4 methods inserted after PC slot 27**
//! (ExecuteBundle). Confirmed slot mapping (Xbox slot -> PC method):
//!   * 0..=26  identical  (IUnknown, Close=9, Reset=10, DrawInstanced=12,
//!     DrawIndexedInstanced=13, Dispatch=14, SetPipelineState=25,
//!     ResourceBarrier=26 — all verified unshifted at their call sites)
//!   * 27..=31 ExecuteBundle (PC27) + 4 Xbox-specific inserted methods
//!   * 32..    PC method (slot - 4): SetDescriptorHeaps(PC28)=Xbox32,
//!     SetGraphicsRootSignature(PC30)=Xbox34, SetGraphicsRootDescriptorTable
//!     (PC32)=Xbox36, OMSetRenderTargets(PC46)=Xbox50 — all verified.
//!
//! Fix: build an "Xbox-layout" vtable whose slot i holds the *real PC method
//! pointer* for the mapped PC slot, then point each real command-list object at
//! it. Because the object IS the real list, no forwarding thunk is needed — the
//! real method is called with the real `this` directly. Slots 0..=26 stay
//! identical so the D3D12 runtime's own calls (QueryInterface, GetType, AddRef,
//! Release) still work when the list is handed to ExecuteCommandLists.

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[allow(unused_imports)] use core::sync::atomic::AtomicU64;
use compat_common::record;

extern "system" {
    fn VirtualProtect(addr: *mut c_void, size: usize, new_protect: u32, old_protect: *mut u32) -> i32;
}
const PAGE_READWRITE: u32 = 0x04;

/// Number of Xbox vtable slots we materialize. Max observed call is Xbox slot 50
/// (OMSetRenderTargets); 128 leaves generous headroom.
const N: usize = 128;
static XBOX_VT: [AtomicUsize; N] = [const { AtomicUsize::new(0) }; N];
static BUILT: AtomicBool = AtomicBool::new(false);
static INSERTED_HITS: AtomicUsize = AtomicUsize::new(0);

/// Xbox slot -> PC slot. `None` = an Xbox-inserted method with no PC equivalent.
fn map_slot(i: usize) -> Option<usize> {
    match i {
        0..=26 => Some(i),   // identical
        27..=31 => None,     // ExecuteBundle + 4 inserted Xbox methods
        _ => Some(i - 4),    // shifted PC methods
    }
}

/// Fail-loud-ish stub for an Xbox-inserted slot of unknown signature. Returns 0
/// in rax (valid for both void- and HRESULT(S_OK)-returning methods) so a stray
/// call doesn't corrupt state; logs the first few so we learn if the game needs one.
#[unsafe(naked)]
unsafe extern "system" fn inserted_stub() {
    core::arch::naked_asm!(
        "lea rax, [rip + {counter}]",
        "lock inc qword ptr [rax]",
        "xor eax, eax",
        "ret",
        counter = sym INSERTED_HITS,
    )
}

/// Point a freshly created real command list at the Xbox-layout vtable.
pub fn apply(real_list: *mut c_void) {
    if real_list.is_null() {
        return;
    }
    unsafe {
        let real_vt = *(real_list as *const *const usize);
        if !BUILT.swap(true, Ordering::SeqCst) {
            for i in 0..N {
                let p = match map_slot(i) {
                    Some(src) => *real_vt.add(src),
                    None => inserted_stub as usize,
                };
                XBOX_VT[i].store(p, Ordering::Relaxed);
            }
            record("d3d12_xs", "cmdlist.vtable",
                &format!("built Xbox-layout cmdlist vtable from real {real_vt:p} (+4 shift at PC28)"));
        }
        // Repoint the object's vtable pointer (first 8 bytes) at XBOX_VT.
        let slot0 = real_list as *mut usize;
        let mut old: u32 = 0;
        if VirtualProtect(slot0 as *mut c_void, 8, PAGE_READWRITE, &mut old) != 0 {
            *slot0 = XBOX_VT.as_ptr() as usize;
            let mut tmp: u32 = 0;
            VirtualProtect(slot0 as *mut c_void, 8, old, &mut tmp);
        }
    }
}

/// How many times an Xbox-inserted (unmapped) slot was called — a nonzero value
/// means the game needs one of those 4 methods implemented.
pub fn inserted_hits() -> usize {
    INSERTED_HITS.load(Ordering::Relaxed)
}
