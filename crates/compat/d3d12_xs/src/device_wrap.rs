//! A COM wrapper around a real PC `ID3D12Device` that lets us intercept
//! `QueryInterface` (to supply Xbox-only interfaces such as the merged DXGI
//! device) while forwarding every other `ID3D12Device` method straight to the
//! real device via naked tail-call thunks.
//!
//! Layout: `{ vtable, real, refs }`. The forwarding thunks replace `this` (our
//! wrapper) with `this->real` and jump to the real method at the same vtable
//! slot — ABI-exact for any signature.

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use compat_common::record;

const S_OK: i32 = 0;
const E_POINTER: i32 = 0x8000_4003u32 as i32;
const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;

#[repr(C)]
#[derive(PartialEq)]
pub struct Guid {
    pub d1: u32,
    pub d2: u16,
    pub d3: u16,
    pub d4: [u8; 8],
}

const IID_IUNKNOWN: Guid = Guid { d1: 0, d2: 0, d3: 0, d4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };

/// ID3D12Device through ID3D12Device8 — all resolve to our wrapper so every
/// device call (CreateDescriptorHeap etc.) stays intercepted.
const DEVICE_IIDS: [Guid; 9] = [
    Guid { d1: 0x1898_19F1, d2: 0x1DB6, d3: 0x4B57, d4: [0xBE, 0x54, 0x18, 0x21, 0x33, 0x9B, 0x85, 0xF7] }, // Device
    Guid { d1: 0x77ac_ce80, d2: 0x638e, d3: 0x4e65, d4: [0x88, 0x95, 0xc1, 0xf2, 0x33, 0x86, 0x86, 0x3e] }, // Device1
    Guid { d1: 0x30ba_a41e, d2: 0xb15b, d3: 0x475c, d4: [0xa0, 0xbb, 0x1a, 0xf5, 0xc5, 0xb6, 0x43, 0x28] }, // Device2
    Guid { d1: 0x81da_dc15, d2: 0x2bad, d3: 0x4392, d4: [0x93, 0xc5, 0x10, 0x13, 0x45, 0xc4, 0xaa, 0x98] }, // Device3
    Guid { d1: 0xe865_df17, d2: 0xa9ee, d3: 0x46f9, d4: [0xa4, 0x63, 0x30, 0x98, 0x31, 0x5a, 0xa2, 0xe5] }, // Device4
    Guid { d1: 0x8b4f_173b, d2: 0x2fea, d3: 0x4b80, d4: [0x8f, 0x58, 0x43, 0x07, 0x19, 0x1a, 0xb9, 0x5d] }, // Device5
    Guid { d1: 0xc70b_221b, d2: 0x40e4, d3: 0x4a17, d4: [0x89, 0xaf, 0x02, 0x5a, 0x07, 0x27, 0xa6, 0xdc] }, // Device6
    Guid { d1: 0x5c01_4b53, d2: 0x68a1, d3: 0x4b9b, d4: [0x8b, 0xd1, 0xdd, 0x60, 0x46, 0xb9, 0x35, 0x8b] }, // Device7
    Guid { d1: 0x9218_e6bb, d2: 0xf944, d3: 0x4f7e, d4: [0xa7, 0x5c, 0xb1, 0xb2, 0xc7, 0xb7, 0x01, 0xf3] }, // Device8
];

fn is_device_iid(g: &Guid) -> bool {
    DEVICE_IIDS.iter().any(|d| d == g)
}

#[repr(C)]
pub struct DeviceWrapper {
    vtable: *const usize,
    real: *mut c_void,
    refs: AtomicU32,
}

fn guid_str(g: &Guid) -> String {
    format!(
        "{{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
        g.d1, g.d2, g.d3, g.d4[0], g.d4[1], g.d4[2], g.d4[3], g.d4[4], g.d4[5], g.d4[6], g.d4[7]
    )
}

unsafe fn real_vtbl(real: *mut c_void) -> *const usize {
    unsafe { *(real as *const *const usize) }
}

unsafe extern "system" fn qi(this: *mut DeviceWrapper, riid: *const Guid, ppv: *mut *mut c_void) -> i32 {
    if ppv.is_null() || riid.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = core::ptr::null_mut() };
    let g = unsafe { &*riid };
    if is_device_iid(g) || *g == IID_IUNKNOWN {
        unsafe {
            *ppv = this as *mut c_void;
            (*this).refs.fetch_add(1, Ordering::SeqCst);
        }
        return S_OK;
    }
    // Xbox "DXGIDevice" interface: synthesize it (PC D3D12 devices lack IDXGIDevice).
    if *g == crate::dxgi_device::IID_XBOX_DXGIDEVICE {
        let shim = crate::dxgi_device::create();
        unsafe { *ppv = shim };
        record("d3d12_xs", "device.QueryInterface", "Xbox DXGIDevice -> synthesized shim");
        return S_OK;
    }
    // Forward anything else to the real device (higher ID3D12Device versions,
    // info-queue, etc.). Log the IID + result so unknown Xbox IIDs surface.
    let real = unsafe { (*this).real };
    let rvt = unsafe { real_vtbl(real) };
    let real_qi: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*rvt) };
    let hr = unsafe { real_qi(real, riid, ppv) };
    record(
        "d3d12_xs",
        "device.QueryInterface",
        &format!("{} -> hr={hr:#010x}{}", guid_str(g), if hr != S_OK { " (UNHANDLED)" } else { "" }),
    );
    if hr == S_OK { hr } else { E_NOINTERFACE }
}

