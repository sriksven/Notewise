//! Noticing that somebody is typing, without learning what they typed.
//!
//! # The design decision this file exists to make
//!
//! Inline completion needs to know when the user paused. It does not need to know which keys they
//! pressed. Those are very different asks of a permission that grants the second in order to
//! provide the first, and the whole reason Input Monitoring is the most alarming grant on the
//! platform is that most software which asks for it keeps everything it sees.
//!
//! So the callback here reads the event type, records a timestamp, and returns. No key code is
//! stored, no character is decoded, nothing is buffered, and there is no field on any type in this
//! module that could hold a keystroke. What leaves is a count and a time.
//!
//! The tap is created `kCGEventTapOptionListenOnly`, which means the system will not let it modify
//! or swallow an event even if this code tried to. That is a property enforced by the OS rather
//! than promised by a comment.
//!
//! # Threading
//!
//! An event tap needs a run loop, and it does not need to be the main one — unlike a Carbon hotkey.
//! So the monitor owns a thread, builds its tap there, and runs that thread's loop. Nothing about
//! this has to coordinate with the window.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicU64, Ordering};

use crate::completion::TypingActivity;

type CFMachPortRef = *const c_void;
type CFRunLoopSourceRef = *const c_void;
type CFRunLoopRef = *mut c_void;
type CGEventRef = *const c_void;
type CGEventTapProxy = *const c_void;

/// `kCGSessionEventTap` — this login session's events.
const SESSION_TAP: u32 = 1;
/// `kCGHeadInsertEventTap`.
const HEAD_INSERT: u32 = 0;
/// `kCGEventTapOptionListenOnly` — cannot modify or drop an event. See the module docs.
const LISTEN_ONLY: u32 = 1;
/// `CGEventMaskBit(kCGEventKeyDown)`.
const KEY_DOWN_MASK: u64 = 1 << 10;
/// `kCGEventKeyDown`.
const KEY_DOWN: u32 = 10;
/// `kCGEventTapDisabledByTimeout` and `...ByUserInput`. The system disables a tap that took too
/// long; re-enabling is the documented recovery.
const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;

type CGEventTapCallBack =
    extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(loop_ref: CFRunLoopRef, source: CFRunLoopSourceRef, mode: *const c_void);
    fn CFRunLoopRun();
    fn CFRunLoopStop(loop_ref: CFRunLoopRef);
    fn CFRelease(cf: *const c_void);
    static kCFRunLoopCommonModes: *const c_void;
}

/// When the last keystroke happened, in milliseconds since the Unix epoch. Zero for none yet.
static LAST_KEYSTROKE_MS: AtomicI64 = AtomicI64::new(0);
/// How many keystrokes have been seen. For diagnostics — "is the tap actually working".
static KEYSTROKES: AtomicU64 = AtomicU64::new(0);
static RUNNING: AtomicBool = AtomicBool::new(false);
/// The monitor thread's run loop, so it can be stopped from elsewhere.
static RUN_LOOP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
/// The tap itself, kept so it can be re-enabled after the system disables it.
static TAP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// The callback. Runs on the monitor thread, inside event dispatch.
///
/// Two lines of work on purpose. A slow tap callback is not just slow — the system disables a tap
/// that overruns its deadline, and a disabled tap is a feature that stops working silently.
extern "C" fn on_event(
    _proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    match event_type {
        KEY_DOWN => {
            // A timestamp and a count. The event is not inspected, and there is nowhere here that
            // a key code could be put.
            LAST_KEYSTROKE_MS.store(now_ms(), Ordering::Relaxed);
            KEYSTROKES.fetch_add(1, Ordering::Relaxed);
        }
        TAP_DISABLED_BY_TIMEOUT | TAP_DISABLED_BY_USER_INPUT => {
            // The documented recovery. Without this the monitor goes quiet after one slow moment
            // and never says why.
            let tap = TAP.load(Ordering::Relaxed);
            if !tap.is_null() {
                // SAFETY: the pointer was stored by `start` from a successful `CGEventTapCreate`
                // and is only cleared after the run loop has stopped.
                unsafe { CGEventTapEnable(tap as CFMachPortRef, true) }
            }
        }
        _ => {}
    }

    // The event, unchanged. A listen-only tap's return value is ignored by the system, which is the
    // point: this cannot swallow somebody's keystroke.
    event
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Why the monitor could not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapRefusal {
    /// `CGEventTapCreate` returned null, which is what a missing grant looks like — there is no
    /// error code, only a null port.
    PermissionMissing,
    /// The run loop source could not be made.
    Failed,
}

/// Start watching for keystrokes.
///
/// Idempotent: a second call while running is a no-op rather than a second tap.
pub fn start() -> Result<(), TapRefusal> {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    let (tx, rx) = std::sync::mpsc::channel();

    let spawned = std::thread::Builder::new()
        .name("notewise-keystrokes".into())
        .spawn(move || {
            // SAFETY: the callback is a plain `extern "C" fn` with no captured state, no user data
            // is passed, and every pointer is checked before use. The source and port are released
            // after the loop returns, by which time no further callback can run.
            unsafe {
                let tap = CGEventTapCreate(
                    SESSION_TAP,
                    HEAD_INSERT,
                    LISTEN_ONLY,
                    KEY_DOWN_MASK,
                    on_event,
                    std::ptr::null_mut(),
                );

                if tap.is_null() {
                    // Null is all the API gives. In practice it means Input Monitoring has not been
                    // granted to this build.
                    let _ = tx.send(Err(TapRefusal::PermissionMissing));
                    RUNNING.store(false, Ordering::SeqCst);
                    return;
                }

                let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
                if source.is_null() {
                    CFRelease(tap);
                    let _ = tx.send(Err(TapRefusal::Failed));
                    RUNNING.store(false, Ordering::SeqCst);
                    return;
                }

                let run_loop = CFRunLoopGetCurrent();
                CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
                CGEventTapEnable(tap, true);

                TAP.store(tap as *mut c_void, Ordering::SeqCst);
                RUN_LOOP.store(run_loop, Ordering::SeqCst);

                let _ = tx.send(Ok(()));

                // Returns when somebody calls `stop`.
                CFRunLoopRun();

                TAP.store(std::ptr::null_mut(), Ordering::SeqCst);
                RUN_LOOP.store(std::ptr::null_mut(), Ordering::SeqCst);
                CFRelease(source);
                CFRelease(tap);
                RUNNING.store(false, Ordering::SeqCst);
            }
        });

    if spawned.is_err() {
        RUNNING.store(false, Ordering::SeqCst);
        return Err(TapRefusal::Failed);
    }

    // Bounded: a tap that cannot be created answers immediately, and one that can is running by the
    // time this returns — so a caller that gets `Ok` knows the monitor is live.
    match rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(answer) => answer,
        Err(_) => {
            RUNNING.store(false, Ordering::SeqCst);
            Err(TapRefusal::Failed)
        }
    }
}

