//! Borrowing the clipboard, and giving it back.
//!
//! # Why the C pasteboard and not `NSPasteboard`
//!
//! `PasteboardCreate` and friends are a plain C API in ApplicationServices. Using them keeps this
//! crate's only foreign dependency the frameworks themselves, matching how `macos-permissions`
//! reads TCC — no Objective-C runtime, no message sends, nothing to get wrong about object
//! lifetimes beyond CoreFoundation's own rules.
//!
//! # Why what is on the clipboard matters more than it looks
//!
//! The insertion path replaces the clipboard and puts it back. That is fine for text. It is not fine
//! for a screenshot somebody copied thirty seconds ago, because a text snapshot cannot restore an
//! image — restoring would *delete* it. So a snapshot records not only the text it captured but
//! whether there was anything it could not, and the caller is told rather than reassured.

use super::cf;
use crate::ClipboardSnapshot;

type PasteboardRef = cf::CFTypeRef;
type PasteboardItemID = cf::CFTypeRef;
type OSStatus = i32;

const NO_ERROR: OSStatus = 0;
/// `kPasteboardFlavorNoFlags`.
const NO_FLAGS: u32 = 0;

/// The uniform type for plain UTF-8 text — what a dictated sentence is.
const UTF8_TEXT: &str = "public.utf8-plain-text";

/// Any non-null identifier works for an item this process is putting on the pasteboard.
const OUR_ITEM: PasteboardItemID = 1 as PasteboardItemID;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn PasteboardCreate(name: cf::CFStringRef, out: *mut PasteboardRef) -> OSStatus;
    fn PasteboardClear(pasteboard: PasteboardRef) -> OSStatus;
    fn PasteboardSynchronize(pasteboard: PasteboardRef) -> u32;
    fn PasteboardGetItemCount(pasteboard: PasteboardRef, out: *mut usize) -> OSStatus;
    /// `inIndex` is a `CFIndex`, which is 64 bits wide. Declaring it as a `u32` leaves the upper
    /// half of the argument register undefined on arm64, and the garbage index segfaults inside
    /// the framework — which is exactly what it did.
    fn PasteboardGetItemIdentifier(
        pasteboard: PasteboardRef,
        index: cf::CFIndex,
        out: *mut PasteboardItemID,
    ) -> OSStatus;
    fn PasteboardCopyItemFlavors(
        pasteboard: PasteboardRef,
        item: PasteboardItemID,
        out: *mut cf::CFArrayRef,
    ) -> OSStatus;
    fn PasteboardCopyItemFlavorData(
        pasteboard: PasteboardRef,
        item: PasteboardItemID,
        flavor: cf::CFStringRef,
        out: *mut cf::CFDataRef,
    ) -> OSStatus;
    fn PasteboardPutItemFlavor(
        pasteboard: PasteboardRef,
        item: PasteboardItemID,
        flavor: cf::CFStringRef,
        data: cf::CFDataRef,
        flags: u32,
    ) -> OSStatus;
}

/// The system clipboard.
#[derive(Debug)]
pub struct Clipboard(cf::Owned);

impl Clipboard {
    pub fn open() -> Result<Self, String> {
        // `kPasteboardClipboard`'s value. Passed as a string rather than linking the constant for
        // the same reason the AX attribute names are: the string *is* the identifier.
        let name = cf::cfstring("com.apple.pasteboard.clipboard")
            .ok_or_else(|| "could not name the clipboard".to_string())?;
        let mut pasteboard: PasteboardRef = std::ptr::null();

        // SAFETY: `name` is borrowed for the call. On success one owned reference is written to
        // `pasteboard` and wrapped immediately; on failure it stays null.
        let status = unsafe { PasteboardCreate(name.as_ptr(), &mut pasteboard) };
        if status != NO_ERROR {
            return Err(format!(
                "the clipboard could not be opened (OSStatus {status})"
            ));
        }

        // SAFETY: a successful create transfers ownership of exactly this reference.
        let owned = unsafe { cf::Owned::from_create(pasteboard) }
            .ok_or_else(|| "the clipboard came back empty-handed".to_string())?;

        Ok(Self(owned))
    }

    /// Pick up changes other applications have made.
    ///
    /// Required before every read: the pasteboard is a shared resource and this handle caches, so a
    /// snapshot taken without synchronising can be one copy behind — which would restore the wrong
    /// thing.
    fn synchronize(&self) {
        // SAFETY: one argument, and the return value is a flags word this code does not need.
        unsafe {
            PasteboardSynchronize(self.0.as_ptr());
        }
    }

