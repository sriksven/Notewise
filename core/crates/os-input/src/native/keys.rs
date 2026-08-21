//! Synthesising a keystroke, which is how the clipboard tier actually pastes.
//!
//! # This needs the Accessibility grant too
//!
//! `CGEventPost` is not a read. Without the grant macOS *silently drops* the event — no error, no
//! log, nothing pasted. That is the exact failure mode the three-tier design exists to prevent, so
//! the platform layer reports `accepts_paste: false` when the grant is absent rather than letting
//! the tier machine choose a path that cannot work. The check lives in [`super::MacTarget`].
//!
//! # Why the paste is followed by a wait
//!
//! The keystroke is delivered asynchronously. The target application reads the clipboard when it
//! gets round to handling the event, which is after this function would otherwise have returned —
//! and the caller's next move is to put the old clipboard back. Restoring too early means the user
//! gets their own previous clipboard pasted instead of their dictation.

use std::ffi::c_void;
use std::time::Duration;

use crate::keycode::KEY_V;

type CGEventRef = *const c_void;
type CGEventSourceRef = *const c_void;

/// `kCGEventSourceStateHIDSystemState` — the same source a physical keyboard uses.
const HID_SYSTEM_STATE: i32 = 1;
/// `kCGHIDEventTap` — post at the lowest point, so every application sees it.
const HID_EVENT_TAP: u32 = 0;
/// `kCGEventFlagMaskCommand`.
const FLAG_COMMAND: u64 = 0x0010_0000;

/// How long to let a paste land before the clipboard is put back.
///
/// Chosen to be longer than a local application needs and short enough not to feel like a hang. Too
/// short is the bug that pastes the user's previous clipboard; too long is a visible stall on every
/// dictation.
pub const PASTE_SETTLE: Duration = Duration::from_millis(150);

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSourceCreate(state_id: i32) -> CGEventSourceRef;
    fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        keycode: u16,
        key_down: bool,
    ) -> CGEventRef;
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CFRelease(cf: *const c_void);
}

/// Press and release one key with the command modifier held.
///
/// Both halves are required: an application that sees a key-down without the matching key-up can
/// leave the modifier stuck, and a stuck command key makes the next thing the user types do
/// something they did not ask for.
fn tap_with_command(keycode: u16) -> Result<(), String> {
    // SAFETY: every pointer is checked before use and every created object is released on every
    // path, including the early returns. `CGEventSetFlags` and `CGEventPost` borrow.
    unsafe {
        let source = CGEventSourceCreate(HID_SYSTEM_STATE);
        if source.is_null() {
            return Err("could not open an event source".into());
        }

        let down = CGEventCreateKeyboardEvent(source, keycode, true);
        let up = CGEventCreateKeyboardEvent(source, keycode, false);

        if down.is_null() || up.is_null() {
            if !down.is_null() {
                CFRelease(down);
            }
            if !up.is_null() {
                CFRelease(up);
            }
            CFRelease(source);
            return Err("could not build the keystroke".into());
        }

        CGEventSetFlags(down, FLAG_COMMAND);
        CGEventSetFlags(up, FLAG_COMMAND);

        CGEventPost(HID_EVENT_TAP, down);
        CGEventPost(HID_EVENT_TAP, up);

        CFRelease(down);
        CFRelease(up);
        CFRelease(source);
    }

    Ok(())
}

/// Synthesise the paste, then wait for it to land.
pub fn paste() -> Result<(), String> {
    tap_with_command(KEY_V)?;
    std::thread::sleep(PASTE_SETTLE);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Too short pastes the user's previous clipboard; too long is a visible stall. Pinned so a
    /// casual edit has to argue with a test.
    #[test]
    fn the_settle_delay_is_long_enough_to_be_useful_and_short_enough_to_be_invisible() {
        assert!(PASTE_SETTLE >= Duration::from_millis(100));
        assert!(PASTE_SETTLE <= Duration::from_millis(400));
    }

    /// Building and posting the events must not crash even without the grant — without it the
    /// system drops them, which is a no-op rather than a fault. Runs, because that no-op is exactly
    /// what has to be safe: this code path is reached on every development build.
    #[test]
    fn synthesising_a_keystroke_is_safe_without_the_grant() {
        // The event is dropped by the window server rather than delivered. What is being checked is
        // that this crate's own allocation and release discipline holds on that path.
        for _ in 0..20 {
            tap_with_command(KEY_V).expect("the events can always be built");
        }
    }

    #[test]
    #[ignore = "needs the Accessibility grant and a focused text field in another application"]
    fn a_synthesised_paste_lands_in_another_app() {
        paste().expect("the paste is posted");
    }
}
