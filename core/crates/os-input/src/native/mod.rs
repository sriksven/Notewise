//! The platform layer: the only place in this crate with `unsafe` in it.
//!
//! # What lives here and why it is quarantined
//!
//! Four frameworks, reached through their C interfaces: CoreFoundation for strings and data,
//! ApplicationServices for the accessibility API and the pasteboard, CoreGraphics for synthesising
//! a keystroke, and Carbon for claiming a hotkey. Every other module in this crate is pure and
//! tested; this one is compiled only when the `os-input` feature is on, and without that feature the
//! crate keeps `forbid(unsafe_code)`.
//!
//! # What is verified and what is not
//!
//! Verified in CI, because it needs no grant: the CoreFoundation round trips, the whole pasteboard —
//! including a real save-and-restore of the user's actual clipboard — building and posting keyboard
//! events, creating the accessibility root, and every error-code mapping.
//!
//! Not verified, and `#[ignore]`d with the reason: reading or writing another application's focused
//! field, a paste that actually lands, and a hotkey press. Those need the Accessibility grant and a
//! GUI process with a run loop, neither of which a `cargo test` binary has. The design says so
//! outright in A6, and a green build here says less than it does elsewhere in the repo.

mod ax;
mod cf;
mod hotkeys;
mod keys;
mod keystrokes;
mod pasteboard;
mod vision;

pub use hotkeys::{listen, register, Registration};
pub use keystrokes::TapRefusal;
pub use vision::{recognise, Bitmap, ScreenCaptureRefusal};

use crate::completion::TypingActivity;

/// Whether the focused element will let its selection be replaced.
///
/// The check 9c needs: "replace" must only be offered where it can work, and offering it on a web
/// page that will not take it is worse than not offering it — the user loses their selection and
/// gets nothing.
pub fn selection_is_replaceable() -> Result<bool> {
    if !trusted() {
        return Err(needs_accessibility("Replacing the selected text"));
    }

    match ax::focused_element() {
        Ok(element) => Ok(element.is_settable(ax::SELECTED_TEXT)),
        Err(ax::AxFailure::PermissionMissing) => {
            Err(needs_accessibility("Replacing the selected text"))
        }
        Err(_) => Ok(false),
    }
}

/// Start watching for keystrokes. Timing only — see `keystrokes`.
pub fn start_typing_monitor() -> Result<()> {
    keystrokes::start().map_err(|refusal| match refusal {
        keystrokes::TapRefusal::PermissionMissing => OsInputError::PermissionRequired {
            what: "Noticing when you pause while typing".to_string(),
            how_to_grant: Capability::InputMonitoring.how_to_grant(),
        },
        keystrokes::TapRefusal::Failed => {
            OsInputError::Platform("the keystroke monitor could not start".to_string())
        }
    })
}

pub fn stop_typing_monitor() {
    keystrokes::stop()
}

pub fn typing_activity() -> TypingActivity {
    keystrokes::activity()
}

use notewise_macos_permissions::Capability;

use crate::context::ScreenContext;
use crate::insert::{
    insert_with, AccessibilityGrant, ClipboardSnapshot, TargetCapabilities, TextTarget,
};
use crate::{Insertion, OsInputError, Result};

/// Whether the Accessibility grant is in force right now.
fn trusted() -> bool {
    matches!(
        notewise_macos_permissions::accessibility(),
        notewise_macos_permissions::Authorization::Granted
    )
}

/// The error to return when it is not.
fn needs_accessibility(what: &str) -> OsInputError {
    OsInputError::PermissionRequired {
        what: what.to_string(),
        how_to_grant: Capability::Accessibility.how_to_grant(),
    }
}

/// This machine, as an insertion target.
#[derive(Debug, Default)]
pub struct MacTarget;

impl MacTarget {
    pub fn new() -> Self {
        Self
    }
}

impl TextTarget for MacTarget {
    /// What the focused element will accept.
    ///
    /// `accessibility_writable` asks whether `AXSelectedText` is settable, not `AXValue`. That
    /// distinction is the difference between inserting and overwriting: setting the value replaces
    /// everything in the field, which for someone dictating into a half-written sentence would
    /// delete the half they typed. Setting the selected text replaces the selection — and an empty
    /// selection is the caret, so it inserts.
    ///
    /// `accepts_paste` is only true when something has focus. A synthesised `⌘V` with nothing
    /// focused goes somewhere unpredictable, and the design's own rule is that inserting into the
    /// wrong field is worse than refusing.
    fn capabilities(&self) -> TargetCapabilities {
        match ax::focused_element() {
            Ok(element) => TargetCapabilities {
                accessibility_writable: element.is_settable(ax::SELECTED_TEXT),
                // Reaching here means the grant is held, which is also what `CGEventPost` needs.
                accepts_paste: true,
            },
            Err(failure) => {
                tracing::debug!(%failure, "nothing usable has focus");
                TargetCapabilities::unknown()
            }
        }
    }

