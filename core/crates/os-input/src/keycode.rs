//! Key names to the numbers macOS wants.
//!
//! # Why this is a table and not a computation
//!
//! A virtual key code is a position on a keyboard, not a character. `9` is where V sits on an ANSI
//! board, and it is still `9` on a French one where that position types a different letter. There is
//! no arithmetic from "the letter k" to a key code, only a layout, and this is the ANSI layout that
//! every other layout is described as a deviation from.
//!
//! # Why it is pure
//!
//! It is the part of hotkey registration that can be wrong in a way a test can catch. Everything
//! else about registration needs a window server.

/// The ANSI virtual key code for a key name, as [`crate::Binding`] spells it.
///
/// `None` for a name this build does not know, which is a refusal at configuration time rather than
/// a hotkey that registers and never fires.
pub fn ansi_keycode(key: &str) -> Option<u16> {
    let code = match key {
        // Letters, in keyboard order rather than alphabetical, because that is the order the codes
        // are in and a reader checking one against Apple's header wants to find it where it is.
        "a" => 0x00,
        "s" => 0x01,
        "d" => 0x02,
        "f" => 0x03,
        "h" => 0x04,
        "g" => 0x05,
        "z" => 0x06,
        "x" => 0x07,
        "c" => 0x08,
        "v" => 0x09,
        "b" => 0x0B,
        "q" => 0x0C,
        "w" => 0x0D,
        "e" => 0x0E,
        "r" => 0x0F,
        "y" => 0x10,
        "t" => 0x11,
        "o" => 0x1F,
        "u" => 0x20,
        "i" => 0x22,
        "p" => 0x23,
        "l" => 0x25,
        "j" => 0x26,
        "k" => 0x28,
        "n" => 0x2D,
        "m" => 0x2E,

        "1" => 0x12,
        "2" => 0x13,
        "3" => 0x14,
        "4" => 0x15,
        "5" => 0x17,
        "6" => 0x16,
        "7" => 0x1A,
        "8" => 0x1C,
        "9" => 0x19,
        "0" => 0x1D,

        "-" | "minus" => 0x1B,
        "=" | "equal" => 0x18,
        "[" => 0x21,
        "]" => 0x1E,
        "\\" => 0x2A,
        ";" => 0x29,
        "'" => 0x27,
        "," => 0x2B,
        "." => 0x2F,
        "/" => 0x2C,
        "`" => 0x32,

        "return" | "enter" => 0x24,
        "tab" => 0x30,
        "space" => 0x31,
        "delete" | "backspace" => 0x33,
        "escape" | "esc" => 0x35,

        "f1" => 0x7A,
        "f2" => 0x78,
        "f3" => 0x63,
        "f4" => 0x76,
        "f5" => 0x60,
        "f6" => 0x61,
        "f7" => 0x62,
        "f8" => 0x64,
        "f9" => 0x65,
        "f10" => 0x6D,
        "f11" => 0x67,
        "f12" => 0x6F,

        "left" => 0x7B,
        "right" => 0x7C,
        "down" => 0x7D,
        "up" => 0x7E,

        _ => return None,
    };

    Some(code)
}

/// The key code for V, used by the synthesised paste.
///
/// Named rather than written as `9` at the call site: a bare number in an event-posting function is
/// unreviewable.
pub const KEY_V: u16 = 0x09;

