//! Whether this machine will actually let us capture.
//!
//! Every function here touches the thing it reports on. A permission check that infers a
//! grant from device enumeration is worse than no check at all: devices enumerate fine while
//! the grant is denied, so the user is shown a green tick and then records silence.

use crate::{CaptureKind, OsBackend};

/// What the OS will let us do, as far as we have actually asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionStatus {
    /// Obtainable, but nothing has prompted yet. Not a guess — a statement that we have not
    /// asked, so the UI can offer the button instead of inventing an answer.
    NotRequested,
    Granted,
    Denied,
    /// Cannot be granted on this build or platform, with the reason. Callers must not gate on
    /// this: there is no action a user could take.
    Unavailable(String),
}

/// Report a capability without prompting.
///
/// Safe to call on load. It never opens a device, because doing so would raise an OS dialog
/// before the user pressed anything.
pub fn permission_status(kind: CaptureKind) -> PermissionStatus {
    match unavailable_reason(kind) {
        Some(reason) => PermissionStatus::Unavailable(reason),
        None => PermissionStatus::NotRequested,
    }
}

/// Ask for a capability, prompting if the OS decides to.
///
/// Blocking: it opens an audio device. Callers on an async runtime must use `spawn_blocking`.
pub fn request_permission(kind: CaptureKind) -> PermissionStatus {
    match unavailable_reason(kind) {
        Some(reason) => PermissionStatus::Unavailable(reason),
        None => probe(kind),
    }
}

/// Why `kind` cannot be granted here, or `None` when it can.
fn unavailable_reason(kind: CaptureKind) -> Option<String> {
    if !cfg!(feature = "os-capture") {
        return Some(
            "this build has no capture support (built without the 'os-capture' feature)".into(),
        );
    }

    match kind {
        // The microphone path is `cpal`, not `OsBackend` — it needs no signed bundle, and is
        // the one capability obtainable everywhere `cpal` runs.
        CaptureKind::Microphone => None,
        other => match OsBackend::for_host(other) {
            None => Some(format!(
                "{other:?} capture is not supported on {}",
                std::env::consts::OS
            )),
            Some(backend) => backend.unavailable_reason().map(str::to_string),
        },
    }
}

/// Open the device briefly and map the outcome.
#[cfg(feature = "os-capture")]
fn probe(kind: CaptureKind) -> PermissionStatus {
    use crate::{CaptureConfig, CaptureError, MicrophoneSource};

    match kind {
        CaptureKind::Microphone => {
            // Opening and immediately dropping is the whole probe: on macOS this is what
            // raises the TCC dialog on first call, and what returns a permission error once
            // the user has declined.
            let config = CaptureConfig {
                kind: CaptureKind::Microphone,
                ..CaptureConfig::default()
            };

            match MicrophoneSource::open(&config) {
                Ok(source) => {
                    drop(source);
                    PermissionStatus::Granted
                }
                Err(CaptureError::PermissionDenied { .. }) => PermissionStatus::Denied,
                Err(e) if is_permission_error(&e.to_string()) => PermissionStatus::Denied,
                // A missing device is not a denied permission, and sending a user to System
                // Settings over an unplugged microphone would send them nowhere useful.
                Err(e) => PermissionStatus::Unavailable(e.to_string()),
            }
        }
        // Opening the tap is the probe, for the same reason as the microphone: on macOS the
        // first attempt is what raises the Screen Recording dialog, and a refused grant is
        // what comes back once the user has declined.
        //
        // Note this is a *stronger* check than `CGPreflightScreenCaptureAccess`, which reports
        // the TCC record without proving a stream can actually start.
        #[cfg(target_os = "macos")]
        CaptureKind::SystemAudio => {
            use crate::system_audio::SystemAudioSource;

            let config = CaptureConfig {
                kind: CaptureKind::SystemAudio,
                ..CaptureConfig::default()
            };

            match SystemAudioSource::open(&config) {
                Ok(source) => {
                    drop(source);
                    PermissionStatus::Granted
                }
                Err(CaptureError::PermissionDenied { .. }) => PermissionStatus::Denied,
                Err(e) if is_permission_error(&e.to_string()) => PermissionStatus::Denied,
                Err(e) => PermissionStatus::Unavailable(e.to_string()),
            }
        }
        other => PermissionStatus::Unavailable(format!("no probe for {other:?}")),
    }
}

#[cfg(not(feature = "os-capture"))]
fn probe(_kind: CaptureKind) -> PermissionStatus {
    PermissionStatus::Unavailable("this build has no capture support".into())
}

/// Whether an OS error string describes a refused permission.
///
/// String matching because `cpal` surfaces the platform's own message rather than a typed
/// error. Shared with `microphone.rs` so the probe and the capture path cannot come to
/// disagree about what a denial looks like.
// Called only from the os-capture path; its tests are not feature-gated.
#[cfg_attr(not(feature = "os-capture"), allow(dead_code))]
pub(crate) fn is_permission_error(message: &str) -> bool {
    let message = message.to_lowercase();
    message.contains("permission") || message.contains("denied")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A build that cannot capture system audio must say so rather than claim to be merely
    /// un-asked: a caller that cannot tell those apart would send a user to grant a permission
    /// that would not help.
    ///
    /// Where the backend does exist, "not asked yet" becomes the honest answer, because
    /// nothing has raised the Screen Recording prompt.
    #[test]
    fn system_audio_reports_a_reason_when_it_cannot_be_had() {
        let status = permission_status(CaptureKind::SystemAudio);

        if cfg!(all(target_os = "macos", feature = "os-capture")) {
            assert_eq!(
                status,
                PermissionStatus::NotRequested,
                "system audio is implemented here; nothing has prompted yet"
            );
        } else {
            match status {
                PermissionStatus::Unavailable(reason) => {
                    assert!(!reason.is_empty(), "an unavailable capability must say why");
                }
                other => panic!("expected Unavailable, got {other:?}"),
            }
        }
    }

    /// Nothing has prompted yet, so the honest answer is `NotRequested` — not a guess derived
    /// from whether devices happen to enumerate.
    #[test]
    #[cfg(feature = "os-capture")]
    fn microphone_starts_out_unrequested() {
        assert_eq!(
            permission_status(CaptureKind::Microphone),
            PermissionStatus::NotRequested
        );
    }

    #[test]
    #[cfg(not(feature = "os-capture"))]
    fn microphone_is_unavailable_without_the_capture_feature() {
        assert!(matches!(
            permission_status(CaptureKind::Microphone),
            PermissionStatus::Unavailable(_)
        ));
    }

    #[test]
    fn permission_error_strings_are_recognised() {
        assert!(is_permission_error("Permission denied"));
        assert!(is_permission_error("the user DENIED access"));
        assert!(!is_permission_error("device disconnected"));
    }

    /// Requires a real device and, on macOS, a TCC grant against a signed bundle. Neither
    /// exists in CI, and a green run must not imply this was verified.
    #[test]
    #[ignore = "requires a microphone and an OS permission grant"]
    #[cfg(feature = "os-capture")]
    fn requesting_the_microphone_reaches_a_terminal_answer() {
        assert!(matches!(
            request_permission(CaptureKind::Microphone),
            PermissionStatus::Granted | PermissionStatus::Denied
        ));
    }
}