    fn insert_via_accessibility(&self, text: &str) -> std::result::Result<(), String> {
        let element = ax::focused_element().map_err(|failure| failure.to_string())?;
        element
            .set_string_attribute(ax::SELECTED_TEXT, text)
            .map_err(|failure| failure.to_string())
    }

    fn snapshot_clipboard(&self) -> Option<ClipboardSnapshot> {
        pasteboard::Clipboard::open().ok()?.snapshot()
    }

    fn write_clipboard(&self, text: &str) -> std::result::Result<(), String> {
        pasteboard::Clipboard::open()?.write_text(text)
    }

    fn restore_clipboard(&self, snapshot: &ClipboardSnapshot) -> bool {
        match pasteboard::Clipboard::open() {
            Ok(clipboard) => clipboard.restore(snapshot),
            Err(reason) => {
                tracing::warn!(%reason, "could not reopen the clipboard to restore it");
                false
            }
        }
    }

    fn paste(&self) -> std::result::Result<(), String> {
        keys::paste()
    }
}

/// Put text where the cursor is.
///
/// The permission check is here rather than inside the tier machine on purpose: a missing grant is a
/// typed error carrying the pane to open, and routing it through a tier refusal would turn an
/// actionable message into a sentence about text fields.
pub fn insert_at_cursor(text: &str) -> Result<Insertion> {
    if !trusted() {
        return Err(needs_accessibility("Typing into other applications"));
    }

    Ok(insert_with(
        &MacTarget::new(),
        AccessibilityGrant::Granted,
        text,
    ))
}

/// What the user currently has highlighted, anywhere.
///
/// `Ok(None)` for "nothing is selected", which is an ordinary state and not a failure.
pub fn read_selection() -> Result<Option<String>> {
    if !trusted() {
        return Err(needs_accessibility("Reading the selected text"));
    }

    match ax::focused_element() {
        Ok(element) => match element.string_attribute(ax::SELECTED_TEXT) {
            Ok(text) if text.is_empty() => Ok(None),
            Ok(text) => Ok(Some(text)),
            Err(ax::AxFailure::PermissionMissing) => {
                Err(needs_accessibility("Reading the selected text"))
            }
            Err(_) => Ok(None),
        },
        Err(ax::AxFailure::PermissionMissing) => {
            Err(needs_accessibility("Reading the selected text"))
        }
        Err(_) => Ok(None),
    }
}

/// What is on screen, as text.
///
/// An empty result is a success: a user with nothing focused has no context, and making every
/// consumer treat that as an error would be wrong twice.
pub fn screen_context() -> Result<ScreenContext> {
    if !trusted() {
        return Err(needs_accessibility("Reading what is on screen"));
    }

    let mut context = ScreenContext::default();

    let Some(root) = ax::system_wide() else {
        return Ok(context);
    };

    if let Ok(app) = root.element_attribute(ax::FOCUSED_APPLICATION) {
        context.app = app
            .string_attribute(ax::TITLE)
            .ok()
            .filter(|t| !t.is_empty());

        if let Ok(window) = app.element_attribute(ax::FOCUSED_WINDOW) {
            context.window_title = window
                .string_attribute(ax::TITLE)
                .ok()
                .filter(|t| !t.is_empty());
        }
    }

    if let Ok(element) = root.element_attribute(ax::FOCUSED_ELEMENT) {
        context.selection = element
            .string_attribute(ax::SELECTED_TEXT)
            .ok()
            .filter(|t| !t.trim().is_empty());
        context.focused_text = element
            .string_attribute(ax::VALUE)
            .ok()
            .filter(|t| !t.trim().is_empty());
    }

    // A4: recognised text is the fallback, and only when nothing structured was available. The
    // accessibility API returns what an application says its field holds; recognition returns a
    // guess about pixels. Reading the screen when the real text was already in hand would be a
    // slower, worse answer and a capture the user did not need to consent to.
    //
    // A failure here is not a failure of `screen_context`: on any unsigned build the capture is
    // impossible, and that must not turn a perfectly good window title into an error.
    if context.selection.is_none() && context.focused_text.is_none() {
        match recognise_text_on_screen() {
            Ok(text) if !text.trim().is_empty() => context.recognised_text = Some(text),
            Ok(_) => {}
            Err(error) => tracing::debug!(%error, "no structured text, and none read from pixels"),
        }
    }

    Ok(context)
}

