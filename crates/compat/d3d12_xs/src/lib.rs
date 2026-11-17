//! `d3d12_xs.dll` compatibility shim (Xbox Series Direct3D 12.x).
//!
//! These are logging stubs for now: they let the loader resolve imports so the
//! process starts and we can observe the real D3D12.x bring-up order. Device
//! creation returns `E_NOTIMPL` (fail-loud, no faked device) until the graphics
//! backend exists. The one real thing here is the exported `IID_ID3D12Device`
//! GUID data symbol, which the game imports by name.

use core::ffi::c_void;
use compat_common::{record, E_NOTIMPL, S_OK};

const M: &str = "d3d12_xs";

/// IID_ID3D12Device = {189819F1-1DB6-4B57-BE54-1821339B85F7}, little-endian
/// GUID byte layout (Data1 LE, Data2 LE, Data3 LE, Data4 as-is). This is a real,
/// correct value — the game reads it to request the device interface.
#[no_mangle]
pub static IID_ID3D12Device: [u8; 16] = [
    0xF1, 0x19, 0x98, 0x18, // Data1 = 0x189819F1
    0xB6, 0x1D, // Data2 = 0x1DB6
    0x57, 0x4B, // Data3 = 0x4B57
    0xBE, 0x54, 0x18, 0x21, 0x33, 0x9B, 0x85, 0xF7, // Data4
];

#[no_mangle]
pub extern "system" fn D3D12CreateDevice(
    _adapter: *mut c_void,
    _min_feature_level: u32,
    _riid: *const c_void,
    _ppdevice: *mut *mut c_void,
) -> i32 {
    record(M, "D3D12CreateDevice", "[STUB -> E_NOTIMPL] (no graphics backend yet)");
    E_NOTIMPL
}

#[no_mangle]
pub extern "system" fn D3D12XboxCreateDevice(
    _adapter: *mut c_void,
    _params: *const c_void,
    _riid: *const c_void,
    _ppdevice: *mut *mut c_void,
) -> i32 {
    record(M, "D3D12XboxCreateDevice", "[STUB -> E_NOTIMPL] (no graphics backend yet)");
    E_NOTIMPL
}

#[no_mangle]
pub extern "system" fn D3D12SerializeRootSignature(
    _desc: *const c_void,
    _version: u32,
    _ppblob: *mut *mut c_void,
    _pperror: *mut *mut c_void,
) -> i32 {
    record(M, "D3D12SerializeRootSignature", "[STUB -> E_NOTIMPL]");
    E_NOTIMPL
}

#[no_mangle]
pub extern "system" fn D3D12SerializeVersionedRootSignature(
    _desc: *const c_void,
    _ppblob: *mut *mut c_void,
    _pperror: *mut *mut c_void,
) -> i32 {
    record(M, "D3D12SerializeVersionedRootSignature", "[STUB -> E_NOTIMPL]");
    E_NOTIMPL
}

#[no_mangle]
pub extern "system" fn D3D12XboxSetProcessDebugFlags(_flags: u32) -> i32 {
    record(M, "D3D12XboxSetProcessDebugFlags", "[STUB -> S_OK] (benign)");
    S_OK
}
