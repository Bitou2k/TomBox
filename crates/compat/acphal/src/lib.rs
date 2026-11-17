//! `AcpHal.dll` compatibility — Xbox Audio Processor (ACP/APU / SHAPE) memory.
//!
//! `FXboxCommonGDKPlatformMemory::CreateApuHeap` fatally errors if `ApuCreateHeap`
//! fails, so these must succeed. We back the "APU heap" with a single host
//! `VirtualAlloc` region and bump-allocate from it (signatures recovered from the
//! call sites in the EXE):
//!   HRESULT ApuCreateHeap(uint32 sizeBytes, uint32 flags)
//!   HRESULT ApuAlloc(void** ppBlock, uint64* pPhysAddr, uint32 size, uint32 align)
//!   HRESULT ApuHeapGetState(struct{u32 free; u32 used; ...}* out, uint32 idx)
//! Audio DSP (SHAPE) is not emulated; this only satisfies the memory contract so
//! init proceeds. Physical addresses are faked as the virtual address.

use core::ffi::c_void;
use std::sync::Mutex;
use compat_common::{record, S_OK};

const M: &str = "acphal";

extern "system" {
    fn VirtualAlloc(addr: *mut c_void, size: usize, typ: u32, protect: u32) -> *mut c_void;
}
const MEM_COMMIT_RESERVE: u32 = 0x1000 | 0x2000;
const PAGE_READWRITE: u32 = 0x04;

struct Heap {
    base: usize,
    size: usize,
    offset: usize,
}

static HEAP: Mutex<Option<Heap>> = Mutex::new(None);

#[no_mangle]
pub extern "system" fn ApuCreateHeap(size_bytes: u32, flags: u32) -> i32 {
    let mut g = HEAP.lock().unwrap();
    if g.is_some() {
        record(M, "ApuCreateHeap", "already created -> S_OK");
        return S_OK;
    }
    // Round up and reserve+commit the APU heap from host memory.
    let size = (size_bytes as usize).max(0x1000);
    let base = unsafe { VirtualAlloc(core::ptr::null_mut(), size, MEM_COMMIT_RESERVE, PAGE_READWRITE) } as usize;
    if base == 0 {
        record(M, "ApuCreateHeap", &format!("VirtualAlloc({size}) FAILED -> E_FAIL"));
        return compat_common::E_FAIL;
    }
    record(M, "ApuCreateHeap", &format!("size={size_bytes} flags={flags:#x} -> heap @ {base:#x} S_OK"));
    *g = Some(Heap { base, size, offset: 0 });
    S_OK
}

#[no_mangle]
pub extern "system" fn ApuAlloc(pp_block: *mut *mut c_void, p_phys: *mut u64, size: u32, align: u32) -> i32 {
    let mut g = HEAP.lock().unwrap();
    let heap = match g.as_mut() {
        Some(h) => h,
        None => {
            record(M, "ApuAlloc", "no heap -> E_FAIL");
            return compat_common::E_FAIL;
        }
    };
    let align = (align as usize).max(16);
    let start = (heap.base + heap.offset + (align - 1)) & !(align - 1);
    let end = start + size as usize;
    let ptr = if end <= heap.base + heap.size {
        heap.offset = end - heap.base;
        start
    } else {
        // Heap exhausted: fall back to a standalone committed region.
        let p = unsafe { VirtualAlloc(core::ptr::null_mut(), (size as usize).max(0x1000), MEM_COMMIT_RESERVE, PAGE_READWRITE) } as usize;
        record(M, "ApuAlloc", &format!("heap full; standalone alloc size={size} -> {p:#x}"));
        p
    };
    if !pp_block.is_null() {
        unsafe { *pp_block = ptr as *mut c_void };
    }
    if !p_phys.is_null() {
        unsafe { *p_phys = ptr as u64 }; // fake physical address = virtual
    }
    S_OK
}

#[no_mangle]
pub extern "system" fn ApuHeapGetState(out: *mut u32, idx: u32) -> i32 {
    let g = HEAP.lock().unwrap();
    let (free, used) = match g.as_ref() {
        Some(h) => ((h.size - h.offset) as u32, h.offset as u32),
        None => (0u32, 0u32),
    };
    // Game reads out[0] and out[1] (offsets 0 and 4). Write exactly those.
    if !out.is_null() {
        unsafe {
            *out = free;
            *out.add(1) = used;
        }
    }
    let _ = idx;
    S_OK
}

#[no_mangle]
pub extern "system" fn ApuFree(_block: *mut c_void) -> i32 {
    // Bump allocator: individual frees are no-ops (APU heap freed at teardown).
    S_OK
}

#[no_mangle]
pub extern "system" fn AcpHalCreate(_params: *const c_void, out: *mut *mut c_void) -> i32 {
    // Return a non-null opaque handle so callers that store/deref it don't crash.
    if !out.is_null() {
        unsafe { *out = 1usize as *mut c_void };
    }
    record(M, "AcpHalCreate", "-> S_OK (opaque handle)");
    S_OK
}

#[no_mangle]
pub extern "system" fn AcpHalAllocateShapeContexts(_a: *mut c_void, _b: u32, _c: *mut c_void) -> i32 {
    record(M, "AcpHalAllocateShapeContexts", "-> S_OK (no SHAPE DSP)");
    S_OK
}