/// Read text off the screen as pixels.
///
/// The recognition half of this is real and tested — see [`vision`]. The capture half needs the
/// Screen Recording grant, and the three ways it can be refused have three different fixes, so they
/// are reported separately rather than collapsed into "unavailable".
pub fn recognise_text_on_screen() -> Result<String> {
    let bitmap = vision::capture_screen().map_err(|refusal| match refusal {
        // No switch in System Settings helps a build with no signature, so it must not be told to
        // go looking for one. This is the case every development build is in.
        ScreenCaptureRefusal::BuildCannotHoldGrant => OsInputError::Unsupported {
            what: "Reading text from the screen".to_string(),
            reason: "this build has no Developer ID signature, and macOS will not grant screen \
                     recording to one — the same wall system audio hits"
                .to_string(),
        },
        ScreenCaptureRefusal::GrantMissing => OsInputError::PermissionRequired {
            what: "Reading text from the screen".to_string(),
            how_to_grant: Capability::ScreenRecording.how_to_grant(),
        },
        ScreenCaptureRefusal::CaptureFailed => OsInputError::Platform(
            "the screen could not be captured, though the permission is held".to_string(),
        ),
    })?;

    let lines = vision::recognise(&bitmap).map_err(OsInputError::Platform)?;
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing grant must be an actionable message, not a sentence about text fields.
    #[test]
    fn without_the_grant_every_entry_point_says_which_pane_to_open() {
        // A test binary is not in the Accessibility list. On a developer machine that has granted
        // the terminal, these succeed instead — which is why the assertion is on the error's shape
        // rather than on there being one.
        for result in [
            insert_at_cursor("text").map(|_| ()),
            read_selection().map(|_| ()),
            screen_context().map(|_| ()),
        ] {
            if let Err(error) = result {
                assert!(
                    matches!(error, OsInputError::PermissionRequired { .. }),
                    "{error:?}"
                );
                let rendered = error.to_string();
                assert!(rendered.contains("Accessibility"), "{rendered}");
                assert!(rendered.contains("System Settings"), "{rendered}");
            }
        }
    }

    /// On an unsigned build the refusal is about the signature, not about a switch — because no
    /// switch helps. Every development build is in this case, so this runs.
    #[test]
    fn an_unsigned_build_is_told_about_the_signature_and_not_sent_to_settings() {
        if notewise_macos_permissions::can_hold_screen_recording() {
            return;
        }

        let error = recognise_text_on_screen().expect_err("must refuse");
        assert!(
            matches!(error, OsInputError::Unsupported { .. }),
            "{error:?}"
        );

        let rendered = error.to_string();
        assert!(rendered.contains("Developer ID"), "{rendered}");
        assert!(
            !rendered.contains("System Settings"),
            "there is no switch that fixes this, so it must not send the user looking for one"
        );
    }

    /// A signed build without the grant gets the pane instead. Two failures, two fixes.
    #[test]
    fn a_signed_build_without_the_grant_is_sent_to_the_pane() {
        if !notewise_macos_permissions::can_hold_screen_recording()
            || notewise_macos_permissions::screen_recording_granted()
        {
            return;
        }

        let error = recognise_text_on_screen().expect_err("must refuse");
        assert!(
            matches!(error, OsInputError::PermissionRequired { .. }),
            "{error:?}"
        );
        assert!(error.to_string().contains("System Settings"));
    }

    /// Inspecting capabilities must be safe with no grant and nothing focused — it runs on every
    /// insertion attempt, including the ones that are about to be refused.
    #[test]
    fn inspecting_the_target_is_safe_without_a_grant() {
        let capabilities = MacTarget::new().capabilities();
        // Values depend on the machine; what matters is that asking did not misbehave.
        let _ = capabilities.accepts_paste;
        let _ = capabilities.accessibility_writable;
    }

    /// The real clipboard, through the real target — the one tier that needs no permission, so the
    /// one place where the platform layer can be proven rather than asserted.
    #[test]
    fn the_target_can_snapshot_and_restore_the_real_clipboard() {
        let _guard = pasteboard::test_lock();
        let target = MacTarget::new();
        let before = target
            .snapshot_clipboard()
            .expect("the clipboard is readable");

        target
            .write_clipboard("notewise target round trip")
            .expect("writes");
        assert_eq!(
            target.snapshot_clipboard().and_then(|s| s.text).as_deref(),
            Some("notewise target round trip")
        );

        // Leave the machine as it was found.
        let restored = target.restore_clipboard(&before);
        assert!(restored || before.had_uncapturable_content);
    }

    #[test]
    #[ignore = "needs the Accessibility grant and a focused text field in another application"]
    fn text_reaches_a_real_field() {
        let outcome = insert_at_cursor("inserted by a test").expect("no permission error");
        assert!(outcome.inserted(), "{outcome:?}");
    }

    #[test]
    #[ignore = "needs the Accessibility grant; without it every attribute read is API-disabled"]
    fn the_screen_context_describes_the_frontmost_app() {
        let context = screen_context().expect("readable with the grant");
        assert!(context.app.is_some(), "{context:?}");
    }
}
