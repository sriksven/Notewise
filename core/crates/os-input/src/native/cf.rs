//! The CoreFoundation edge: strings, data, and release discipline.
//!
//! # Why [`Owned`] exists
//!
//! Every CoreFoundation function is either a Create/Copy — which hands over a reference this code
//! must release — or a Get, which does not. Mixing them up leaks or double-frees, and the
//! difference is only in the function's name. So the ones that transfer ownership are wrapped the
//! moment they return, and `Drop` does the releasing. There is no path through this module where
//! remembering to release is left to a reader.

use std::ffi::c_void;

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFDataRef = *const c_void;
pub type CFArrayRef = *const c_void;
pub type CFIndex = isize;
pub type CFTypeID = usize;

/// `kCFStringEncodingUTF8`.
pub const UTF8: u32 = 0x0800_0100;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    fn CFStringGetTypeID() -> CFTypeID;
    fn CFDataGetTypeID() -> CFTypeID;

    fn CFStringCreateWithBytes(
        allocator: CFTypeRef,
        bytes: *const u8,
        num_bytes: CFIndex,
        encoding: u32,
        is_external_representation: bool,
    ) -> CFStringRef;
    fn CFStringGetLength(string: CFStringRef) -> CFIndex;
    fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: u32) -> CFIndex;
    fn CFStringGetCString(
        string: CFStringRef,
        buffer: *mut u8,
        buffer_size: CFIndex,
        encoding: u32,
    ) -> bool;

    fn CFDataCreate(allocator: CFTypeRef, bytes: *const u8, length: CFIndex) -> CFDataRef;
    fn CFDataGetLength(data: CFDataRef) -> CFIndex;
    fn CFDataGetBytePtr(data: CFDataRef) -> *const u8;

    fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, index: CFIndex) -> CFTypeRef;
}

/// A CoreFoundation object this code owns a reference to.
///
/// Deliberately not `Send`: the AX and pasteboard objects held through this are documented as
/// main-thread affine, and a type that cannot cross threads cannot be used from the wrong one.
#[derive(Debug)]
pub struct Owned(CFTypeRef);

impl Owned {
    /// Wrap a reference from a Create or Copy function. `None` for null.
    ///
    /// # Safety
    ///
    /// `ptr` must be a reference this code owns — that is, from a function whose name contains
    /// `Create` or `Copy` — and must not be released anywhere else.
    pub unsafe fn from_create(ptr: CFTypeRef) -> Option<Self> {
        if ptr.is_null() {
            None
        } else {
            Some(Self(ptr))
        }
    }

    pub fn as_ptr(&self) -> CFTypeRef {
        self.0
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: `from_create` is the only constructor and it rejects null, so this is exactly one
        // owned reference being given back.
        unsafe { CFRelease(self.0) }
    }
}

/// A UTF-8 Rust string as a `CFString`.
pub fn cfstring(value: &str) -> Option<Owned> {
    // SAFETY: the bytes and their length come from one slice, the encoding matches those bytes, and
    // CFStringCreateWithBytes copies them — so the borrow does not outlive this call.
    unsafe {
        Owned::from_create(CFStringCreateWithBytes(
            std::ptr::null(),
            value.as_ptr(),
            value.len() as CFIndex,
            UTF8,
            false,
        ))
    }
}

/// Read a `CFString` back into a Rust string.
///
/// Checks the type first. A CoreFoundation "any" can hold a number or an array, and reading one of
/// those as a string is how a plausible-looking value turns into garbage.
pub fn to_string(value: CFTypeRef) -> Option<String> {
    if value.is_null() {
        return None;
    }

    // SAFETY: a null check above, and the type is verified before the string functions are used.
    unsafe {
        if CFGetTypeID(value) != CFStringGetTypeID() {
            return None;
        }

        let length = CFStringGetLength(value);
        if length == 0 {
            return Some(String::new());
        }

        // Plus one for the terminator CFStringGetCString writes.
        let capacity = CFStringGetMaximumSizeForEncoding(length, UTF8) + 1;
        if capacity <= 0 {
            return None;
        }

        let mut buffer = vec![0u8; capacity as usize];
        if !CFStringGetCString(value, buffer.as_mut_ptr(), capacity, UTF8) {
            return None;
        }

        let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
        buffer.truncate(end);
        String::from_utf8(buffer).ok()
    }
}

