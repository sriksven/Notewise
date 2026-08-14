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
///
/// This used to answer `NotRequested` for anything obtainable, without asking the system at
/// all — which made pressing Enable look like it did nothing. The grant went through, and then
/// the readiness this feeds re-read `NotRequested` and redrew the same button. Anything that
/// reports state has to read that state.
pub fn permission_status(kind: CaptureKind) -> PermissionStatus {
    match unavailable_reason(kind) {
        Some(reason) => PermissionStatus::Unavailable(reason),
        None => recorded_status(kind),
    }
}

/// What the OS has already decided, without asking it to decide anything.
///
/// macOS keeps this in TCC and will answer for a capability the user granted in a previous
/// launch, or revoked in System Settings since. Reading it is the only way for the app to agree
/// with the Privacy pane.
#[cfg(all(target_os = "macos", feature = "os-capture"))]
fn recorded_status(kind: CaptureKind) -> PermissionStatus {
    use notewise_macos_permissions::Authorization;

    // Microphone is the AVFoundation audio capability, and the only one this can answer for.
    // System audio is ScreenCaptureKit, whose authorization is not an `AVCaptureDevice` media
    // type; `unavailable_reason` has already handled the unsigned case, and for a signed bundle
    // the honest answer is that we have not asked.
    if !matches!(kind, CaptureKind::Microphone) {
        return PermissionStatus::NotRequested;
    }

    match notewise_macos_permissions::microphone() {
        Authorization::Granted => PermissionStatus::Granted,
        Authorization::Denied => PermissionStatus::Denied,
        Authorization::NotDetermined | Authorization::Unknown => PermissionStatus::NotRequested,
    }
}

/// Everywhere else there is nothing to read, so the honest answer is that we have not asked.
#[cfg(not(all(target_os = "macos", feature = "os-capture")))]
fn recorded_status(_kind: CaptureKind) -> PermissionStatus {
    PermissionStatus::NotRequested
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
            Some(backend) => backend
                .unavailable_reason()
                .map(str::to_string)
                .or_else(|| outside_app_bundle_reason(other)),
        },
    }
}

/// Why a loose binary cannot hold a screen-recording grant.
///
/// macOS ties the Screen Recording permission to an application bundle. Run as a bare
/// executable — `cargo run`, or the binary out of `target/` — there is nothing for the grant to
/// attach to, so ScreenCaptureKit refuses and the OS reports a denial.
///
/// Without this the refusal surfaces as "Declined. Grant it in System Settings, then re-check",
/// which sends the user to a pane where the app does not appear and cannot be added. The cause
/// is the build, not their answer, and saying so is the difference between a limitation and a
/// wild goose chase.
#[cfg(target_os = "macos")]
fn outside_app_bundle_reason(kind: CaptureKind) -> Option<String> {
    if !matches!(kind, CaptureKind::SystemAudio) {
        return None;
    }

    let in_bundle = std::env::current_exe()
        .ok()
        .map(|path| path.to_string_lossy().contains(".app/Contents/MacOS/"))
        .unwrap_or(false);

    (!in_bundle).then(|| {
        "system audio needs macOS to recognise this as an application — it is running as a \
         loose binary, which cannot be granted screen recording"
            .to_string()
    })
}

#[cfg(not(target_os = "macos"))]
fn outside_app_bundle_reason(_kind: CaptureKind) -> Option<String> {
    None
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
        // A test binary is a loose executable, so on macOS this is also the "not in a bundle"
        // path — which is the same answer a developer running `cargo run` gets, and it must
        // name the build rather than blame the user for declining.
        match permission_status(CaptureKind::SystemAudio) {
            PermissionStatus::Unavailable(reason) => {
                assert!(!reason.is_empty(), "an unavailable capability must say why");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    /// The status must come from the operating system, not from a constant.
    ///
    /// This test previously asserted `NotRequested` unconditionally, which is exactly what the
    /// code did — and it was wrong. A machine that had already granted the microphone was told
    /// it had not, so pressing Enable granted the permission, the readiness re-read
    /// `NotRequested`, and the button redrew unchanged. The bug looked like a dead button and
    /// had a passing test over it.
    ///
    /// The grant on the machine running this cannot be controlled, so what is pinned is that
    /// the answer is a real reading: never `Unavailable` when capture is compiled in, and
    /// stable across calls — a status that changed between two reads would mean something here
    /// is prompting.
    #[test]
    #[cfg(feature = "os-capture")]
    fn microphone_status_is_read_from_the_system() {
        let first = permission_status(CaptureKind::Microphone);
        let second = permission_status(CaptureKind::Microphone);

        assert_eq!(first, second, "reading the status must not change it");
        assert!(
            matches!(
                first,
                PermissionStatus::NotRequested
                    | PermissionStatus::Granted
                    | PermissionStatus::Denied
            ),
            "the microphone is obtainable wherever cpal runs, got {first:?}"
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
