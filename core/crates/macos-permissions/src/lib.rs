//! What macOS has already decided about privacy-gated capabilities.
//!
//! Reads TCC through AVFoundation. Reading is not asking: this never raises a dialog, so it is
//! safe to call while a window is painting, which is the whole reason it exists separately from
//! the code that requests a permission.
//!
//! # Why this is its own crate
//!
//! Every other crate in the engine sets `#![forbid(unsafe_code)]`. Asking the Objective-C
//! runtime for an authorization status needs `unsafe`, and `forbid` cannot be relaxed locally —
//! by design. Rather than downgrade that guarantee across a crate that does real audio work,
//! the two unsafe calls live here, in a file short enough to audit in one sitting.
//!
//! # What the unsafe is
//!
//! `AVMediaTypeAudio` is a framework string constant, and `authorizationStatusForMediaType:` is
//! a pure read of process state. Neither takes a pointer from us, allocates, or mutates
//! anything. objc2 marks them `unsafe` because it marks nearly all Objective-C bindings so; the
//! obligation being discharged is that the framework symbols are the ones they claim to be,
//! which linking against AVFoundation establishes.

/// What the system will allow, as it currently stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authorization {
    /// Never asked. The caller may prompt.
    NotDetermined,
    Granted,
    /// Refused, or forbidden by policy on a managed device. Prompting again will not help —
    /// only the Privacy pane in System Settings can change it.
    Denied,
    /// Not answerable here: another platform, or a capability AVFoundation does not track.
    Unknown,
}

/// Whether this process may use the microphone, without asking for it.
#[cfg(target_os = "macos")]
pub fn microphone() -> Authorization {
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

    // The constant is loaded from the framework at runtime. A null would mean AVFoundation is
    // not loaded, in which case there is nothing to report rather than something to assume.
    let Some(media_type) = (unsafe { AVMediaTypeAudio }) else {
        return Authorization::Unknown;
    };

    match unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) } {
        AVAuthorizationStatus::Authorized => Authorization::Granted,
        AVAuthorizationStatus::Denied | AVAuthorizationStatus::Restricted => Authorization::Denied,
        AVAuthorizationStatus::NotDetermined => Authorization::NotDetermined,
        // A status a future macOS adds. Reporting it as unknown keeps the UI from claiming a
        // grant this crate does not understand.
        _ => Authorization::Unknown,
    }
}

#[cfg(not(target_os = "macos"))]
pub fn microphone() -> Authorization {
    Authorization::Unknown
}

/// Ask for the microphone, raising the system dialog if it has never been asked.
///
/// This is the call that puts the app in the Privacy pane's Microphone list. Opening an audio
/// stream is *not* — on macOS an input stream opens happily without a grant and delivers
/// silence, so treating a successful open as permission reports a grant that does not exist and
/// records nothing.
///
/// Blocks until the user answers. The dialog is modal to them, not to the app, so a caller must
/// be on a thread that can afford to wait.
#[cfg(target_os = "macos")]
pub fn request_microphone() -> Authorization {
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::StackBlock;
    use objc2::runtime::Bool;
    use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeAudio};

    // Already answered: `requestAccess` would return the standing answer without a dialog, but
    // asking outright is clearer and avoids waiting on a callback that fires immediately.
    match microphone() {
        Authorization::NotDetermined => {}
        settled => return settled,
    }

    let Some(media_type) = (unsafe { AVMediaTypeAudio }) else {
        return Authorization::Unknown;
    };

    let (tx, rx) = mpsc::channel();
    let handler = StackBlock::new(move |granted: Bool| {
        // The receiver may already be gone if the wait below timed out. Nothing to do about a
        // late answer except drop it.
        let _ = tx.send(granted.as_bool());
    });

    unsafe {
        AVCaptureDevice::requestAccessForMediaType_completionHandler(media_type, &handler);
    }

    // Generous, because the bound is a person noticing a dialog. A timeout here is not a denial
    // — it means we still do not know, and the next status read will say so.
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(true) => Authorization::Granted,
        Ok(false) => Authorization::Denied,
        Err(_) => microphone(),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn request_microphone() -> Authorization {
    Authorization::Unknown
}

#[cfg(target_os = "macos")]
mod screen {
    // CoreGraphics, not AVFoundation: screen recording is not an `AVCaptureDevice` media type,
    // and this is the only public call that reads the grant without asking for it.
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        /// Whether this process currently holds the screen-recording grant. Does not prompt.
        fn CGPreflightScreenCaptureAccess() -> bool;

        /// Prompt for the screen-recording grant, registering this app with TCC.
        fn CGRequestScreenCaptureAccess() -> bool;
    }

    pub fn granted() -> bool {
        // The function takes no arguments, returns a plain bool, and only reads TCC state for
        // the calling process.
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    pub fn request() -> bool {
        // Same shape: no arguments, plain bool. The side effect is TCC's, not memory's.
        unsafe { CGRequestScreenCaptureAccess() }
    }
}