/// Carbon's modifier bits for a binding.
///
/// Carbon uses its own mask constants, which are not the same numbers as `CGEventFlags` or as the
/// `NSEvent` ones. Three different encodings of the same four keys is a genuine trap: a value from
/// the wrong set registers a hotkey nobody can press.
pub fn carbon_modifiers(binding: &crate::Binding) -> u32 {
    /// `cmdKey`.
    const COMMAND: u32 = 0x0100;
    /// `shiftKey`.
    const SHIFT: u32 = 0x0200;
    /// `optionKey`.
    const OPTION: u32 = 0x0800;
    /// `controlKey`.
    const CONTROL: u32 = 0x1000;

    binding
        .modifiers()
        .iter()
        .map(|modifier| match modifier {
            crate::Modifier::Super => COMMAND,
            crate::Modifier::Shift => SHIFT,
            crate::Modifier::Alt => OPTION,
            crate::Modifier::Ctrl => CONTROL,
        })
        .fold(0, |mask, bit| mask | bit)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spot-checks against Apple's `Events.h`. Wrong codes here mean a hotkey that fires on the
    /// wrong key, which is worse than one that does not register.
    #[test]
    fn the_codes_match_the_platform_header() {
        assert_eq!(ansi_keycode("a"), Some(0x00));
        assert_eq!(ansi_keycode("v"), Some(0x09));
        assert_eq!(ansi_keycode("k"), Some(0x28));
        assert_eq!(ansi_keycode("space"), Some(0x31));
        assert_eq!(ansi_keycode("escape"), Some(0x35));
        assert_eq!(ansi_keycode("f1"), Some(0x7A));
        assert_eq!(ansi_keycode("0"), Some(0x1D));
    }

    /// V is used by name in the paste path, and a mismatch there pastes nothing while looking fine.
    #[test]
    fn the_paste_key_is_the_v_key() {
        assert_eq!(Some(KEY_V), ansi_keycode("v"));
    }

    /// The number row is not sequential, and assuming it is puts `6` and `7` in the wrong places.
    #[test]
    fn the_number_row_is_not_sequential() {
        assert_eq!(ansi_keycode("5"), Some(0x17));
        assert_eq!(ansi_keycode("6"), Some(0x16));
        assert!(ansi_keycode("6") < ansi_keycode("5"));
    }

    /// The aliases people type. A key a user cannot name is a key they cannot bind.
    #[test]
    fn the_names_people_type_are_accepted() {
        assert_eq!(ansi_keycode("enter"), ansi_keycode("return"));
        assert_eq!(ansi_keycode("esc"), ansi_keycode("escape"));
        assert_eq!(ansi_keycode("backspace"), ansi_keycode("delete"));
    }

    /// A refusal at configuration time beats a hotkey that registers and never fires.
    #[test]
    fn an_unknown_key_is_refused_rather_than_guessed() {
        assert_eq!(ansi_keycode("fn"), None);
        assert_eq!(ansi_keycode("eject"), None);
        assert_eq!(ansi_keycode(""), None);
        assert_eq!(
            ansi_keycode("K"),
            None,
            "the binding parser lowercases first"
        );
    }

    /// No two names may share a code, or one binding silently shadows another.
    #[test]
    fn every_letter_has_its_own_code() {
        let letters = "abcdefghijklmnopqrstuvwxyz";
        let codes: std::collections::BTreeSet<u16> = letters
            .chars()
            .map(|c| ansi_keycode(&c.to_string()).expect("every letter is mapped"))
            .collect();
        assert_eq!(codes.len(), 26);
    }

    #[test]
    fn every_digit_is_mapped() {
        for digit in 0..=9 {
            assert!(
                ansi_keycode(&digit.to_string()).is_some(),
                "digit {digit} is unmapped"
            );
        }
    }
    /// Three different encodings of the same four keys is a genuine trap: a value from the wrong
    /// set registers a hotkey nobody can press. These are Carbon's, from `Events.h`.
    #[test]
    fn the_modifier_masks_are_carbons() {
        let binding = crate::Binding::parse("cmd+shift+k").expect("parses");
        assert_eq!(carbon_modifiers(&binding), 0x0100 | 0x0200);

        let all = crate::Binding::parse("cmd+shift+alt+ctrl+k").expect("parses");
        assert_eq!(carbon_modifiers(&all), 0x0100 | 0x0200 | 0x0800 | 0x1000);
    }

    /// Order cannot change the mask, since the binding sorts its modifiers.
    #[test]
    fn the_mask_does_not_depend_on_how_it_was_typed() {
        let a = crate::Binding::parse("shift+cmd+k").expect("parses");
        let b = crate::Binding::parse("cmd+shift+k").expect("parses");
        assert_eq!(carbon_modifiers(&a), carbon_modifiers(&b));
    }

    /// Every modifier has to contribute, or a four-key chord registers as a three-key one.
    #[test]
    fn every_modifier_sets_a_distinct_bit() {
        let bits: std::collections::BTreeSet<u32> = ["cmd+k", "shift+k", "alt+k", "ctrl+k"]
            .iter()
            .map(|raw| carbon_modifiers(&crate::Binding::parse(raw).expect("parses")))
            .collect();
        assert_eq!(bits.len(), 4);
        assert!(!bits.contains(&0));
    }
}