unsafe extern "system" fn addref(this: *mut DeviceWrapper) -> u32 {
    unsafe { (*this).refs.fetch_add(1, Ordering::SeqCst) + 1 }
}

unsafe extern "system" fn release(this: *mut DeviceWrapper) -> u32 {
    let n = unsafe { (*this).refs.fetch_sub(1, Ordering::SeqCst) } - 1;
    if n == 0 {
        unsafe {
            let real = (*this).real;
            let rvt = real_vtbl(real);
            let real_rel: unsafe extern "system" fn(*mut c_void) -> u32 =
                core::mem::transmute(*rvt.add(2));
            real_rel(real);
            drop(Box::from_raw(this));
        }
    }
    n
}

/// Intercept `CheckFeatureSupport` (slot 13): the Xbox feature structs differ in
/// size from PC, so a verbatim forward gets E_INVALIDARG. Zero the caller buffer
/// and negotiate the largest size <= the requested size that the real device
/// accepts (D3D12 feature structs are append-only, so filling the PC-known prefix
/// and leaving Xbox-only trailing fields zero is sound).
unsafe extern "system" fn check_feature_support(
    this: *mut DeviceWrapper, feature: u32, pdata: *mut c_void, size: u32,
) -> i32 {
    let real = unsafe { (*this).real };
    let rvt = unsafe { real_vtbl(real) };
    let f: unsafe extern "system" fn(*mut c_void, u32, *mut c_void, u32) -> i32 =
        unsafe { core::mem::transmute(*rvt.add(13)) };
    let hr = unsafe { f(real, feature, pdata, size) };
    if hr == S_OK {
        return S_OK;
    }
    if !pdata.is_null() && size > 0 && size <= 65536 {
        unsafe { core::ptr::write_bytes(pdata as *mut u8, 0, size as usize) };
        let mut try_size = size & !3; // 4-byte aligned
        while try_size >= 4 {
            let hr2 = unsafe { f(real, feature, pdata, try_size) };
            if hr2 == S_OK {
                record("d3d12_xs", "CheckFeatureSupport",
                    &format!("feature={feature} xbox_size={size} -> PC size={try_size} S_OK"));
                return S_OK;
            }
            try_size -= 4;
        }
    }
    record("d3d12_xs", "CheckFeatureSupport", &format!("feature={feature} size={size} UNHANDLED hr={hr:#010x}"));
    hr
}

/// Intercept `CreateDescriptorHeap` (slot 14) and wrap the returned heap so the
/// descriptor-handle getters get the Xbox RAX return-convention bridge.
unsafe extern "system" fn create_descriptor_heap(
    this: *mut DeviceWrapper, pdesc: *const c_void, riid: *const Guid, ppv: *mut *mut c_void,
) -> i32 {
    let real = unsafe { (*this).real };
    let f: unsafe extern "system" fn(*mut c_void, *const c_void, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*real_vtbl(real).add(14)) };
    let hr = unsafe { f(real, pdesc, riid, ppv) };
    if hr == S_OK && !ppv.is_null() {
        let real_heap = unsafe { *ppv };
        if !real_heap.is_null() {
            let wrapped = crate::heap_wrap::wrap(real_heap);
            unsafe { *ppv = wrapped };
        }
    }
    hr
}

/// Intercept the Xbox extension `CreateCommandQueueX` (slot 48) and translate to
/// the real PC `CreateCommandQueue` (slot 8). The Xbox
/// `D3D12XBOX_COMMAND_QUEUE_DESC` extends the PC `D3D12_COMMAND_QUEUE_DESC`; the
/// first 16 bytes ({Type, Priority, Flags, NodeMask}) are layout-compatible.
unsafe extern "system" fn create_command_queue_x(
    this: *mut DeviceWrapper, pdescx: *const c_void, riid: *const Guid, ppv: *mut *mut c_void,
) -> i32 {
    let real = unsafe { (*this).real };
    let mut pc_desc = [0u8; 16];
    if !pdescx.is_null() {
        unsafe { core::ptr::copy_nonoverlapping(pdescx as *const u8, pc_desc.as_mut_ptr(), 16) };
    }
    let f: unsafe extern "system" fn(*mut c_void, *const c_void, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*real_vtbl(real).add(8)) }; // PC CreateCommandQueue
    let hr = unsafe { f(real, pc_desc.as_ptr() as *const c_void, riid, ppv) };
    let qtype = u32::from_le_bytes([pc_desc[0], pc_desc[1], pc_desc[2], pc_desc[3]]);
    let qflags = u32::from_le_bytes([pc_desc[8], pc_desc[9], pc_desc[10], pc_desc[11]]);
    record("d3d12_xs", "CreateCommandQueueX", &format!("type={qtype} flags={qflags} -> hr={hr:#010x}"));
    hr
}

