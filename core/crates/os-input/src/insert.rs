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

/// What was on the clipboard before it was borrowed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClipboardSnapshot {
    /// The plain text that was there. `None` for an empty clipboard, which restores by clearing.
    pub text: Option<String>,
    /// Whether there was content the snapshot could not capture — an image, a file, rich text.
    ///
    /// Recorded rather than ignored because putting the text back would *destroy* it, and a user
    /// whose copied screenshot silently vanished deserves to be told at the time rather than to
    /// discover it at their next paste.
    pub had_uncapturable_content: bool,
}

/// Everything the insertion tiers need from the operating system.
///
/// A trait so the tier machine — which is the part that can be wrong in an interesting way — runs
/// against a mock in CI. The real implementation needs the Accessibility grant, which a `cargo test`
/// binary cannot hold; this way the decisions are checked even though the syscalls are not.
pub trait TextTarget: std::fmt::Debug {
    /// What the focused element will accept.
    fn capabilities(&self) -> TargetCapabilities;

    /// Insert at the caret through the accessibility API, replacing any selection.
    ///
    /// Named for what it does rather than for the attribute it sets. Implementations must *insert*:
    /// writing the focused element's whole value instead would delete whatever the user had already
    /// typed, which for a dictation surface is the difference between a feature and a bug report.
    fn insert_via_accessibility(&self, text: &str) -> std::result::Result<(), String>;

    /// Read the clipboard so it can be put back. `None` if it could not be read at all.
    fn snapshot_clipboard(&self) -> Option<ClipboardSnapshot>;

    fn write_clipboard(&self, text: &str) -> std::result::Result<(), String>;

    /// Put back what [`Self::snapshot_clipboard`] found. `false` if it could not be.
    fn restore_clipboard(&self, snapshot: &ClipboardSnapshot) -> bool;

    /// Synthesise the paste keystroke.
    ///
    /// Implementations must not return until the paste has had a chance to land: the keystroke is
    /// delivered asynchronously, and restoring the clipboard too soon puts the old contents back
    /// before the target application has read the new ones.
    fn paste(&self) -> std::result::Result<(), String>;
}

/// Put text where the cursor is, through whichever tier works.
///
/// The order and the fallbacks are the whole design:
///
/// - the accessibility API first, because nothing else on the machine is touched;
/// - the clipboard second, and only after the first has been tried and failed — including when it
///   was available and *errored*, since a target that advertises a settable value and then refuses
///   the write is common enough to matter;
/// - a refusal third, which is a supported outcome. Inserting into the wrong field is worse than
///   saying it cannot be done, because the first is a mistake the user has to find and undo
///   somewhere they were not looking.
pub fn insert_with(target: &dyn TextTarget, grant: AccessibilityGrant, text: &str) -> Insertion {
    if text.is_empty() {
        return Insertion::Refused {
            reason: "There was nothing to insert.".into(),
        };
    }

    let capabilities = target.capabilities();

    match choose_tier(grant, capabilities) {
        Tier::Accessibility => match target.insert_via_accessibility(text) {
            Ok(()) => Insertion::Accessibility,
            Err(reason) => {
                tracing::debug!(%reason, "the accessibility write failed; falling back");
                if capabilities.accepts_paste {
                    via_clipboard(target, text)
                } else {
                    Insertion::Refused {
                        reason: "Notewise could not write into that field.".into(),
                    }
                }
            }
        },
        Tier::Clipboard => via_clipboard(target, text),
        Tier::Refuse => Insertion::Refused {
            reason: refusal_reason(grant, capabilities),
        },
    }
}

