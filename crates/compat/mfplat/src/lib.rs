//! `mfplat.dll` runtime-forwarding proxy.
//!
//! The Fortnite EXE imports 16 functions from mfplat. 15 are standard Media
//! Foundation functions present in the real system mfplat.dll; exactly one,
//! `MFResetDXGIDeviceManagerX`, is Xbox-only and absent on PC.
//!
//! Strategy: at first call we load a renamed copy of the real system mfplat
//! (`mfplat_sys.dll`, placed next to us — a different base name, so no self-
//! reference) and resolve the 15 standard exports from it. Each of our exported
//! functions is a thin thunk that tail-forwards to the real one.
//!
//! ABI note: all 15 take at most 5 integer/pointer arguments and return an
//! HRESULT/LONGLONG in RAX (no float args/returns). A single generic
//! `extern "system" fn(6 x usize) -> u64` thunk therefore forwards every one
//! correctly: the extra register/stack slots are ignored by the callee, and RAX
//! is passed straight back. `MFResetDXGIDeviceManagerX` is our own stub.

#![allow(non_snake_case)]

use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Once;

type HMODULE = *mut c_void;

extern "system" {
    fn LoadLibraryW(lpLibFileName: *const u16) -> HMODULE;
    fn GetProcAddress(hModule: HMODULE, lpProcName: *const u8) -> *mut c_void;
    fn GetModuleHandleExW(dwFlags: u32, lpModuleName: *const u16, phModule: *mut HMODULE) -> i32;
    fn GetModuleFileNameW(hModule: HMODULE, lpFilename: *mut u16, nSize: u32) -> u32;
}

const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 4;
const GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT: u32 = 2;

const N: usize = 15;

/// NUL-terminated export names, index-aligned with `PTRS`.
static NAMES: [&[u8]; N] = [
    b"MFStartup\0",
    b"MFShutdown\0",
    b"MFCreateMediaType\0",
    b"MFCreateSample\0",
    b"MFCreateMemoryBuffer\0",
    b"MFCreateAlignedMemoryBuffer\0",
    b"MFCreateAttributes\0",
    b"MFCreateAsyncResult\0",
    b"MFInvokeCallback\0",
    b"MFCreateFile\0",
    b"MFCreateDXGIDeviceManager\0",
    b"MFCreateWaveFormatExFromMFMediaType\0",
    b"MFCreateSourceResolver\0",
    b"MFPutWorkItem2\0",
    b"MFllMulDiv\0",
];

static PTRS: [AtomicUsize; N] = [const { AtomicUsize::new(0) }; N];
static INIT: Once = Once::new();

type Fn6 = extern "system" fn(usize, usize, usize, usize, usize, usize) -> u64;

/// Resolve the path of the real mfplat (copied next to us as `mfplat_sys.dll`)
/// and load all 15 standard exports. Runs once.
fn ensure() {
    INIT.call_once(|| {
        // Find our own module's directory via the address of this function.
        let mut hself: HMODULE = core::ptr::null_mut();
        let addr = ensure as *const c_void as *const u16;
        let ok = unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                    | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                addr,
                &mut hself,
            )
        };
        if ok == 0 {
            compat_common::record("mfplat", "ensure", "GetModuleHandleExW failed");
            return;
        }
        let mut buf = [0u16; 32768];
        let len = unsafe { GetModuleFileNameW(hself, buf.as_mut_ptr(), buf.len() as u32) } as usize;
        if len == 0 || len >= buf.len() {
            compat_common::record("mfplat", "ensure", "GetModuleFileNameW failed");
            return;
        }
        // Replace the final path component with "mfplat_sys.dll".
        let mut path: Vec<u16> = buf[..len].to_vec();
        let sep = path.iter().rposition(|&c| c == b'\\' as u16 || c == b'/' as u16);
        let dir_end = sep.map(|i| i + 1).unwrap_or(0);
        path.truncate(dir_end);
        path.extend("mfplat_sys.dll\0".encode_utf16());

        let real = unsafe { LoadLibraryW(path.as_ptr()) };
        if real.is_null() {
            compat_common::record(
                "mfplat",
                "ensure",
                "LoadLibraryW(mfplat_sys.dll) failed — is mfplat_sys.dll present next to mfplat.dll?",
            );
            return;
        }
        let mut resolved = 0;
        for i in 0..N {
            let p = unsafe { GetProcAddress(real, NAMES[i].as_ptr()) } as usize;
            PTRS[i].store(p, Ordering::Relaxed);
            if p != 0 {
                resolved += 1;
            }
        }
        compat_common::record(
            "mfplat",
            "ensure",
            &format!("loaded mfplat_sys.dll, resolved {resolved}/{N} exports"),
        );
    });
}

#[inline]
fn forward(idx: usize, a: usize, b: usize, c: usize, d: usize, e: usize, f: usize) -> u64 {
    ensure();
    let p = PTRS[idx].load(Ordering::Relaxed);
    if p == 0 {
        compat_common::record("mfplat", "forward", &format!("export #{idx} unresolved -> E_FAIL"));
        return compat_common::E_FAIL as u32 as u64;
    }
    let g: Fn6 = unsafe { core::mem::transmute(p) };
    g(a, b, c, d, e, f)
}

macro_rules! thunk {
    ($name:ident, $idx:expr) => {
        #[no_mangle]
        pub extern "system" fn $name(a: usize, b: usize, c: usize, d: usize, e: usize, f: usize) -> u64 {
            forward($idx, a, b, c, d, e, f)
        }
    };
}

thunk!(MFStartup, 0);
thunk!(MFShutdown, 1);
thunk!(MFCreateMediaType, 2);
thunk!(MFCreateSample, 3);
thunk!(MFCreateMemoryBuffer, 4);
thunk!(MFCreateAlignedMemoryBuffer, 5);
thunk!(MFCreateAttributes, 6);
thunk!(MFCreateAsyncResult, 7);
thunk!(MFInvokeCallback, 8);
thunk!(MFCreateFile, 9);
thunk!(MFCreateDXGIDeviceManager, 10);
thunk!(MFCreateWaveFormatExFromMFMediaType, 11);
thunk!(MFCreateSourceResolver, 12);
thunk!(MFPutWorkItem2, 13);
thunk!(MFllMulDiv, 14);

/// The one Xbox-only export (absent on PC). Associates a D3D device with an
/// IMFDXGIDeviceManager for hardware video decode — needed only for video
/// playback, not a first frame. Stub for now.
#[no_mangle]
pub extern "system" fn MFResetDXGIDeviceManagerX(
    _device_manager: *mut c_void,
    _device: *mut c_void,
    _reset_token: u32,
) -> i32 {
    compat_common::record(
        "mfplat",
        "MFResetDXGIDeviceManagerX",
        "[STUB -> E_NOTIMPL] (video-only; impl via IMFDXGIDeviceManager::ResetDevice later)",
    );
    compat_common::E_NOTIMPL
}