/// Stop watching, and wait until it has.
///
/// The wait is the point. `CFRunLoopStop` only asks: the monitor thread has to unwind, release the
/// tap, and clear its flag, and until it does [`activity`] still answers `running: true`. A caller
/// that turned the feature off and immediately read back "on" would show the user a switch that
/// flipped itself, so this does not return until the answer is the true one.
///
/// Bounded, because a thread that will not come back must not hang the request that asked.
pub fn stop() {
    let run_loop = RUN_LOOP.load(Ordering::SeqCst);
    if run_loop.is_null() {
        return;
    }

    // SAFETY: the pointer was stored by the monitor thread from `CFRunLoopGetCurrent` and is
    // cleared by that thread before it exits. `CFRunLoopStop` is documented as callable from any
    // thread.
    unsafe { CFRunLoopStop(run_loop) }

    for _ in 0..100 {
        if !RUNNING.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    tracing::warn!("the keystroke monitor did not stop within half a second");
}

/// What has been typed lately, as timing.
pub fn activity() -> TypingActivity {
    let last = LAST_KEYSTROKE_MS.load(Ordering::Relaxed);

    TypingActivity {
        running: RUNNING.load(Ordering::Relaxed),
        last_keystroke_ms: (last > 0).then_some(last),
        keystrokes: KEYSTROKES.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The event mask, spelled out. A wrong bit watches the wrong events — mouse movement instead
    /// of typing, which would be both useless and considerably more alarming.
    #[test]
    fn the_mask_watches_key_presses_and_nothing_else() {
        assert_eq!(KEY_DOWN_MASK, 1 << KEY_DOWN);
        assert_eq!(KEY_DOWN_MASK.count_ones(), 1, "one event type, not a range");
    }

    /// The property the OS enforces for us.
    #[test]
    fn the_tap_is_listen_only() {
        assert_eq!(LISTEN_ONLY, 1, "kCGEventTapOptionListenOnly");
        assert_ne!(
            LISTEN_ONLY, 0,
            "0 is kCGEventTapOptionDefault, which can modify events"
        );
    }

    /// Without the grant, starting is refused rather than appearing to work. A test binary never
    /// holds Input Monitoring, so this is the path every development build takes.
    #[test]
    fn starting_without_the_grant_is_refused() {
        match start() {
            Err(TapRefusal::PermissionMissing) => {
                assert!(!activity().running, "nothing should be left running");
            }
            // A developer who has granted Input Monitoring to their terminal lands here. Clean up,
            // because leaving a tap running would affect every later test in the process.
            Ok(()) => {
                assert!(activity().running);
                stop();
                assert!(!activity().running, "stop must mean stopped");
            }
            Err(other) => panic!("unexpected refusal: {other:?}"),
        }
    }

    /// Nothing has been typed into a test binary, and the count is a count rather than a buffer.
    #[test]
    fn activity_reports_timing_and_not_content() {
        let activity = activity();
        // The assertion that matters is structural: there is no field here that could hold a key.
        assert!(activity.keystrokes < u64::MAX);
        let _ = activity.last_keystroke_ms;
    }

    #[test]
    #[ignore = "needs the Input Monitoring grant and a person typing"]
    fn typing_moves_the_timestamp() {
        start().expect("the grant is held");
        let before = activity();

        std::thread::sleep(std::time::Duration::from_secs(5));

        let after = activity();
        assert!(after.keystrokes > before.keystrokes);
        stop();
    }
}
