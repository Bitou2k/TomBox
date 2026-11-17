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

// High-slot diagnostic: the +4 shift is only valid through PC slot 46 (Xbox 50).
// Past that the Xbox command list has its own extension methods, so our naive map
// sends calls to the wrong PC method (e.g. a texture-copy landed on DispatchMesh ->
// device removal). Install logging no-op probes across the high range to capture,
// for every high slot the game actually calls, the slot + first arg + caller RVA,
// so we can reconstruct the true mapping. No-op avoids recording invalid GPU work.
static EXE_BASE: AtomicUsize = AtomicUsize::new(0);
static PROBE_HITS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn probe_impl(_this: *mut c_void, a: u64, caller: usize, slot: u64) {
    let h = PROBE_HITS.fetch_add(1, Ordering::Relaxed);
    if h < 120 {
        let base = EXE_BASE.load(Ordering::Relaxed);
        record("d3d12_xs", "cmdlist.hi",
            &format!("slot={slot} arg1={a:#x} caller={:#x}", caller.wrapping_sub(base)));
    }
}

macro_rules! make_probes {
    ($(($name:ident, $slot:literal)),* $(,)?) => {
        $(
            #[unsafe(naked)]
            unsafe extern "system" fn $name() {
                core::arch::naked_asm!(
                    "mov r8, [rsp]",        // caller return address -> 3rd arg
                    "mov r9d, {s}",         // slot -> 4th arg
                    "jmp {i}",
                    s = const $slot, i = sym probe_impl,
                );
            }
        )*
        #[allow(dead_code)]
        unsafe fn install_all_probes() {
            $( XBOX_VT[$slot].store($name as usize, Ordering::Relaxed); )*
        }
        /// The high-slot scan showed the game only calls slot 83 (an Xbox texture-copy
        /// command our +4 map misrouted to DispatchMesh). No-op just that slot for now.
        unsafe fn install_probes() {
            XBOX_VT[83].store(p83 as usize, Ordering::Relaxed);
        }
    };
}
make_probes!(
    (p47,47),(p48,48),(p49,49),(p51,51),(p52,52),(p53,53),(p54,54),(p55,55),(p56,56),(p57,57),
    (p58,58),(p59,59),(p60,60),(p61,61),(p62,62),(p63,63),(p64,64),(p65,65),(p66,66),(p67,67),
    (p68,68),(p69,69),(p70,70),(p71,71),(p72,72),(p73,73),(p74,74),(p75,75),(p76,76),(p77,77),
    (p78,78),(p79,79),(p80,80),(p81,81),(p82,82),(p83,83),(p84,84),(p85,85),(p86,86),(p87,87),
    (p88,88),(p89,89),(p90,90),(p91,91),(p92,92),(p93,93),(p94,94),(p95,95),
);

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
            extern "system" { fn GetModuleHandleW(n: *const u16) -> *mut c_void; }
            EXE_BASE.store(GetModuleHandleW(core::ptr::null()) as usize, Ordering::Relaxed);
            install_probes(); // high-slot logging no-op probes (47..95, except 50)
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
