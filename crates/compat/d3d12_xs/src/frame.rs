//! Xbox frame-pacing model: frame intervals, frame-pipeline tokens, flip events and
//! per-frame statistics (device slots 78..81, consumed by queue slot 30 `PresentX`).
//!
//! Evidence (title call sites; names are the public GDK ones where the argument pattern
//! matches, otherwise descriptive):
//!   * slot 78 `SetFrameIntervalX(output, lengthUs, periodicity, flags)`  — EXE+0x704e6e
//!   * slot 79 `ScheduleFrameEventX(type, offsetUs, ancillary, flags)`    — EXE+0x704d36/7b
//!       ancillary = { u32 kind = 1; void* @+8 } where the pointer is the address of the
//!       RHI's flip-event HANDLE field (the handle RHIWaitForFlip waits on).
//!   * slot 80 `WaitFrameEventX(type, timeoutMs, ancillary, flags, u64* token)` — EXE+0x7048dc
//!       the token is stored in a 16-entry ring (TFramePipelineTokenRingBuffer) and passed to
//!       PresentX in the plane parameters.
//!   * slot 81 frame statistics `(u64 token, u32 type, u32* count /*in: 1*/, void* stats)`
//!       — EXE+0x7033f6 (GetFrameStatisticsForMostRecentFrame). The caller accepts the
//!       result only if hr >= 0, *count == 1 and the first u32 of `stats` equals `type`;
//!       for type 0x10000 it then reads two u64 tick counts at stats+0x10 / +0x18 and
//!       converts them to seconds with the platform seconds-per-cycle constant.
//!
//! Model implemented here:
//!   * a timer thread ticks once per frame interval, wakes `WaitFrameEventX` callers and
//!     signals registered flip events;
//!   * tokens are a monotonically increasing non-zero counter;
//!   * `PresentX` marks its token as flipped at the current QPC time; statistics succeed
//!     only for flipped tokens and report that time in both tick fields.
//!
//! Not modelled / unverified: event offsets within the interval, alignment to the host's
//! real vblank, which of the two tick fields is which timestamp, any other statistics
//! `type` layouts (only the header + two tick fields are written), and the true size of
//! the statistics structure.

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, Once};
use compat_common::record;

extern "system" {
    fn SetEvent(h: *mut c_void) -> i32;
    fn QueryPerformanceCounter(v: *mut i64) -> i32;
    fn IsBadReadPtr(p: *const c_void, n: usize) -> i32;
}

const S_OK: i32 = 0;
const E_FAIL: i32 = 0x80004005u32 as i32;
const E_INVALIDARG: i32 = 0x80070057u32 as i32;
const E_NOTIMPL: i32 = 0x80004001u32 as i32;
const WAIT_TIMEOUT_HR: i32 = 0x80070102u32 as i32; // HRESULT_FROM_WIN32(WAIT_TIMEOUT)

static INTERVAL_US: AtomicU32 = AtomicU32::new(16_667);
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);
/// Addresses of HANDLE fields to signal each interval.
static EVENTS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static TICK: Mutex<u64> = Mutex::new(0);
static TICK_CV: Condvar = Condvar::new();
/// Recently flipped tokens: (token, QPC time of the PresentX call).
static FLIPPED: Mutex<Vec<(u64, i64)>> = Mutex::new(Vec::new());
const FLIPPED_KEEP: usize = 64;

fn qpc() -> i64 {
    let mut v = 0i64;
    unsafe { QueryPerformanceCounter(&mut v) };
    v
}

fn ensure_timer() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        std::thread::spawn(|| loop {
            let us = INTERVAL_US.load(Ordering::Relaxed) as u64;
            std::thread::sleep(std::time::Duration::from_micros(us));
            {
                let mut t = TICK.lock().unwrap();
                *t += 1;
                TICK_CV.notify_all();
            }
            let evs = EVENTS.lock().unwrap().clone();
            for p in evs {
                let h = unsafe { *(p as *const usize) };
                if h != 0 && unsafe { SetEvent(h as *mut c_void) } == 0 {
                    static WARNED: Once = Once::new();
                    WARNED.call_once(|| record("d3d12_xs", "frame", &format!("SetEvent({h:#x}) FAILED")));
                }
            }
        });
    });
}

