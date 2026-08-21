//! What can go wrong reaching outside the app, and what to tell the user about it.
//!
//! Every variant carries the thing a person needs in order to act. A permission error that does not
//! say which pane to open is a dead end, and that is the class of bug the capture crate learned
//! about the hard way — see `63f6f6d fix(capture): stop demanding a permission this build cannot be
//! given`.

use crate::HotkeyError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OsInputError {
    /// A grant is missing, and it can be given.
    #[error("{what} needs a permission Notewise does not have. {how_to_grant}")]
    PermissionRequired { what: String, how_to_grant: String },

    /// This build or this platform cannot do it at all.
    ///
    /// The same shape `audio-capture` returns, and for the same reason: a user asked to grant a
    /// permission the build is incapable of holding will try forever and get nowhere.
    #[error("{what} is not available: {reason}")]
    Unsupported { what: String, reason: String },

    /// The OS refused a registration, which usually means another application holds it.
    ///
    /// Surfaced when the binding is set, not when it is pressed. A hotkey that silently does
    /// nothing is indistinguishable from a broken feature.
    #[error("the system refused '{binding}': another application may already use it")]
    HotkeyUnavailable { binding: String },

    /// Two Notewise features want the same combination.
    #[error("'{a}' and '{b}' cannot both use that combination")]
    HotkeyConflict { a: String, b: String },

    /// Nothing was inserted, and the reason is fit to show a user.
    #[error("{reason}")]
    InsertionRefused { reason: String },

    /// The platform layer failed in a way that is nobody's fault to fix.
    #[error("{0}")]
    Platform(String),
}

impl From<HotkeyError> for OsInputError {
    fn from(error: HotkeyError) -> Self {
        match error {
            HotkeyError::Conflict { binding, holder } => OsInputError::HotkeyConflict {
                a: binding,
                b: holder,
            },
            HotkeyError::RefusedByOs { binding } => OsInputError::HotkeyUnavailable { binding },
            HotkeyError::Malformed(message) => OsInputError::Platform(message),
        }
    }
}

pub type Result<T> = std::result::Result<T, OsInputError>;

/// The error for a capability this platform has no equivalent of.
///
/// Linux, always. The design's non-goals say so outright: AnythingLLM's own Linux support for these
/// is degraded or absent and the underlying APIs have no portable equivalent, so the honest answer
/// is a refusal with a reason rather than a feature that appears to exist.
pub fn unsupported_platform(what: &str) -> OsInputError {
    OsInputError::Unsupported {
        what: what.to_string(),
        reason: format!(
            "Notewise can only do this on macOS, and this is {}",
            std::env::consts::OS
        ),
    }
}

/// The error for a capability this *build* cannot do, though the platform can.
pub fn not_compiled_in(what: &str) -> OsInputError {
    OsInputError::Unsupported {
        what: what.to_string(),
        reason: "this build was made without the desktop assistant, which needs the 'os-input' \
                 feature"
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A permission error that does not name the pane is a dead end.
    #[test]
    fn a_permission_error_says_how_to_fix_it() {
        let error = OsInputError::PermissionRequired {
            what: "Typing into other apps".into(),
            how_to_grant: "Open System Settings → Privacy & Security → Accessibility.".into(),
        };
        let rendered = error.to_string();
        assert!(rendered.contains("System Settings"), "{rendered}");
        assert!(rendered.contains("Accessibility"), "{rendered}");
    }

    /// A user asked to grant something the build cannot hold will try forever.
    #[test]
    fn an_unsupported_capability_says_why_rather_than_asking_for_a_permission() {
        let error = not_compiled_in("Dictation");
        let rendered = error.to_string();
        assert!(rendered.contains("os-input"), "{rendered}");
        assert!(
            !rendered.contains("System Settings"),
            "there is no setting that fixes a missing feature flag: {rendered}"
        );
    }

    #[test]
    fn an_unsupported_platform_names_the_platform() {
        let rendered = unsupported_platform("Global hotkeys").to_string();
        assert!(rendered.contains(std::env::consts::OS), "{rendered}");
        assert!(rendered.contains("macOS"), "{rendered}");
    }

    /// The two kinds of hotkey failure have different fixes — one is a setting in this app, the
    /// other is a setting in somebody else's — so they must not collapse into one message.
    #[test]
    fn the_two_hotkey_failures_stay_distinct() {
        let ours: OsInputError = HotkeyError::Conflict {
            binding: "super+shift+d".into(),
            holder: "dictation".into(),
        }
        .into();
        assert!(matches!(ours, OsInputError::HotkeyConflict { .. }));

        let theirs: OsInputError = HotkeyError::RefusedByOs {
            binding: "super+space".into(),
        }
        .into();
        assert!(matches!(theirs, OsInputError::HotkeyUnavailable { .. }));
        assert!(theirs.to_string().contains("another application"));
    }
}