/// Bytes as a `CFData`.
pub fn cfdata(bytes: &[u8]) -> Option<Owned> {
    // SAFETY: pointer and length describe one slice, and CFDataCreate copies.
    unsafe {
        Owned::from_create(CFDataCreate(
            std::ptr::null(),
            bytes.as_ptr(),
            bytes.len() as CFIndex,
        ))
    }
}

/// A `CFData`'s contents, copied out.
pub fn data_bytes(value: CFTypeRef) -> Option<Vec<u8>> {
    if value.is_null() {
        return None;
    }

    // SAFETY: the type is checked, then the length and pointer come from the same object and are
    // read within one call — nothing here outlives the borrow.
    unsafe {
        if CFGetTypeID(value) != CFDataGetTypeID() {
            return None;
        }

        let length = CFDataGetLength(value);
        let pointer = CFDataGetBytePtr(value);
        if length < 0 || pointer.is_null() {
            return None;
        }

        Ok::<Vec<u8>, ()>(std::slice::from_raw_parts(pointer, length as usize).to_vec()).ok()
    }
}

/// Every element of a `CFArray`, as strings. Non-strings are skipped.
pub fn string_array(array: CFTypeRef) -> Vec<String> {
    if array.is_null() {
        return Vec::new();
    }

    // SAFETY: `CFArrayGetValueAtIndex` borrows rather than transfers, so nothing here is released.
    // The index stays below the reported count.
    unsafe {
        let count = CFArrayGetCount(array);
        (0..count)
            .filter_map(|index| to_string(CFArrayGetValueAtIndex(array, index)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real round trip through CoreFoundation. Needs no permission of any kind, so it runs.
    #[test]
    fn a_string_survives_the_round_trip() {
        let owned = cfstring("dictated text").expect("creates");
        assert_eq!(to_string(owned.as_ptr()).as_deref(), Some("dictated text"));
    }

    /// A transcript can contain anything a person said, in any script.
    #[test]
    fn non_ascii_survives_the_round_trip() {
        for value in ["café", "日本語のテキスト", "emoji 🎙 included", "Ω≈ç√"] {
            let owned = cfstring(value).expect("creates");
            assert_eq!(to_string(owned.as_ptr()).as_deref(), Some(value), "{value}");
        }
    }

    #[test]
    fn an_empty_string_is_a_string_and_not_a_failure() {
        let owned = cfstring("").expect("creates");
        assert_eq!(to_string(owned.as_ptr()).as_deref(), Some(""));
    }

    /// Reading a number as a string is how a plausible-looking value turns into garbage.
    #[test]
    fn a_non_string_is_refused_rather_than_misread() {
        let data = cfdata(b"not a string").expect("creates");
        assert_eq!(to_string(data.as_ptr()), None);
    }

    #[test]
    fn null_reads_as_nothing() {
        assert_eq!(to_string(std::ptr::null()), None);
        assert_eq!(data_bytes(std::ptr::null()), None);
        assert!(string_array(std::ptr::null()).is_empty());
    }

    #[test]
    fn data_survives_the_round_trip() {
        let bytes = b"\x00\x01\xffbinary".to_vec();
        let data = cfdata(&bytes).expect("creates");
        assert_eq!(data_bytes(data.as_ptr()), Some(bytes));
    }

    #[test]
    fn a_string_is_not_mistaken_for_data() {
        let string = cfstring("text").expect("creates");
        assert_eq!(data_bytes(string.as_ptr()), None);
    }

    /// Creating and dropping many objects must not leak or crash. Not a leak *detector*, but it
    /// would catch a double free immediately.
    #[test]
    fn many_creations_and_drops_are_stable() {
        for i in 0..2_000 {
            let owned = cfstring(&format!("value {i}")).expect("creates");
            assert!(to_string(owned.as_ptr()).is_some());
        }
    }
}
