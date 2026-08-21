//! Global hotkeys, text insertion and screen context for the desktop assistant.
//!
//! # Two halves, and the split is the design
//!
//! The decisions are pure and tested: which hotkey a feature holds and whether that collides, which
//! insertion tier to attempt, what to tell the user afterwards, how a screenful of context reduces
//! to a prompt. All of that runs in CI on any platform.
//!
//! The platform layer is [`native`], behind the off-by-default `os-input` feature, and it is the
//! only place in this crate with `unsafe` in it — a build without the feature keeps
//! `forbid(unsafe_code)`, which the attribute below states conditionally rather than as a comment.
//! Engine CI must not need an Accessibility grant or a windowing toolchain, exactly as it must not
//! need a microphone.
//!
//! # What a green build here does and does not mean
//!
//! More than it used to and less than elsewhere in the repo. Genuinely verified, because none of it
//! needs a permission: the CoreFoundation string and data round trips, the entire pasteboard path
//! including a real save-and-restore of the user's own clipboard, building and posting keyboard
//! events, and every mapping from an OS error code to something a person can act on.
//!
//! Not verified, and `#[ignore]`d with the reason on each: reading or writing another application's
//! focused field, a paste that lands, a hotkey press. Those need the Accessibility grant and a GUI
//! process with a run loop. The design says so in A6 and it will stay true.
//!
//! # Why the assistant is last
//!
//! Its own design recommends deferring it, and it is the least meeting-shaped thing in the roadmap.
//! This is the foundation and dictation, not four features.

// Unsafe is confined to `native`, and only exists at all when the platform layer is compiled in.
#![cfg_attr(not(feature = "os-input"), forbid(unsafe_code))]
#![warn(missing_debug_implementations)]
#![deny(unsafe_op_in_unsafe_fn)]

mod completion;
mod context;
mod error;
mod hotkey;
mod insert;
mod keycode;

#[cfg(all(target_os = "macos", feature = "os-input"))]
pub mod native;

pub use completion::{continuation_of, decide, join, CompletionPolicy, Decision, TypingActivity};
pub use context::{ScreenContext, PROMPT_LIMIT};
pub use error::{not_compiled_in, unsupported_platform, OsInputError, Result};
pub use hotkey::{
    is_commonly_claimed, Binding, HotkeyError, HotkeyRegistry, Modifier, AVOID_BY_DEFAULT,
};
pub use insert::{
    aftermath, choose_tier, insert_with, refusal_reason, AccessibilityGrant, ClipboardSnapshot,
    Insertion, TargetCapabilities, TextTarget, Tier,
};
pub use keycode::{ansi_keycode, carbon_modifiers, KEY_V};

/// Whether this build can reach the operating system at all.
///
/// Reported rather than inferred, for the same reason `can_record` is on the health endpoint: a
/// client has no other way to know, and offering a dictation button that silently does nothing is
/// worse than saying plainly that this build cannot do it.
pub const SUPPORTED: bool = cfg!(all(target_os = "macos", feature = "os-input"));

/// Put text where the cursor is.
///
/// On a build without the platform layer, or off macOS, this refuses with a reason rather than
/// returning a success that did nothing.
pub fn insert_at_cursor(text: &str) -> Result<Insertion> {
    #[cfg(all(target_os = "macos", feature = "os-input"))]
    {
        native::insert_at_cursor(text)
    }

    #[cfg(all(target_os = "macos", not(feature = "os-input")))]
    {
        let _ = text;
        Err(not_compiled_in("Typing into other applications"))
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = text;
        Err(unsupported_platform("Typing into other applications"))
    }
}

/// What the user has highlighted, anywhere on the machine.
pub fn read_selection() -> Result<Option<String>> {
    #[cfg(all(target_os = "macos", feature = "os-input"))]
    {
        native::read_selection()
    }

    #[cfg(all(target_os = "macos", not(feature = "os-input")))]
    {
        Err(not_compiled_in("Reading the selected text"))
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err(unsupported_platform("Reading the selected text"))
    }
}