/// Whether this process may capture the screen — which on macOS is what gates system audio.
///
/// Only two answers, and that is the API's limit rather than a simplification: preflight
/// reports `false` both for "never asked" and for "asked and refused". Distinguishing them
/// needs the memory of having asked, which belongs to whoever did the asking.
#[cfg(target_os = "macos")]
pub fn screen_recording_granted() -> bool {
    screen::granted()
}

#[cfg(not(target_os = "macos"))]
pub fn screen_recording_granted() -> bool {
    false
}

/// Ask for screen recording, which is what gates system audio.
///
/// This is the call that makes the app appear under Privacy & Security → Screen & System Audio
/// Recording. Attempting a ScreenCaptureKit stream is not: it fails immediately when the grant
/// is missing, without registering anything, so the app never shows up in the list the error
/// message tells the user to go to.
///
/// Returns the grant as it stands after asking. macOS commonly answers `false` even when the
/// user says yes, because the grant applies from the next launch — so a caller must treat
/// `false` as "not yet", not as a refusal to be recorded forever.
#[cfg(target_os = "macos")]
pub fn request_screen_recording() -> bool {
    screen::request()
}

#[cfg(not(target_os = "macos"))]
pub fn request_screen_recording() -> bool {
    false
}

#[cfg(target_os = "macos")]
mod signature {
    use std::ffi::c_void;

    type OSStatus = i32;

    #[link(name = "Security", kind = "framework")]
    extern "C" {
        /// A handle on the running process's own code signature.
        fn SecCodeCopySelf(flags: u32, out: *mut *const c_void) -> OSStatus;

        /// Signing details as a dictionary. `kSecCSSigningInformation` is required for the
        /// team identifier to be included.
        fn SecCodeCopySigningInformation(
            code: *const c_void,
            flags: u32,
            information: *mut *const c_void,
        ) -> OSStatus;

        static kSecCodeInfoTeamIdentifier: *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
        fn CFRelease(cf: *const c_void);
    }

    /// `kSecCSSigningInformation`. Without it the dictionary omits the team identifier.
    const SIGNING_INFORMATION: u32 = 2;

    /// Whether this build carries a team identifier — the mark of a Developer ID or App Store
    /// signature, as opposed to ad-hoc or unsigned.
    ///
    /// Conservative on failure: an error reading our own signature answers `true`, so a build
    /// this cannot inspect is never told it lacks a capability it may well have.
    pub fn has_team_identifier() -> bool {
        // SAFETY: `SecCodeCopySelf` writes one owned reference on success, which is released
        // below. The dictionary lookup borrows — `CFDictionaryGetValue` returns an unowned
        // pointer — so only the two copies obtained here are released, and neither is used
        // afterwards. All pointers are checked before use.
        unsafe {
            let mut code: *const c_void = std::ptr::null();
            if SecCodeCopySelf(0, &mut code) != 0 || code.is_null() {
                return true;
            }

            let mut info: *const c_void = std::ptr::null();
            let status = SecCodeCopySigningInformation(code, SIGNING_INFORMATION, &mut info);
            CFRelease(code);

            if status != 0 || info.is_null() {
                return true;
            }

            let team = CFDictionaryGetValue(info, kSecCodeInfoTeamIdentifier);
            CFRelease(info);

            !team.is_null()
        }
    }
}

/// Whether macOS will let this build hold a screen-recording grant at all.
///
/// It will not for an ad-hoc or unsigned build. The permission is attached to a verifiable app
/// identity, and without one `CGRequestScreenCaptureAccess` returns false immediately, raises no
/// dialog, and writes nothing to TCC — so the app never appears in the Privacy pane, and there
/// is no setting anywhere that adds it.
///
/// This exists so the product can say that, instead of asking forever for something the build
/// cannot obtain. The microphone is a lower bar and is unaffected: an ad-hoc build holds that
/// grant perfectly well, which is why one of the two permissions works and the other never does.
#[cfg(target_os = "macos")]
pub fn can_hold_screen_recording() -> bool {
    signature::has_team_identifier()
}

#[cfg(not(target_os = "macos"))]
pub fn can_hold_screen_recording() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test binary is ad-hoc signed, so this is the case the product hits on every
    /// development build: no team identifier, therefore no screen recording, therefore the
    /// capability must be reported unavailable rather than demanded forever.
    #[test]
    #[cfg(target_os = "macos")]
    fn an_ad_hoc_build_cannot_hold_screen_recording() {
        assert!(
            !can_hold_screen_recording(),
            "cargo's test binary is not Developer ID signed, so this must be false"
        );
    }

    /// Calling it must not prompt, panic, or block — this runs on every readiness check, on a
    /// machine whose actual grant we cannot control from a test.
    #[test]
    fn reading_the_status_is_harmless_and_terminates() {
        let status = microphone();
        assert!(matches!(
            status,
            Authorization::NotDetermined
                | Authorization::Granted
                | Authorization::Denied
                | Authorization::Unknown
        ));
    }
}