/// Xbox slot 73 = `CreateCommandList` (standard signature, relocated in the Xbox
/// vtable) -> PC `CreateCommandList` (slot 12).
unsafe extern "system" fn create_command_list_x(
    this: *mut DeviceWrapper, node_mask: u32, list_type: u32, alloc: *mut c_void,
    init_state: *mut c_void, riid: *const Guid, ppv: *mut *mut c_void,
) -> i32 {
    let real = unsafe { (*this).real };
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, *mut c_void, *mut c_void, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*real_vtbl(real).add(12)) };
    let hr = unsafe { f(real, node_mask, list_type, alloc, init_state, riid, ppv) };
    record("d3d12_xs", "CreateCommandList", &format!("type={list_type} -> hr={hr:#010x}"));
    hr
}

// Forwarding thunk: this(rcx) -> this->real; jmp real->vtbl[slot].
macro_rules! fwd {
    ($name:ident, $slot:expr) => {
        #[unsafe(naked)]
        unsafe extern "system" fn $name() {
            core::arch::naked_asm!(
                "mov rcx, [rcx+8]",
                "mov rax, [rcx]",
                "jmp qword ptr [rax + {o}]",
                o = const ($slot * 8usize),
            );
        }
    };
}

// ID3D12Device methods 3..=43 (0,1,2 are IUnknown, handled above).
fwd!(f3, 3); fwd!(f4, 4); fwd!(f5, 5); fwd!(f6, 6); fwd!(f7, 7); fwd!(f8, 8);
fwd!(f9, 9); fwd!(f10, 10); fwd!(f11, 11); fwd!(f12, 12);
/* 13 = check_feature_support, 14 = create_descriptor_heap */
fwd!(f15, 15); fwd!(f16, 16); fwd!(f17, 17); fwd!(f18, 18); fwd!(f19, 19); fwd!(f20, 20);
fwd!(f21, 21); fwd!(f22, 22); fwd!(f23, 23); fwd!(f24, 24); fwd!(f25, 25); fwd!(f26, 26);
fwd!(f27, 27); fwd!(f28, 28); fwd!(f29, 29); fwd!(f30, 30); fwd!(f31, 31); fwd!(f32, 32);
fwd!(f33, 33); fwd!(f34, 34); fwd!(f35, 35); fwd!(f36, 36); fwd!(f37, 37); fwd!(f38, 38);
fwd!(f39, 39); fwd!(f40, 40); fwd!(f41, 41); fwd!(f42, 42); fwd!(f43, 43);
// Slots 44+ are Xbox-specific device extensions (NOT PC ID3D12Device1-8), so they
// are NOT forwarded by slot number. Unrecognized ones get this fail-loud stub
// (returns E_NOTIMPL) so the game fails that call cleanly and names it in its log,
// instead of reading past the vtable and crashing. Recognized Xbox methods are
// intercepted individually (e.g. slot 48 = CreateCommandQueueX).
unsafe extern "system" fn dev_stub() -> i32 {
    0x8000_4001u32 as i32 // E_NOTIMPL
}

fn vtable() -> *const usize {
    static VT: OnceLock<usize> = OnceLock::new();
    *VT.get_or_init(|| {
        let mut v: Vec<usize> = vec![
            qi as usize, addref as usize, release as usize,
            f3 as usize, f4 as usize, f5 as usize, f6 as usize, f7 as usize, f8 as usize,
            f9 as usize, f10 as usize, f11 as usize, f12 as usize,
            check_feature_support as usize,   // 13
            create_descriptor_heap as usize,  // 14
            f15 as usize, f16 as usize, f17 as usize, f18 as usize, f19 as usize, f20 as usize,
            f21 as usize, f22 as usize, f23 as usize, f24 as usize, f25 as usize, f26 as usize,
            f27 as usize, f28 as usize, f29 as usize, f30 as usize, f31 as usize, f32 as usize,
            f33 as usize, f34 as usize, f35 as usize, f36 as usize, f37 as usize, f38 as usize,
            f39 as usize, f40 as usize, f41 as usize, f42 as usize, f43 as usize,
        ];
        // 44..160: Xbox-extension slots — fail-loud stub unless intercepted below.
        while v.len() < 160 {
            v.push(dev_stub as usize);
        }
        v[48] = create_command_queue_x as usize; // CreateCommandQueueX (verified)
        // Slot 73 is an Xbox command-list create whose exact signature differs from
        // PC CreateCommandList (needs the GDK d3d12_x headers); left as fail-loud
        // stub until its ABI is reversed. (create_command_list_x kept for reference.)
        let _ = create_command_list_x;
        Box::leak(v.into_boxed_slice()).as_ptr() as usize
    }) as *const usize
}

/// Wrap a real `ID3D12Device` so we can intercept QueryInterface.
pub fn wrap_device(real: *mut c_void) -> *mut c_void {
    let w = Box::new(DeviceWrapper { vtable: vtable(), real, refs: AtomicU32::new(1) });
    Box::into_raw(w) as *mut c_void
}