/// What is on screen, as text.
pub fn screen_context() -> Result<ScreenContext> {
    #[cfg(all(target_os = "macos", feature = "os-input"))]
    {
        native::screen_context()
    }

    #[cfg(all(target_os = "macos", not(feature = "os-input")))]
    {
        Err(not_compiled_in("Reading what is on screen"))
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err(unsupported_platform("Reading what is on screen"))
    }
}

/// Whether the focused element will let its selection be replaced.
///
/// The check "replace this text" needs before it is offered: on a target that will not take it the
/// user loses their selection and gets nothing.
pub fn selection_is_replaceable() -> Result<bool> {
    #[cfg(all(target_os = "macos", feature = "os-input"))]
    {
        native::selection_is_replaceable()
    }

    #[cfg(all(target_os = "macos", not(feature = "os-input")))]
    {
        Err(not_compiled_in("Replacing the selected text"))
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err(unsupported_platform("Replacing the selected text"))
    }
}

/// Read text off the screen as pixels.
///
/// Needs the Screen Recording grant, which macOS will not give a build without a Developer ID
/// signature — so on a development build this refuses with that reason rather than a permission
/// prompt that would do nothing.
pub fn recognise_text_on_screen() -> Result<String> {
    #[cfg(all(target_os = "macos", feature = "os-input"))]
    {
        native::recognise_text_on_screen()
    }

    #[cfg(all(target_os = "macos", not(feature = "os-input")))]
    {
        Err(not_compiled_in("Reading text from the screen"))
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err(unsupported_platform("Reading text from the screen"))
    }
}

/// Start noticing when the user pauses while typing.
///
/// Records timing and a count. Not keys — see `native::keystrokes` for why that distinction is the
/// whole argument for asking for Input Monitoring at all.
pub fn start_typing_monitor() -> Result<()> {
    #[cfg(all(target_os = "macos", feature = "os-input"))]
    {
        native::start_typing_monitor()
    }

    #[cfg(all(target_os = "macos", not(feature = "os-input")))]
    {
        Err(not_compiled_in("Noticing when you pause while typing"))
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err(unsupported_platform("Noticing when you pause while typing"))
    }
}

pub fn stop_typing_monitor() {
    #[cfg(all(target_os = "macos", feature = "os-input"))]
    native::stop_typing_monitor();
}

/// What has been typed lately, as timing.
///
/// A default — nothing running, nothing seen — on a build that cannot watch. Not an error: a caller
/// asking "should I suggest something" gets "no" rather than a failure to handle.
pub fn typing_activity() -> TypingActivity {
    #[cfg(all(target_os = "macos", feature = "os-input"))]
    {
        native::typing_activity()
    }

    #[cfg(not(all(target_os = "macos", feature = "os-input")))]
    {
        TypingActivity::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A build that cannot do something must say so rather than appearing to succeed.
    ///
    /// This is the whole of `63f6f6d`'s lesson applied to a second feature: the failure is typed,
    /// carries a reason, and does not send the user to a settings pane that would not help.
    #[test]
    fn every_entry_point_refuses_rather_than_pretending() {
        if SUPPORTED {
            // With the platform layer compiled in, the answer depends on a grant this test cannot
            // hold, so the refusal shape is asserted in `native`'s own tests instead.
            return;
        }

        for error in [
            insert_at_cursor("text").err(),
            read_selection().err(),
            screen_context().err(),
            selection_is_replaceable().err(),
            recognise_text_on_screen().err(),
            start_typing_monitor().err(),
        ] {
            let error = error.expect("a build without the platform layer must refuse");
            assert!(
                matches!(error, OsInputError::Unsupported { .. }),
                "{error:?}"
            );
            assert!(!error.to_string().is_empty());
        }
    }

    /// A caller asking "should I suggest something" gets "no" rather than a failure to handle.
    #[test]
    fn typing_activity_answers_on_every_build() {
        let activity = typing_activity();
        if !SUPPORTED {
            assert!(!activity.running);
            assert_eq!(activity.keystrokes, 0);
            assert_eq!(activity.last_keystroke_ms, None);
        }
    }

    /// The flag a client reads to decide whether to show the feature at all.
    #[test]
    fn support_is_reported_and_not_guessed() {
        assert_eq!(
            SUPPORTED,
            cfg!(all(target_os = "macos", feature = "os-input"))
        );
    }
}
