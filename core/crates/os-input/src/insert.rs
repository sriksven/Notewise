//! Putting text where the cursor is.
//!
//! # Three tiers, and the order is the point
//!
//! Set the focused element's value through the accessibility API; failing that, synthesise a paste
//! with clipboard save-and-restore; failing that, refuse and say so.
//!
//! Clipboard paste is a real technique and a real hazard. It clobbers whatever the user had copied,
//! and restoring it races with anything else reading the clipboard. So it is second, never first, and
//! never silent — a caller can see which tier ran and tell the user their clipboard was borrowed.
//!
//! # Why refusing is a supported outcome
//!
//! Some applications accept neither. A feature that inserts text into the wrong field is worse than
//! one that says it cannot, because the first is a mistake the user has to find and undo somewhere
//! they were not looking.

use serde::{Deserialize, Serialize};

/// How the text got there, or why it did not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Insertion {
    /// Written straight into the focused element. Nothing else on the machine was touched.
    Accessibility,
    /// Pasted. The clipboard was borrowed and put back.
    ///
    /// Carries whether the restore succeeded, because a user whose clipboard was silently replaced
    /// deserves to be told rather than discovering it at their next paste.
    Clipboard { clipboard_restored: bool },
    /// Nothing was inserted.
    Refused { reason: String },
}

impl Insertion {
    pub fn inserted(&self) -> bool {
        !matches!(self, Insertion::Refused { .. })
    }
}

/// What a target application will accept.
///
/// Separated from the doing so the tier choice is testable without a focused window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetCapabilities {
    /// The focused element exposes a settable value through the accessibility API.
    pub accessibility_writable: bool,
    /// A synthesised paste is likely to land.
    pub accepts_paste: bool,
}

impl TargetCapabilities {
    /// Nothing works. The honest default when a target has not been inspected.
    pub fn unknown() -> Self {
        Self {
            accessibility_writable: false,
            accepts_paste: false,
        }
    }
}

/// Which tier to attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Accessibility,
    Clipboard,
    Refuse,
}

/// Whether the user has allowed the Accessibility grant.
///
/// A separate input from the target's capabilities: an app whose field is writable is still
/// unwritable without the permission, and the two produce different messages — one is a setting the
/// user can change, the other is an application that will never work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessibilityGrant {
    Granted,
    Denied,
    /// Never asked. Treated as denied for choosing a tier, because acting on a permission that has
    /// not been given is what a prompt exists to prevent.
    Unknown,
}

impl AccessibilityGrant {
    fn usable(&self) -> bool {
        matches!(self, AccessibilityGrant::Granted)
    }
}

/// Pick a tier.
///
/// Pure, so every combination is enumerable in a test rather than discovered against a real window.
pub fn choose_tier(grant: AccessibilityGrant, target: TargetCapabilities) -> Tier {
    if grant.usable() && target.accessibility_writable {
        return Tier::Accessibility;
    }
    if target.accepts_paste {
        return Tier::Clipboard;
    }
    Tier::Refuse
}

/// Why a refusal happened, in words a user can act on.
///
/// The distinction matters: a missing grant is a checkbox in System Settings, and an application
/// that accepts nothing is not.
pub fn refusal_reason(grant: AccessibilityGrant, target: TargetCapabilities) -> String {
    if !grant.usable() && target.accessibility_writable {
        return "Notewise needs the Accessibility permission to type into other apps.".into();
    }
    if !target.accepts_paste && !target.accessibility_writable {
        return "This application does not accept text from other apps.".into();
    }
    "There is nowhere to put the text — no editable field is focused.".into()
}