    /// What is on the clipboard now, and whether any of it could not be captured.
    pub fn snapshot(&self) -> Option<ClipboardSnapshot> {
        self.synchronize();

        let mut count: usize = 0;
        // SAFETY: writes one integer into a live local.
        if unsafe { PasteboardGetItemCount(self.0.as_ptr(), &mut count) } != NO_ERROR {
            return None;
        }

        // An empty clipboard is a snapshot, not a failure: restoring it means clearing, which is a
        // real restore and reported as one.
        if count == 0 {
            return Some(ClipboardSnapshot::default());
        }

        let mut text = None;
        let mut uncapturable = false;

        // Items are one-based, which is the sort of thing that silently reads the wrong one.
        for index in 1..=count {
            let mut item: PasteboardItemID = std::ptr::null();
            // SAFETY: writes one borrowed identifier into a live local. Item identifiers are not
            // owned references and must not be released.
            let status = unsafe {
                PasteboardGetItemIdentifier(self.0.as_ptr(), index as cf::CFIndex, &mut item)
            };
            if status != NO_ERROR || item.is_null() {
                uncapturable = true;
                continue;
            }

            for flavor in self.flavors(item) {
                if is_capturable_as_text(&flavor) {
                    if text.is_none() && flavor == UTF8_TEXT {
                        text = self.read_text(item);
                    }
                } else {
                    // An image, a file, rich text: real content a text snapshot cannot put back.
                    uncapturable = true;
                }
            }
        }

        Some(ClipboardSnapshot {
            text,
            had_uncapturable_content: uncapturable,
        })
    }

    fn flavors(&self, item: PasteboardItemID) -> Vec<String> {
        let mut array: cf::CFArrayRef = std::ptr::null();

        // SAFETY: on success one owned array reference is written and wrapped immediately.
        let status = unsafe { PasteboardCopyItemFlavors(self.0.as_ptr(), item, &mut array) };
        if status != NO_ERROR {
            return Vec::new();
        }

        // SAFETY: a successful copy transfers ownership.
        let Some(owned) = (unsafe { cf::Owned::from_create(array) }) else {
            return Vec::new();
        };

        cf::string_array(owned.as_ptr())
    }

    fn read_text(&self, item: PasteboardItemID) -> Option<String> {
        let flavor = cf::cfstring(UTF8_TEXT)?;
        let mut data: cf::CFDataRef = std::ptr::null();

        // SAFETY: `flavor` is borrowed; on success one owned data reference is written.
        let status = unsafe {
            PasteboardCopyItemFlavorData(self.0.as_ptr(), item, flavor.as_ptr(), &mut data)
        };
        if status != NO_ERROR {
            return None;
        }

        // SAFETY: a successful copy transfers ownership.
        let owned = unsafe { cf::Owned::from_create(data) }?;
        let bytes = cf::data_bytes(owned.as_ptr())?;
        String::from_utf8(bytes).ok()
    }

    /// Replace the clipboard with one piece of text.
    pub fn write_text(&self, text: &str) -> Result<(), String> {
        let flavor =
            cf::cfstring(UTF8_TEXT).ok_or_else(|| "could not name the type".to_string())?;
        let data =
            cf::cfdata(text.as_bytes()).ok_or_else(|| "could not wrap the text".to_string())?;

        // SAFETY: clear takes only the pasteboard; put borrows the flavor name and the data, both
        // of which outlive the call and are released with their locals.
        unsafe {
            let cleared = PasteboardClear(self.0.as_ptr());
            if cleared != NO_ERROR {
                return Err(format!(
                    "the clipboard would not clear (OSStatus {cleared})"
                ));
            }

            let status = PasteboardPutItemFlavor(
                self.0.as_ptr(),
                OUR_ITEM,
                flavor.as_ptr(),
                data.as_ptr(),
                NO_FLAGS,
            );
            if status != NO_ERROR {
                return Err(format!(
                    "the clipboard would not take the text (OSStatus {status})"
                ));
            }
        }

        Ok(())
    }

    /// Empty the clipboard, which is how an empty snapshot restores.
    pub fn clear(&self) -> Result<(), String> {
        // SAFETY: one argument.
        let status = unsafe { PasteboardClear(self.0.as_ptr()) };
        if status == NO_ERROR {
            Ok(())
        } else {
            Err(format!("the clipboard would not clear (OSStatus {status})"))
        }
    }

    /// Put a snapshot back.
    ///
    /// Answers `false` when it could not be — including when the snapshot knew it held something a
    /// text restore cannot reproduce. Claiming otherwise is the silent version of destroying it.
    pub fn restore(&self, snapshot: &ClipboardSnapshot) -> bool {
        if snapshot.had_uncapturable_content {
            // Put back what there is, so the user is not left with dictated text where their own
            // content was, but do not claim the restore worked.
            if let Some(text) = &snapshot.text {
                let _ = self.write_text(text);
            }
            return false;
        }

        match &snapshot.text {
            Some(text) => self.write_text(text).is_ok(),
            None => self.clear().is_ok(),
        }
    }
}

