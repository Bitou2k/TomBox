//! In-process network isolation for the title.
//!
//! Project rule (CLAUDE.md §10): never contact Fortnite/Epic production services, and run
//! execution experiments with no unnecessary network access. The title and the EOS SDK
//! link WinHTTP and Winsock directly, so once the online subsystem starts they issue real
//! requests (observed: analytics to datarouter.ol.epicgames.com). This guard makes those
//! calls fail locally, inside the title process, before anything leaves the machine:
//!
//!   * name resolution  — getaddrinfo / GetAddrInfoW / GetAddrInfoExW / gethostbyname
//!   * outbound sockets — connect / WSAConnect / WSAConnectByName* / sendto / WSASendTo
//!   * WinHTTP          — WinHttpConnect / WinHttpOpenRequest / WinHttpSendRequest
//!
//! Each entry point is redirected (12-byte `mov rax, imm64; jmp rax`) to a stub that sets
//! a network-unreachable error and returns the API's failure value. The x64 ABI lets one
//! argument-agnostic stub serve every signature (caller cleans up).
//!
//! Limits (documented, not hidden): a socket connected through `ConnectEx` to a literal IP
//! address is not covered, and other processes are unaffected. Set `TOMBOX_ALLOW_NETWORK=1`
//! to disable the guard (never needed for bring-up).

use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};

extern "system" {
    fn GetModuleHandleA(name: *const u8) -> *mut c_void;
    fn LoadLibraryA(name: *const u8) -> *mut c_void;
    fn GetProcAddress(h: *mut c_void, name: *const u8) -> *mut c_void;
    fn VirtualProtect(addr: *mut c_void, size: usize, new_protect: u32, old_protect: *mut u32) -> i32;
    fn FlushInstructionCache(process: *mut c_void, addr: *const c_void, size: usize) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn SetLastError(code: u32);
}

const WSAENETUNREACH: u32 = 10051;
const WSAHOST_NOT_FOUND: i32 = 11001;
const ERROR_WINHTTP_CANNOT_CONNECT: u32 = 12029;

static DENIED: AtomicUsize = AtomicUsize::new(0);

fn note(what: &str) {
    let n = DENIED.fetch_add(1, Ordering::Relaxed);
    if n < 40 || n % 500 == 0 {
        crate::record_raw("net_guard", "denied", &format!("{what} (total blocked calls: {})", n + 1));
    }
}

/// Winsock call returning SOCKET_ERROR.
unsafe extern "system" fn deny_sock() -> i32 {
    note("socket connect/send");
    SetLastError(WSAENETUNREACH); // WSAGetLastError reads the thread's last-error value
    -1
}
/// Winsock BOOL-returning connect helpers.
unsafe extern "system" fn deny_sock_bool() -> i32 {
    note("WSAConnectByName/List");
    SetLastError(WSAENETUNREACH);
    0
}
/// getaddrinfo family: returns the error code directly.
unsafe extern "system" fn deny_resolve() -> i32 {
    note("name resolution");
    SetLastError(WSAHOST_NOT_FOUND as u32);
    WSAHOST_NOT_FOUND
}
/// Pointer/handle-returning calls (gethostbyname, WinHttpConnect, WinHttpOpenRequest).
unsafe extern "system" fn deny_null() -> usize {
    note("WinHTTP connect / gethostbyname");
    SetLastError(ERROR_WINHTTP_CANNOT_CONNECT);
    0
}
/// WinHTTP BOOL-returning calls.
unsafe extern "system" fn deny_false() -> i32 {
    note("WinHttpSendRequest");
    SetLastError(ERROR_WINHTTP_CANNOT_CONNECT);
    0
}

unsafe fn redirect(module: &[u8], func: &[u8], stub: usize) -> bool {
    let mut h = GetModuleHandleA(module.as_ptr());
    if h.is_null() {
        h = LoadLibraryA(module.as_ptr());
    }
    if h.is_null() {
        return false;
    }
    let p = GetProcAddress(h, func.as_ptr()) as *mut u8;
    if p.is_null() {
        return false;
    }
    // mov rax, imm64 ; jmp rax
    let mut code = [0u8; 12];
    code[0] = 0x48;
    code[1] = 0xB8;
    code[2..10].copy_from_slice(&(stub as u64).to_le_bytes());
    code[10] = 0xFF;
    code[11] = 0xE0;
    let mut old = 0u32;
    if VirtualProtect(p as *mut c_void, code.len(), 0x40, &mut old) == 0 {
        return false;
    }
    core::ptr::copy_nonoverlapping(code.as_ptr(), p, code.len());
    let mut tmp = 0u32;
    VirtualProtect(p as *mut c_void, code.len(), old, &mut tmp);
    FlushInstructionCache(GetCurrentProcess(), p as *const c_void, code.len());
    true
}

/// Install the guard once per process image that links compat-common.
pub fn ensure() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if std::env::var_os("TOMBOX_ALLOW_NETWORK").map(|v| v == "1").unwrap_or(false) {
            crate::record_raw("net_guard", "disabled", "TOMBOX_ALLOW_NETWORK=1 — title network access NOT blocked");
            return;
        }
        let ws = b"ws2_32.dll\0";
        let wh = b"winhttp.dll\0";
        let table: [(&[u8], &[u8], usize); 14] = [
            (ws, b"getaddrinfo\0", deny_resolve as usize),
            (ws, b"GetAddrInfoW\0", deny_resolve as usize),
            (ws, b"GetAddrInfoExW\0", deny_resolve as usize),
            (ws, b"GetAddrInfoExA\0", deny_resolve as usize),
            (ws, b"gethostbyname\0", deny_null as usize),
            (ws, b"connect\0", deny_sock as usize),
            (ws, b"WSAConnect\0", deny_sock as usize),
            (ws, b"WSAConnectByNameW\0", deny_sock_bool as usize),
            (ws, b"WSAConnectByNameA\0", deny_sock_bool as usize),
            (ws, b"sendto\0", deny_sock as usize),
            (ws, b"WSASendTo\0", deny_sock as usize),
            (wh, b"WinHttpConnect\0", deny_null as usize),
            (wh, b"WinHttpOpenRequest\0", deny_null as usize),
            (wh, b"WinHttpSendRequest\0", deny_false as usize),
        ];
        let mut ok = 0;
        let mut missing = Vec::new();
        for (m, f, stub) in table {
            if unsafe { redirect(m, f, stub) } {
                ok += 1;
            } else {
                missing.push(String::from_utf8_lossy(&f[..f.len() - 1]).into_owned());
            }
        }
        crate::record_raw(
            "net_guard",
            "installed",
            &format!("{ok}/14 network entry points blocked in-process{}", if missing.is_empty() { String::new() } else { format!(" (not patched: {})", missing.join(", ")) }),
        );
    });
}
