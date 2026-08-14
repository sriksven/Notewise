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

#[cfg(target_os = "macos")]
mod screen {
    // CoreGraphics, not AVFoundation: screen recording is not an `AVCaptureDevice` media type,
    // and this is the only public call that reads the grant without asking for it.
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        /// Whether this process currently holds the screen-recording grant. Does not prompt.
        fn CGPreflightScreenCaptureAccess() -> bool;
    }

    pub fn granted() -> bool {
        // The function takes no arguments, returns a plain bool, and only reads TCC state for
        // the calling process.
        unsafe { CGPreflightScreenCaptureAccess() }
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

#[cfg(test)]
mod tests {
    use super::*;

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
