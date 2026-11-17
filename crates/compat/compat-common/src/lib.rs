//! Shared call-logging for the compat DLLs.
//!
//! Every stubbed export records a line so we can reconstruct the exact runtime
//! call order of the real game — which is how we prioritize what to implement
//! for real next. Log target: `%TOMBOX_LOG%` if set, else
//! `%TEMP%\tombox_compat.log`. Appends; one open per call (robust across DLLs,
//! fast enough for this purpose).

use std::io::Write;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

fn log_path() -> &'static std::path::PathBuf {
    static PATH: OnceLock<std::path::PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        if let Ok(p) = std::env::var("TOMBOX_LOG") {
            return std::path::PathBuf::from(p);
        }
        let mut d = std::env::temp_dir();
        d.push("tombox_compat.log");
        d
    })
}

/// Record a call from `module` (e.g. "xmem") to `func` with a short `detail`.
pub fn record(module: &str, func: &str, detail: &str) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        let _ = writeln!(f, "{ts} [{module}] {func} {detail}");
    }
}

/// Common HRESULTs.
pub const S_OK: i32 = 0;
pub const E_NOTIMPL: i32 = 0x8000_4001u32 as i32;
pub const E_FAIL: i32 = 0x8000_4005u32 as i32;
