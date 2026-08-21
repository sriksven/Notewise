//! Registering a global hotkey with the window server.
//!
//! # Why Carbon
//!
//! `RegisterEventHotKey` is the only API on macOS that claims a key combination system-wide without
//! watching every keystroke. The alternative — a `CGEventTap` — needs the Input Monitoring grant,
//! which means asking a user for permission to see everything they type in order to notice one
//! chord. For a product sold on privacy that trade is not close.
//!
//! Carbon is long-deprecated and this specific function is not: it is what every launcher and
//! clipboard manager on the platform still uses, and there is no replacement.
//!
//! # Main thread only, and the type system says so
//!
//! Carbon events are delivered to the application's event target, which is serviced by the main
//! thread's run loop. [`Registration`] holds a raw reference and is therefore not `Send` — which is
//! not an oversight to work around but the constraint being written down. A registration made on a
//! worker thread would never fire.
//!
//! Presses leave through an ordinary channel, which *is* `Send`, so the work a hotkey triggers
//! happens wherever the receiver lives.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

use crate::keycode::{ansi_keycode, carbon_modifiers};
use crate::{Binding, OsInputError, Result};

type OSStatus = i32;
type EventRef = *const c_void;
type EventTargetRef = *const c_void;
type EventHotKeyRef = *const c_void;
type EventHandlerRef = *const c_void;
type EventHandlerCallRef = *const c_void;

const NO_ERROR: OSStatus = 0;

/// `kEventClassKeyboard` — the four-character code `'keyb'`.
const CLASS_KEYBOARD: u32 = 0x6B65_7962;
/// `kEventHotKeyPressed`.
const HOT_KEY_PRESSED: u32 = 5;
/// `kEventParamDirectObject` — `'----'`.
const PARAM_DIRECT_OBJECT: u32 = 0x2D2D_2D2D;
/// `typeEventHotKeyID` — `'hkid'`.
const TYPE_HOT_KEY_ID: u32 = 0x686B_6964;
/// Our four-character signature, `'ntws'`, so a hotkey id of ours is identifiable in a trace.
const SIGNATURE: u32 = 0x6E74_7773;

#[repr(C)]
#[derive(Clone, Copy)]
struct EventTypeSpec {
    class: u32,
    kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

type EventHandlerProc = extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OSStatus;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandlerProc,
        num_types: usize,
        types: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut EventHandlerRef,
    ) -> OSStatus;
    fn RegisterEventHotKey(
        keycode: u32,
        modifiers: u32,
        id: EventHotKeyID,
        target: EventTargetRef,
        options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OSStatus;
    fn UnregisterEventHotKey(hotkey: EventHotKeyRef) -> OSStatus;
    fn GetEventParameter(
        event: EventRef,
        name: u32,
        parameter_type: u32,
        out_actual_type: *mut u32,
        buffer_size: usize,
        out_actual_size: *mut usize,
        data: *mut c_void,
    ) -> OSStatus;
}

/// Which feature owns which registered id, for the callback to look up.
///
/// Process-global because the Carbon handler is a bare `extern "C" fn` with no closure to capture
/// into. The alternative — passing a pointer through `user_data` — would mean a raw pointer to Rust
/// state living for as long as the handler, which is a worse thing to have to reason about than a
/// mutex.
static FEATURES: OnceLock<Mutex<BTreeMap<u32, String>>> = OnceLock::new();
static PRESSES: OnceLock<Sender<String>> = OnceLock::new();
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

fn features() -> &'static Mutex<BTreeMap<u32, String>> {
    FEATURES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The Carbon callback. Runs on the main thread, in the middle of event dispatch.
///
/// Does as little as possible: read the id, look up the name, send it. Anything slower would stall
/// the run loop that delivers every other event in the app.
extern "C" fn on_hotkey_pressed(
    _call: EventHandlerCallRef,
    event: EventRef,
    _user_data: *mut c_void,
) -> OSStatus {
    let mut id = EventHotKeyID::default();

    // SAFETY: the out-parameter is a live local of exactly the size passed, and the type code
    // matches what a hot-key event carries. Nothing is retained.
    let status = unsafe {
        GetEventParameter(
            event,
            PARAM_DIRECT_OBJECT,
            TYPE_HOT_KEY_ID,
            std::ptr::null_mut(),
            std::mem::size_of::<EventHotKeyID>(),
            std::ptr::null_mut(),
            &mut id as *mut EventHotKeyID as *mut c_void,
        )
    };

    if status != NO_ERROR || id.signature != SIGNATURE {
        // Not ours, or unreadable. `eventNotHandledErr` would be more correct, but returning
        // success and doing nothing cannot break another application's hotkey.
        return NO_ERROR;
    }

    // A poisoned mutex here would mean the callback panicked once already; dropping the press is
    // better than panicking across an FFI boundary, which is undefined behaviour.
    let name = match features().lock() {
        Ok(map) => map.get(&id.id).cloned(),
        Err(_) => None,
    };

    if let (Some(name), Some(sender)) = (name, PRESSES.get()) {
        // The receiver may be gone if the app is shutting down. Nothing to do about it.
        let _ = sender.send(name);
    }

    NO_ERROR
}