/// Borrow the clipboard, paste, and put it back.
fn via_clipboard(target: &dyn TextTarget, text: &str) -> Insertion {
    let snapshot = target.snapshot_clipboard();

    if let Err(reason) = target.write_clipboard(text) {
        return Insertion::Refused {
            reason: format!("Notewise could not use the clipboard: {reason}"),
        };
    }

    if let Err(reason) = target.paste() {
        // Put it back before reporting: the user did not get their text, and losing their clipboard
        // as well would be two failures for one attempt.
        let restored = snapshot
            .as_ref()
            .map(|snapshot| target.restore_clipboard(snapshot))
            .unwrap_or(false);

        return Insertion::Refused {
            reason: if restored {
                format!("Notewise could not paste into that app: {reason}")
            } else {
                format!(
                    "Notewise could not paste into that app: {reason}. Your text is on \
                     the clipboard."
                )
            },
        };
    }

    let restored = match &snapshot {
        // An image or a file cannot be put back from a text snapshot, and pretending otherwise
        // would be the silent version of destroying it.
        Some(snapshot) if snapshot.had_uncapturable_content => {
            let _ = target.restore_clipboard(snapshot);
            false
        }
        Some(snapshot) => target.restore_clipboard(snapshot),
        None => false,
    };

    Insertion::Clipboard {
        clipboard_restored: restored,
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
    // ------------------------------------------------------------ the machine, over a mock

    use std::cell::RefCell;

    /// A target whose every answer the test chooses.
    #[derive(Debug)]
    struct Mock {
        capabilities: TargetCapabilities,
        accessibility_write: std::result::Result<(), String>,
        clipboard: RefCell<Option<ClipboardSnapshot>>,
        write_result: std::result::Result<(), String>,
        paste_result: std::result::Result<(), String>,
        restore_succeeds: bool,
        /// Everything that happened, in order, so the sequence can be asserted rather than guessed.
        log: RefCell<Vec<String>>,
    }

    impl Mock {
        fn new(writable: bool, paste: bool) -> Self {
            Self {
                capabilities: target(writable, paste),
                accessibility_write: Ok(()),
                clipboard: RefCell::new(Some(ClipboardSnapshot {
                    text: Some("what the user had copied".into()),
                    had_uncapturable_content: false,
                })),
                write_result: Ok(()),
                paste_result: Ok(()),
                restore_succeeds: true,
                log: RefCell::new(Vec::new()),
            }
        }

        fn note(&self, what: &str) {
            self.log.borrow_mut().push(what.to_string());
        }

        fn log(&self) -> Vec<String> {
            self.log.borrow().clone()
        }
    }

    impl TextTarget for Mock {
        fn capabilities(&self) -> TargetCapabilities {
            self.capabilities
        }

        fn insert_via_accessibility(&self, _text: &str) -> std::result::Result<(), String> {
            self.note("accessibility");
            self.accessibility_write.clone()
        }

        fn snapshot_clipboard(&self) -> Option<ClipboardSnapshot> {
            self.note("snapshot");
            self.clipboard.borrow().clone()
        }

        fn write_clipboard(&self, text: &str) -> std::result::Result<(), String> {
            self.note("write");
            if self.write_result.is_ok() {
                *self.clipboard.borrow_mut() = Some(ClipboardSnapshot {
                    text: Some(text.to_string()),
                    had_uncapturable_content: false,
                });
            }
            self.write_result.clone()
        }

        fn restore_clipboard(&self, snapshot: &ClipboardSnapshot) -> bool {
            self.note("restore");
            if self.restore_succeeds {
                *self.clipboard.borrow_mut() = Some(snapshot.clone());
            }
            self.restore_succeeds
        }

        fn paste(&self) -> std::result::Result<(), String> {
            self.note("paste");
            self.paste_result.clone()
        }
    }

    /// Nothing else on the machine is touched, so the clipboard is never even read.
    #[test]
    fn the_accessibility_path_does_not_go_near_the_clipboard() {
        let mock = Mock::new(true, true);
        let outcome = insert_with(&mock, AccessibilityGrant::Granted, "dictated text");

        assert_eq!(outcome, Insertion::Accessibility);
        assert_eq!(mock.log(), vec!["accessibility"]);
        assert_eq!(
            mock.clipboard
                .borrow()
                .as_ref()
                .and_then(|c| c.text.clone()),
            Some("what the user had copied".into()),
            "the clipboard must be exactly as it was"
        );
    }

    /// A target that advertises a settable value and then refuses the write is common enough to
    /// matter, and the user should still get their text.
    #[test]
    fn a_failed_accessibility_write_falls_back_to_the_clipboard() {
        let mut mock = Mock::new(true, true);
        mock.accessibility_write = Err("AXError -25205".into());

        let outcome = insert_with(&mock, AccessibilityGrant::Granted, "dictated text");

        assert_eq!(
            outcome,
            Insertion::Clipboard {
                clipboard_restored: true
            }
        );
        assert_eq!(
            mock.log(),
            vec!["accessibility", "snapshot", "write", "paste", "restore"],
            "the order is the design: try, snapshot, borrow, paste, put back"
        );
    }

    /// And with nowhere to fall back to, it refuses rather than doing something else.
    #[test]
    fn a_failed_accessibility_write_with_no_paste_path_refuses() {
        let mut mock = Mock::new(true, false);
        mock.accessibility_write = Err("AXError -25205".into());

        let outcome = insert_with(&mock, AccessibilityGrant::Granted, "dictated text");
        assert!(matches!(outcome, Insertion::Refused { .. }));
        assert_eq!(mock.log(), vec!["accessibility"]);
    }

    /// The property the whole tier order exists for.
    #[test]
    fn the_clipboard_is_put_back_after_a_synthesised_paste() {
        let mock = Mock::new(false, true);
        let outcome = insert_with(&mock, AccessibilityGrant::Granted, "dictated text");

        assert_eq!(
            outcome,
            Insertion::Clipboard {
                clipboard_restored: true
            }
        );
        assert_eq!(
            mock.clipboard
                .borrow()
                .as_ref()
                .and_then(|c| c.text.clone()),
            Some("what the user had copied".into()),
            "what the user had copied has to come back"
        );
    }

    /// Restoring races with clipboard managers, and a race that lost must be reported.
    #[test]
    fn a_clipboard_that_could_not_be_put_back_is_reported_rather_than_assumed() {
        let mut mock = Mock::new(false, true);
        mock.restore_succeeds = false;

        let outcome = insert_with(&mock, AccessibilityGrant::Granted, "dictated text");
        assert_eq!(
            outcome,
            Insertion::Clipboard {
                clipboard_restored: false
            }
        );

        let told = aftermath(&outcome).expect("a warning");
        assert!(told.contains("could not be restored"), "{told}");
    }

    /// An image cannot be put back from a text snapshot, and pretending otherwise is the silent
    /// version of destroying it.
    #[test]
    fn a_clipboard_holding_something_uncapturable_is_never_claimed_restored() {
        let mock = Mock::new(false, true);
        *mock.clipboard.borrow_mut() = Some(ClipboardSnapshot {
            text: None,
            had_uncapturable_content: true,
        });

        let outcome = insert_with(&mock, AccessibilityGrant::Granted, "dictated text");
        assert_eq!(
            outcome,
            Insertion::Clipboard {
                clipboard_restored: false
            }
        );
    }

    /// An empty clipboard restores by being emptied again, which is a real restore.
    #[test]
    fn an_empty_clipboard_is_restored_to_empty() {
        let mock = Mock::new(false, true);
        *mock.clipboard.borrow_mut() = Some(ClipboardSnapshot::default());

        let outcome = insert_with(&mock, AccessibilityGrant::Granted, "dictated text");
        assert_eq!(
            outcome,
            Insertion::Clipboard {
                clipboard_restored: true
            }
        );
        assert_eq!(
            mock.clipboard
                .borrow()
                .as_ref()
                .and_then(|c| c.text.clone()),
            None
        );
    }

    /// Both tiers gone: refuse, and say which kind of refusal it is.
    #[test]
    fn with_no_tier_available_it_refuses_with_a_reason_the_user_can_act_on() {
        let mock = Mock::new(true, false);
        let outcome = insert_with(&mock, AccessibilityGrant::Denied, "dictated text");

        match outcome {
            Insertion::Refused { reason } => {
                assert!(reason.contains("Accessibility permission"), "{reason}")
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(mock.log().is_empty(), "nothing should have been attempted");
    }

    /// A paste that failed must not also cost the user their clipboard.
    #[test]
    fn a_failed_paste_puts_the_clipboard_back_before_reporting() {
        let mut mock = Mock::new(false, true);
        mock.paste_result = Err("the event was not delivered".into());

        let outcome = insert_with(&mock, AccessibilityGrant::Granted, "dictated text");
        assert!(matches!(outcome, Insertion::Refused { .. }));
        assert_eq!(
            mock.clipboard
                .borrow()
                .as_ref()
                .and_then(|c| c.text.clone()),
            Some("what the user had copied".into())
        );
        assert_eq!(mock.log(), vec!["snapshot", "write", "paste", "restore"]);
    }

    /// When it cannot be put back, the text is at least still reachable — and the message says so
    /// rather than leaving the user with neither their text nor their clipboard.
    #[test]
    fn a_failed_paste_that_also_lost_the_clipboard_says_where_the_text_went() {
        let mut mock = Mock::new(false, true);
        mock.paste_result = Err("no".into());
        mock.restore_succeeds = false;

        match insert_with(&mock, AccessibilityGrant::Granted, "dictated text") {
            Insertion::Refused { reason } => {
                assert!(reason.contains("on the clipboard"), "{reason}")
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_clipboard_that_cannot_be_written_refuses_rather_than_pasting_nothing() {
        let mut mock = Mock::new(false, true);
        mock.write_result = Err("the pasteboard is held by another process".into());

        let outcome = insert_with(&mock, AccessibilityGrant::Granted, "dictated text");
        assert!(matches!(outcome, Insertion::Refused { .. }));
        assert_eq!(mock.log(), vec!["snapshot", "write"], "nothing was pasted");
    }

    /// A clipboard that could not be read at all is borrowed anyway — the user asked for their
    /// text — but the loss is reported rather than hidden.
    #[test]
    fn an_unreadable_clipboard_is_still_borrowed_and_the_loss_is_reported() {
        let mock = Mock::new(false, true);
        *mock.clipboard.borrow_mut() = None;

        let outcome = insert_with(&mock, AccessibilityGrant::Granted, "dictated text");
        assert_eq!(
            outcome,
            Insertion::Clipboard {
                clipboard_restored: false
            }
        );
    }

    /// Nothing to insert is not an error worth a tier attempt.
    #[test]
    fn empty_text_is_refused_without_touching_anything() {
        let mock = Mock::new(true, true);
        assert!(matches!(
            insert_with(&mock, AccessibilityGrant::Granted, ""),
            Insertion::Refused { .. }
        ));
        assert!(mock.log().is_empty());
    }

    /// An unasked permission means the accessibility tier is not attempted at all.
    #[test]
    fn an_unasked_permission_goes_straight_to_the_clipboard() {
        let mock = Mock::new(true, true);
        let outcome = insert_with(&mock, AccessibilityGrant::Unknown, "dictated text");

        assert!(matches!(outcome, Insertion::Clipboard { .. }));
        assert!(
            !mock.log().contains(&"accessibility".to_string()),
            "acting on a permission that was never given is what a prompt exists to prevent"
        );
    }
}