/// Whether a pasteboard type is plain text this code can capture and put back verbatim.
///
/// Pure, and the one piece of judgement in this file: everything else is a syscall. Rich text and
/// HTML are deliberately *not* capturable — the bytes could be copied, but restoring only the plain
/// text would silently strip the formatting, and a user who copied a styled paragraph and got back
/// an unstyled one has lost something.
pub fn is_capturable_as_text(flavor: &str) -> bool {
    matches!(
        flavor,
        "public.utf8-plain-text"
            | "public.utf16-plain-text"
            | "public.utf16-external-plain-text"
            | "public.plain-text"
            | "public.text"
            | "com.apple.traditional-mac-plain-text"
            | "NSStringPboardType"
    )
}

/// Serialises the tests that replace the real clipboard.
///
/// There is one system clipboard, `cargo test` runs in parallel, and a test that reads back what
/// another one just wrote fails for a reason that has nothing to do with the code. Shared with
/// [`super`], whose target-level test writes to the same place.
#[cfg(test)]
pub(super) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // A poisoned lock means another clipboard test panicked. Carrying on is right: the machine's
    // clipboard is in an unknown state either way, and refusing to run tells us less.
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The judgement call, and the reason it goes this way: restoring plain text over rich text
    /// silently strips the formatting.
    #[test]
    fn rich_content_is_not_capturable_as_text() {
        assert!(is_capturable_as_text("public.utf8-plain-text"));
        assert!(is_capturable_as_text("NSStringPboardType"));

        for rich in [
            "public.rtf",
            "public.html",
            "public.png",
            "public.tiff",
            "public.file-url",
            "com.adobe.pdf",
            "com.apple.webarchive",
        ] {
            assert!(!is_capturable_as_text(rich), "{rich} must not be captured");
        }
    }

    /// The clipboard needs no permission at all, so all of this is real and runs in CI.
    #[test]
    fn the_clipboard_opens() {
        Clipboard::open().expect("the clipboard is always there");
    }

    /// A genuine round trip through the real system clipboard: save what is there, write, read it
    /// back, put the original back. This is the one native path that can be fully verified, and it
    /// is also the one that can lose a user's data — so it is verified.
    #[test]
    fn text_written_to_the_clipboard_can_be_read_back_and_the_original_restored() {
        let _guard = test_lock();
        let clipboard = Clipboard::open().expect("opens");

        let before = clipboard.snapshot().expect("a snapshot");

        clipboard.write_text("notewise round trip").expect("writes");
        let after = clipboard.snapshot().expect("a snapshot");
        assert_eq!(after.text.as_deref(), Some("notewise round trip"));
        assert!(
            !after.had_uncapturable_content,
            "we put plain text on it, so nothing should look uncapturable"
        );

        // Put the machine back exactly as it was. A test that steals somebody's clipboard is a
        // test that gets deleted.
        assert!(clipboard.restore(&before) || before.had_uncapturable_content);
        if !before.had_uncapturable_content {
            let restored = clipboard.snapshot().expect("a snapshot");
            assert_eq!(restored.text, before.text);
        }
    }

    /// Non-ASCII text is the normal case for dictation, not an edge one.
    #[test]
    fn non_ascii_text_survives_the_clipboard() {
        let _guard = test_lock();
        let clipboard = Clipboard::open().expect("opens");
        let before = clipboard.snapshot().expect("a snapshot");

        clipboard.write_text("café — 日本語 🎙").expect("writes");
        assert_eq!(
            clipboard.snapshot().expect("a snapshot").text.as_deref(),
            Some("café — 日本語 🎙")
        );

        let _ = clipboard.restore(&before);
    }

    /// An empty clipboard is a state, not a failure, and clearing is a real restore.
    #[test]
    fn an_emptied_clipboard_reads_as_empty() {
        let _guard = test_lock();
        let clipboard = Clipboard::open().expect("opens");
        let before = clipboard.snapshot().expect("a snapshot");

        clipboard.write_text("temporary").expect("writes");
        clipboard.clear().expect("clears");

        let empty = clipboard.snapshot().expect("a snapshot");
        assert_eq!(empty.text, None);
        assert!(!empty.had_uncapturable_content);

        let _ = clipboard.restore(&before);
    }

    /// Restoring must never claim success for content it cannot reproduce.
    #[test]
    fn restoring_uncapturable_content_answers_false() {
        let _guard = test_lock();
        let clipboard = Clipboard::open().expect("opens");
        let before = clipboard.snapshot().expect("a snapshot");

        let pretend = ClipboardSnapshot {
            text: Some("the text part".into()),
            had_uncapturable_content: true,
        };
        assert!(
            !clipboard.restore(&pretend),
            "an image cannot come back from a text snapshot"
        );
        // The text part is still put back, so the user is not left with dictation where their
        // content was.
        assert_eq!(
            clipboard.snapshot().expect("a snapshot").text.as_deref(),
            Some("the text part")
        );

        let _ = clipboard.restore(&before);
    }
}
