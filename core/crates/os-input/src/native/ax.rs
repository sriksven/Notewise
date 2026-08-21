//! The accessibility API: reading and writing another application's focused field.
//!
//! # What this can and cannot see
//!
//! With the Accessibility grant, the system-wide element leads to whatever has keyboard focus in
//! whichever app is frontmost, and its value can often be read and sometimes written. Without the
//! grant every call returns `kAXErrorAPIDisabled`, which is why that error has a name here and is
//! reported as a missing permission rather than a failure.
//!
//! # Why attribute names are string literals
//!
//! `kAXValueAttribute` and friends are `CFStringRef` constants exported by the framework, and their
//! values are exactly the strings written here. Linking the constants would mean six more extern
//! declarations for no additional safety — the string is the contract either way, and a typo fails
//! the same way in both versions.

use super::cf;

type AXUIElementRef = cf::CFTypeRef;
type AXError = i32;

/// `kAXErrorSuccess`.
const SUCCESS: AXError = 0;
/// `kAXErrorAPIDisabled` — the Accessibility grant is missing.
const API_DISABLED: AXError = -25211;
/// `kAXErrorNoValue` — the attribute exists and holds nothing. Not a failure.
const NO_VALUE: AXError = -25212;
/// `kAXErrorAttributeUnsupported` — this element has no such attribute.
const ATTRIBUTE_UNSUPPORTED: AXError = -25205;

pub const FOCUSED_ELEMENT: &str = "AXFocusedUIElement";
pub const FOCUSED_WINDOW: &str = "AXFocusedWindow";
pub const FOCUSED_APPLICATION: &str = "AXFocusedApplication";
pub const VALUE: &str = "AXValue";
pub const SELECTED_TEXT: &str = "AXSelectedText";
pub const TITLE: &str = "AXTitle";

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: cf::CFStringRef,
        value: *mut cf::CFTypeRef,
    ) -> AXError;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: cf::CFStringRef,
        value: cf::CFTypeRef,
    ) -> AXError;
    fn AXUIElementIsAttributeSettable(
        element: AXUIElementRef,
        attribute: cf::CFStringRef,
        settable: *mut bool,
    ) -> AXError;
}

/// Why an attribute could not be read or written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxFailure {
    /// The grant is missing. Every call fails this way until it is given.
    PermissionMissing,
    /// The element does not have that attribute, or has it and it is empty.
    ///
    /// One variant for both because the caller does the same thing either way: there is no text
    /// here, which is an ordinary answer rather than a fault.
    Absent,
    /// Something else. The code is carried because it is the only thing worth logging.
    Failed(AXError),
}

impl AxFailure {
    fn of(status: AXError) -> Self {
        match status {
            API_DISABLED => AxFailure::PermissionMissing,
            NO_VALUE | ATTRIBUTE_UNSUPPORTED => AxFailure::Absent,
            other => AxFailure::Failed(other),
        }
    }
}

impl std::fmt::Display for AxFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AxFailure::PermissionMissing => f.write_str("the Accessibility permission is missing"),
            AxFailure::Absent => f.write_str("there is no such attribute"),
            AxFailure::Failed(code) => write!(f, "AXError {code}"),
        }
    }
}

/// One element in some application's window.
#[derive(Debug)]
pub struct Element(cf::Owned);

/// The root, from which focus is reachable.
pub fn system_wide() -> Option<Element> {
    // SAFETY: takes no arguments and returns an owned reference, which `Owned` releases.
    unsafe { cf::Owned::from_create(AXUIElementCreateSystemWide()).map(Element) }
}

/// Whatever has keyboard focus right now, anywhere on the machine.
pub fn focused_element() -> Result<Element, AxFailure> {
    system_wide()
        .ok_or(AxFailure::Failed(0))?
        .element_attribute(FOCUSED_ELEMENT)
}

impl Element {
    /// Copy an attribute's value.
    fn copy_attribute(&self, attribute: &str) -> Result<cf::Owned, AxFailure> {
        let name = cf::cfstring(attribute).ok_or(AxFailure::Absent)?;
        let mut value: cf::CFTypeRef = std::ptr::null();

        // SAFETY: `name` outlives the call and is borrowed, not transferred. On success the
        // function writes one owned reference into `value`, which is wrapped immediately; on
        // failure it writes nothing and `value` stays null, which `from_create` rejects.
        let status =
            unsafe { AXUIElementCopyAttributeValue(self.0.as_ptr(), name.as_ptr(), &mut value) };

        if status != SUCCESS {
            return Err(AxFailure::of(status));
        }

        // SAFETY: a successful copy transfers ownership of exactly this reference.
        unsafe { cf::Owned::from_create(value) }.ok_or(AxFailure::Absent)
    }