/// Slot 78.
pub unsafe extern "system" fn set_frame_interval_x(_this: *mut c_void, _output: *mut c_void, len_us: u32, periodicity: u32, flags: u32) -> i32 {
    if len_us != 0 {
        INTERVAL_US.store(len_us.max(1000), Ordering::Relaxed);
    }
    record("d3d12_xs", "SetFrameIntervalX", &format!("length={len_us}us periodicity={periodicity} flags={flags:#x}"));
    S_OK
}

/// Slot 79.
pub unsafe extern "system" fn schedule_frame_event_x(_this: *mut c_void, ty: u32, offset_us: u32, anc: *const u8, flags: u32) -> i32 {
    let (kind, ptr) = if anc.is_null() { (0u32, 0usize) } else { (*(anc as *const u32), *(anc.add(8) as *const usize)) };
    record("d3d12_xs", "ScheduleFrameEventX", &format!(
        "type={ty} offset={offset_us}us ancillary={{kind={kind}, ptr={ptr:#x}}} flags={flags:#x}"));
    ensure_timer();
    if anc.is_null() {
        return S_OK; // moves the frame-event origin only; offsets are not modelled
    }
    if kind != 1 {
        record("d3d12_xs", "ScheduleFrameEventX", "UNSUPPORTED ancillary kind -> E_NOTIMPL");
        return E_NOTIMPL;
    }
    if ptr < 0x10000 || IsBadReadPtr(ptr as *const c_void, 8) != 0 {
        record("d3d12_xs", "ScheduleFrameEventX", "ancillary is not a readable pointer -> E_INVALIDARG");
        return E_INVALIDARG;
    }
    let mut evs = EVENTS.lock().unwrap();
    if !evs.contains(&ptr) {
        evs.push(ptr);
    }
    S_OK
}

/// Slot 80: block until the next frame interval, then issue a token.
pub unsafe extern "system" fn wait_frame_event_x(_this: *mut c_void, ty: u32, timeout_ms: u32, anc: *const u8, flags: u32, token: *mut u64) -> i32 {
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let n = CALLS.fetch_add(1, Ordering::Relaxed);
    if n < 3 {
        record("d3d12_xs", "WaitFrameEventX", &format!(
            "type={ty} timeout={timeout_ms}ms ancillary={anc:p} flags={flags:#x} token_out={token:p}"));
    }
    if ty != 0 || !anc.is_null() {
        record("d3d12_xs", "WaitFrameEventX", &format!("UNSUPPORTED type={ty} / ancillary -> E_NOTIMPL"));
        return E_NOTIMPL;
    }
    ensure_timer();
    let guard = TICK.lock().unwrap();
    let start = *guard;
    let timed_out = if timeout_ms == u32::MAX {
        let _g = TICK_CV.wait_while(guard, |t| *t == start).unwrap();
        false
    } else {
        let (_g, r) = TICK_CV
            .wait_timeout_while(guard, std::time::Duration::from_millis(timeout_ms as u64), |t| *t == start)
            .unwrap();
        r.timed_out()
    };
    if timed_out {
        return WAIT_TIMEOUT_HR;
    }
    if !token.is_null() {
        *token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    }
    S_OK
}

/// Called by PresentX with the token found in its plane parameters.
pub fn mark_flipped(token: u64) {
    if token == 0 {
        return;
    }
    let mut f = FLIPPED.lock().unwrap();
    f.push((token, qpc()));
    if f.len() > FLIPPED_KEEP {
        let drop = f.len() - FLIPPED_KEEP;
        f.drain(..drop);
    }
}

/// Slot 81: statistics for the frame identified by `token`.
pub unsafe extern "system" fn frame_statistics_x(_this: *mut c_void, token: u64, ty: u32, count: *mut u32, stats: *mut u8) -> i32 {
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let n = CALLS.fetch_add(1, Ordering::Relaxed);
    let flipped = FLIPPED.lock().unwrap().iter().rev().find(|(t, _)| *t == token).map(|(_, q)| *q);
    if n < 4 {
        record("d3d12_xs", "FrameStatistics", &format!(
            "token={token:#x} type={ty:#x} count_in={} -> {}",
            if count.is_null() { 0 } else { *count },
            if flipped.is_some() { "flipped" } else { "not flipped yet" }));
    }
    if count.is_null() || stats.is_null() {
        return E_INVALIDARG;
    }
    let Some(t) = flipped else {
        *count = 0;
        return E_FAIL;
    };
    if *count < 1 {
        return E_INVALIDARG;
    }
    *count = 1;
    *(stats as *mut u32) = ty;
    *(stats.add(0x10) as *mut i64) = t;
    *(stats.add(0x18) as *mut i64) = t;
    S_OK
}
