//! Global hotkeys, text insertion and screen context for the desktop assistant.
//!
//! # What is here, and what deliberately is not
//!
//! The decisions. Which hotkey a feature holds and whether that collides; which insertion tier to
//! attempt and what to tell the user afterwards. All pure, all enumerable in a test.
//!
//! **There is no native code in this crate yet.** No `unsafe`, no accessibility API, no screen
//! capture, no OS hotkey registration. That is not an oversight and not a stub: the design's own A6
//! says every native path needs a grant a `cargo test` binary cannot hold, so those tests would
//! arrive `#[ignore]`d — and shipping a quarantine crate full of unrunnable code would say this
//! feature works when nothing has ever exercised it.
//!
//! What this crate does instead is the split `audio-capture` already demonstrates: the logic that
//! can be wrong lives where it can be checked, and the platform layer arrives behind a feature flag
//! when somebody can hold the grant to verify it. Consumers get a real contract to build against in
//! the meantime.
//!
//! # Why the assistant is last
//!
//! Its own design recommends deferring it, and it is the least meeting-shaped thing in the roadmap —
//! a dictation surface and a screen-context reader are a different product from meeting
//! intelligence. This is the foundation, not the feature.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod hotkey;
mod insert;

pub use hotkey::{
    is_commonly_claimed, Binding, HotkeyError, HotkeyRegistry, Modifier, AVOID_BY_DEFAULT,
};
pub use insert::{
    aftermath, choose_tier, refusal_reason, AccessibilityGrant, Insertion, TargetCapabilities, Tier,
};