/// Start receiving hotkey presses.
///
/// Returns the receiver once. A second call answers `None`, because there is one channel and two
/// receivers would mean presses arriving at whichever happened to be polling.
///
/// Must be called before [`register`], and from the main thread — installing the handler is a Carbon
/// call like any other.
pub fn listen() -> Option<Receiver<String>> {
    if PRESSES.get().is_some() {
        return None;
    }

    let (sender, receiver) = mpsc::channel();
    if PRESSES.set(sender).is_err() {
        return None;
    }

    let spec = EventTypeSpec {
        class: CLASS_KEYBOARD,
        kind: HOT_KEY_PRESSED,
    };
    let mut handler: EventHandlerRef = std::ptr::null();

    // SAFETY: the spec array is a live local for the duration of the call and is copied by the
    // event manager. No user data is passed, so there is no pointer for the handler to outlive.
    // The returned handler reference is deliberately not kept: it lives for the process.
    unsafe {
        InstallEventHandler(
            GetApplicationEventTarget(),
            on_hotkey_pressed,
            1,
            &spec,
            std::ptr::null_mut(),
            &mut handler,
        );
    }

    Some(receiver)
}

/// A live registration. Dropping it gives the combination back to the system.
///
/// Not `Send` on purpose — see the module docs.
#[derive(Debug)]
pub struct Registration {
    id: u32,
    reference: EventHotKeyRef,
    binding: String,
}

impl Registration {
    pub fn binding(&self) -> &str {
        &self.binding
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        if !self.reference.is_null() {
            // SAFETY: the reference came from a successful `RegisterEventHotKey` and is
            // unregistered exactly once, here.
            unsafe {
                UnregisterEventHotKey(self.reference);
            }
        }
        if let Ok(mut map) = features().lock() {
            map.remove(&self.id);
        }
    }
}

/// Claim a combination system-wide.
///
/// Fails rather than warns when the OS refuses, because the alternative is a hotkey the user has
/// configured, can see in settings, and that does nothing when pressed.
pub fn register(feature: &str, binding: &Binding) -> Result<Registration> {
    let keycode = ansi_keycode(binding.key()).ok_or_else(|| {
        OsInputError::Platform(format!(
            "'{}' is not a key this build can register",
            binding.key()
        ))
    })?;

    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);

    // Recorded before registering: the callback can fire the instant the registration succeeds, and
    // a press arriving before the name is known would be dropped.
    if let Ok(mut map) = features().lock() {
        map.insert(id, feature.to_string());
    }

    let mut reference: EventHotKeyRef = std::ptr::null();

    // SAFETY: the id is a plain C struct passed by value, the target comes from the framework, and
    // on success one reference is written which `Registration` unregisters on drop.
    let status = unsafe {
        RegisterEventHotKey(
            keycode as u32,
            carbon_modifiers(binding),
            EventHotKeyID {
                signature: SIGNATURE,
                id,
            },
            GetApplicationEventTarget(),
            0,
            &mut reference,
        )
    };

    if status != NO_ERROR || reference.is_null() {
        if let Ok(mut map) = features().lock() {
            map.remove(&id);
        }
        return Err(OsInputError::HotkeyUnavailable {
            binding: binding.to_string(),
        });
    }

    tracing::info!(feature, binding = %binding, "global hotkey registered");

    Ok(Registration {
        id,
        reference,
        binding: binding.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four-character codes, spelled out. A wrong one means a handler that never fires, which
    /// looks exactly like a hotkey another app has taken.
    #[test]
    fn the_four_character_codes_are_the_ones_carbon_expects() {
        assert_eq!(&CLASS_KEYBOARD.to_be_bytes(), b"keyb");
        assert_eq!(&PARAM_DIRECT_OBJECT.to_be_bytes(), b"----");
        assert_eq!(&TYPE_HOT_KEY_ID.to_be_bytes(), b"hkid");
        assert_eq!(&SIGNATURE.to_be_bytes(), b"ntws");
    }

    /// Ids must not repeat, or one feature's registration would answer to another's press.
    #[test]
    fn ids_are_unique() {
        let first = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let second = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        assert_ne!(first, second);
    }

    /// A key the table does not know is refused before the OS is asked, so the message names the
    /// key rather than blaming another application.
    #[test]
    fn an_unregisterable_key_is_refused_with_its_own_reason() {
        let binding = Binding::parse("cmd+eject").expect("parses as a binding");
        let error = register("dictation", &binding).expect_err("must refuse");

        assert!(matches!(error, OsInputError::Platform(_)), "{error:?}");
        assert!(error.to_string().contains("eject"), "{error}");
    }

    /// The application event target exists in any process, GUI or not.
    #[test]
    fn the_application_event_target_is_available() {
        // SAFETY: takes no arguments and returns a borrowed reference owned by the framework.
        let target = unsafe { GetApplicationEventTarget() };
        assert!(!target.is_null());
    }

    #[test]
    #[ignore = "needs a GUI process with a run loop; a test binary never dispatches Carbon events"]
    fn a_registered_hotkey_delivers_a_press() {
        let presses = listen().expect("the first listener");
        let binding = Binding::parse("cmd+alt+ctrl+f19").expect("parses");
        let _registration = register("dictation", &binding).expect("registers");

        let pressed = presses
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("somebody pressed it");
        assert_eq!(pressed, "dictation");
    }
}