    /// An attribute as text. `Absent` when the attribute is missing, empty, or not a string.
    pub fn string_attribute(&self, attribute: &str) -> Result<String, AxFailure> {
        let value = self.copy_attribute(attribute)?;
        cf::to_string(value.as_ptr()).ok_or(AxFailure::Absent)
    }

    /// An attribute that is itself an element — focus, the frontmost app, the focused window.
    pub fn element_attribute(&self, attribute: &str) -> Result<Element, AxFailure> {
        Ok(Element(self.copy_attribute(attribute)?))
    }

    /// Whether an attribute can be written.
    ///
    /// Asked before writing rather than after failing, because a failed write into the wrong place
    /// is not always visible — and this is the check that decides which insertion tier runs.
    pub fn is_settable(&self, attribute: &str) -> bool {
        let Some(name) = cf::cfstring(attribute) else {
            return false;
        };
        let mut settable = false;

        // SAFETY: `settable` is a live local for the duration of the call, and the function writes
        // one bool into it only on success. `name` is borrowed.
        let status = unsafe {
            AXUIElementIsAttributeSettable(self.0.as_ptr(), name.as_ptr(), &mut settable)
        };

        status == SUCCESS && settable
    }

    /// Write text into an attribute.
    pub fn set_string_attribute(&self, attribute: &str, value: &str) -> Result<(), AxFailure> {
        let name = cf::cfstring(attribute).ok_or(AxFailure::Absent)?;
        let text = cf::cfstring(value).ok_or(AxFailure::Absent)?;

        // SAFETY: both arguments are borrowed for the duration of the call — the setter copies what
        // it needs — and both are released when the locals drop.
        let status =
            unsafe { AXUIElementSetAttributeValue(self.0.as_ptr(), name.as_ptr(), text.as_ptr()) };

        if status == SUCCESS {
            Ok(())
        } else {
            Err(AxFailure::of(status))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mapping from an OS error code to something a caller can act on. Pure, so it runs.
    #[test]
    fn a_missing_grant_is_distinguishable_from_a_missing_attribute() {
        assert_eq!(AxFailure::of(API_DISABLED), AxFailure::PermissionMissing);
        assert_eq!(AxFailure::of(NO_VALUE), AxFailure::Absent);
        assert_eq!(AxFailure::of(ATTRIBUTE_UNSUPPORTED), AxFailure::Absent);
        assert_eq!(AxFailure::of(-25200), AxFailure::Failed(-25200));
    }

    /// An empty field and a missing permission must never produce the same message: one is a
    /// setting the user can change and the other is nothing at all.
    #[test]
    fn the_failures_read_differently() {
        assert!(AxFailure::PermissionMissing
            .to_string()
            .contains("Accessibility"));
        assert!(!AxFailure::Absent.to_string().contains("Accessibility"));
    }

    /// The root element is available without any grant — it is reading *through* it that is gated.
    /// So this runs, and proves the framework is linked and the call convention is right.
    #[test]
    fn the_system_wide_element_can_be_created() {
        assert!(system_wide().is_some());
    }

    /// Reading focus without the grant returns rather than hanging or crashing.
    ///
    /// Deliberately not asserting *which* failure. macOS answers this with at least three
    /// different codes depending on the version and on whether assistive access is on for anything
    /// else on the machine — `kAXErrorAPIDisabled` when it is off globally, `kAXErrorCannotComplete`
    /// when the frontmost app will not answer us. Pinning one of them would make this test a
    /// report on the machine it ran on. What matters, and what is checked, is that the call comes
    /// back at all: this is the path every development build takes.
    #[test]
    fn reading_focus_without_the_grant_returns_rather_than_misbehaving() {
        let _ = focused_element();
    }

    #[test]
    #[ignore = "needs the Accessibility grant and a focused text field in another application"]
    fn the_focused_field_can_be_read_and_written() {
        let element = focused_element().expect("focus is readable with the grant");
        let before = element.string_attribute(VALUE).expect("a value");
        element
            .set_string_attribute(VALUE, "written by a test")
            .expect("the write succeeds");
        element
            .set_string_attribute(VALUE, &before)
            .expect("and is undone");
    }
}