/// What a caller should tell the user afterwards, if anything.
///
/// Empty for the accessibility path, because nothing else on the machine was touched. A borrowed
/// clipboard is worth a sentence; a clipboard that could not be put back is worth a warning.
pub fn aftermath(outcome: &Insertion) -> Option<String> {
    match outcome {
        Insertion::Accessibility => None,
        Insertion::Clipboard {
            clipboard_restored: true,
        } => Some("Pasted, and your clipboard was put back.".into()),
        Insertion::Clipboard {
            clipboard_restored: false,
        } => Some("Pasted, but your clipboard could not be restored.".into()),
        Insertion::Refused { reason } => Some(reason.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(writable: bool, paste: bool) -> TargetCapabilities {
        TargetCapabilities {
            accessibility_writable: writable,
            accepts_paste: paste,
        }
    }

    /// Nothing else on the machine is touched, so it goes first.
    #[test]
    fn accessibility_is_preferred_when_it_is_available() {
        assert_eq!(
            choose_tier(AccessibilityGrant::Granted, target(true, true)),
            Tier::Accessibility
        );
    }

    /// The hazard is real, so it is the fallback rather than the default.
    #[test]
    fn the_clipboard_is_only_used_when_accessibility_cannot_be() {
        assert_eq!(
            choose_tier(AccessibilityGrant::Granted, target(false, true)),
            Tier::Clipboard
        );
        assert_eq!(
            choose_tier(AccessibilityGrant::Denied, target(true, true)),
            Tier::Clipboard
        );
    }

    /// Acting on a permission that has not been given is what a prompt exists to prevent.
    #[test]
    fn an_unasked_permission_is_treated_as_denied() {
        assert_eq!(
            choose_tier(AccessibilityGrant::Unknown, target(true, true)),
            Tier::Clipboard
        );
        assert_eq!(
            choose_tier(AccessibilityGrant::Unknown, target(true, false)),
            Tier::Refuse
        );
    }

    /// Inserting into the wrong field is worse than saying it cannot.
    #[test]
    fn refusing_is_a_supported_outcome() {
        assert_eq!(
            choose_tier(AccessibilityGrant::Granted, target(false, false)),
            Tier::Refuse
        );
        assert_eq!(
            choose_tier(AccessibilityGrant::Denied, TargetCapabilities::unknown()),
            Tier::Refuse
        );
    }

    /// Every combination, so none is discovered against a real window.
    #[test]
    fn every_combination_has_a_defined_tier() {
        for grant in [
            AccessibilityGrant::Granted,
            AccessibilityGrant::Denied,
            AccessibilityGrant::Unknown,
        ] {
            for writable in [true, false] {
                for paste in [true, false] {
                    let tier = choose_tier(grant, target(writable, paste));
                    let expected = match (grant.usable() && writable, paste) {
                        (true, _) => Tier::Accessibility,
                        (false, true) => Tier::Clipboard,
                        (false, false) => Tier::Refuse,
                    };
                    assert_eq!(
                        tier, expected,
                        "{grant:?} writable={writable} paste={paste}"
                    );
                }
            }
        }
    }

    /// A missing grant is a checkbox in System Settings; an application that accepts nothing is not.
    #[test]
    fn a_missing_grant_and_an_unusable_app_give_different_reasons() {
        let missing = refusal_reason(AccessibilityGrant::Denied, target(true, false));
        assert!(missing.contains("Accessibility permission"), "{missing}");

        let unusable = refusal_reason(AccessibilityGrant::Granted, target(false, false));
        assert!(
            !unusable.contains("permission"),
            "telling a user to grant a permission that will not help is worse than saying nothing: \
             {unusable}"
        );
    }

    #[test]
    fn the_accessibility_path_needs_no_explanation() {
        assert_eq!(aftermath(&Insertion::Accessibility), None);
    }

    /// A user whose clipboard was silently replaced deserves to be told, not to find out at their
    /// next paste.
    #[test]
    fn a_borrowed_clipboard_is_mentioned() {
        let restored = aftermath(&Insertion::Clipboard {
            clipboard_restored: true,
        })
        .expect("a sentence");
        assert!(restored.contains("put back"), "{restored}");

        let lost = aftermath(&Insertion::Clipboard {
            clipboard_restored: false,
        })
        .expect("a warning");
        assert!(lost.contains("could not be restored"), "{lost}");
    }

    #[test]
    fn a_refusal_carries_its_reason_through() {
        let outcome = Insertion::Refused {
            reason: "nowhere to put it".into(),
        };
        assert_eq!(aftermath(&outcome).as_deref(), Some("nowhere to put it"));
        assert!(!outcome.inserted());
    }

    #[test]
    fn an_uninspected_target_accepts_nothing() {
        let unknown = TargetCapabilities::unknown();
        assert!(!unknown.accessibility_writable);
        assert!(!unknown.accepts_paste);
    }

    #[test]
    fn an_insertion_round_trips_through_json() {
        let outcome = Insertion::Clipboard {
            clipboard_restored: false,
        };
        let json = serde_json::to_string(&outcome).expect("serializes");
        assert_eq!(
            serde_json::from_str::<Insertion>(&json).expect("deserializes"),
            outcome
        );
    }
}
