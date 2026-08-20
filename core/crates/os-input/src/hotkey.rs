//! Global hotkeys, registered in one place.
//!
//! # Why one registry
//!
//! Four features registering hotkeys independently produces the bug where enabling one silently
//! breaks another and the user has no way to see why. One registry means a conflict is a refusal
//! with both names in it.
//!
//! # Two kinds of conflict, and only one of them is ours
//!
//! A collision between two Notewise bindings is detectable here, before anything is registered with
//! the OS. A collision with *another application* is not — the OS simply refuses, and all this layer
//! can do is report which binding failed and why. Keeping the two distinct matters because the fixes
//! differ: one is a setting in this app, the other is a setting in somebody else's.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A modifier key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modifier {
    /// Command on macOS, Windows key elsewhere.
    Super,
    Ctrl,
    Alt,
    Shift,
}

impl Modifier {
    fn token(&self) -> &'static str {
        match self {
            Modifier::Super => "super",
            Modifier::Ctrl => "ctrl",
            Modifier::Alt => "alt",
            Modifier::Shift => "shift",
        }
    }

    fn parse(token: &str) -> Option<Self> {
        Some(match token.trim().to_ascii_lowercase().as_str() {
            // The aliases people actually type. A binding a user cannot express is a binding they
            // will not set.
            "super" | "cmd" | "command" | "meta" | "win" => Modifier::Super,
            "ctrl" | "control" => Modifier::Ctrl,
            "alt" | "option" | "opt" => Modifier::Alt,
            "shift" => Modifier::Shift,
            _ => return None,
        })
    }
}

/// A key combination.
///
/// Modifiers are held in a sorted set, so `cmd+shift+k` and `shift+cmd+k` are the same binding.
/// Comparing them as written would let two settings collide without either looking wrong.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Binding {
    modifiers: Vec<Modifier>,
    /// The non-modifier key, lowercased.
    key: String,
}

impl Binding {
    /// Parse something like `cmd+shift+k`.
    pub fn parse(raw: &str) -> Result<Self, HotkeyError> {
        let mut modifiers = Vec::new();
        let mut key = None;

        for token in raw.split('+').map(str::trim).filter(|t| !t.is_empty()) {
            match Modifier::parse(token) {
                Some(modifier) => {
                    if !modifiers.contains(&modifier) {
                        modifiers.push(modifier);
                    }
                }
                None => {
                    if key.is_some() {
                        return Err(HotkeyError::Malformed(format!(
                            "'{raw}' names more than one non-modifier key"
                        )));
                    }
                    key = Some(token.to_ascii_lowercase());
                }
            }
        }

        let key = key
            .ok_or_else(|| HotkeyError::Malformed(format!("'{raw}' has no key, only modifiers")))?;

        // A bare letter would fire whenever the user typed it. The OS would refuse it anyway, but
        // refusing here means the message names the problem instead of reporting a failed
        // registration.
        if modifiers.is_empty() {
            return Err(HotkeyError::Malformed(format!(
                "'{raw}' needs at least one modifier, or it would fire while typing"
            )));
        }

        modifiers.sort();
        Ok(Self { modifiers, key })
    }

    pub fn modifiers(&self) -> &[Modifier] {
        &self.modifiers
    }

    pub fn key(&self) -> &str {
        &self.key
    }
}

impl std::fmt::Display for Binding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for modifier in &self.modifiers {
            write!(f, "{}+", modifier.token())?;
        }
        write!(f, "{}", self.key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HotkeyError {
    #[error("{0}")]
    Malformed(String),

    /// Two Notewise features want the same combination.
    #[error("'{binding}' is already used by {holder}")]
    Conflict { binding: String, holder: String },

    /// The OS refused, which usually means another application holds it.
    #[error("the system refused '{binding}': another application may already use it")]
    RefusedByOs { binding: String },
}

/// Every hotkey this app has claimed.
#[derive(Debug, Default)]
pub struct HotkeyRegistry {
    /// Binding to the feature holding it.
    claimed: BTreeMap<Binding, String>,
}

impl HotkeyRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim a binding for a feature.
    ///
    /// Refuses a collision rather than replacing the existing holder: silently taking a binding from
    /// another feature is the failure this registry exists to prevent.
    pub fn claim(&mut self, feature: &str, binding: Binding) -> Result<(), HotkeyError> {
        if let Some(holder) = self.claimed.get(&binding) {
            // Re-claiming your own binding is idempotent, which matters because settings get saved
            // repeatedly and a no-op save should not fail.
            if holder == feature {
                return Ok(());
            }
            return Err(HotkeyError::Conflict {
                binding: binding.to_string(),
                holder: holder.clone(),
            });
        }

        self.claimed.insert(binding, feature.to_string());
        Ok(())
    }

    /// Move a feature to a different binding, freeing its old one.
    ///
    /// One operation rather than release-then-claim, so a rebind that collides leaves the feature on
    /// the binding it already had instead of on none.
    pub fn rebind(&mut self, feature: &str, binding: Binding) -> Result<(), HotkeyError> {
        if let Some(holder) = self.claimed.get(&binding) {
            if holder != feature {
                return Err(HotkeyError::Conflict {
                    binding: binding.to_string(),
                    holder: holder.clone(),
                });
            }
            return Ok(());
        }

        self.claimed.retain(|_, held| held != feature);
        self.claimed.insert(binding, feature.to_string());
        Ok(())
    }

    pub fn release(&mut self, feature: &str) {
        self.claimed.retain(|_, held| held != feature);
    }

    pub fn holder_of(&self, binding: &Binding) -> Option<&str> {
        self.claimed.get(binding).map(String::as_str)
    }

    pub fn binding_for(&self, feature: &str) -> Option<&Binding> {
        self.claimed
            .iter()
            .find(|(_, held)| held.as_str() == feature)
            .map(|(binding, _)| binding)
    }

    /// Every claim, for a settings screen.
    pub fn claims(&self) -> Vec<(&Binding, &str)> {
        self.claimed.iter().map(|(b, f)| (b, f.as_str())).collect()
    }

    pub fn len(&self) -> usize {
        self.claimed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.claimed.is_empty()
    }
}

/// Combinations to keep away from by default.
///
/// A hardcoded hotkey that collides with somebody's IDE is an uninstall, so the defaults avoid what
/// the host OS and common editors claim. This is advisory — a user who wants one of these can have
/// it, because it is their machine.
pub const AVOID_BY_DEFAULT: &[&str] = &[
    "super+space", // Spotlight, and most launchers
    "super+tab",   // application switching
    "super+c",     // copy
    "super+v",     // paste
    "super+q",     // quit
    "super+w",     // close
    "super+s",     // save
    "super+z",     // undo
    "ctrl+c",      // interrupt, in every terminal
    "super+shift+3",
    "super+shift+4", // screenshots
];

/// Whether a binding is one of the combinations the defaults avoid.
pub fn is_commonly_claimed(binding: &Binding) -> bool {
    let rendered = binding.to_string();
    AVOID_BY_DEFAULT
        .iter()
        .filter_map(|raw| Binding::parse(raw).ok())
        .any(|known| known.to_string() == rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(raw: &str) -> Binding {
        Binding::parse(raw).expect("parses")
    }

    /// Comparing them as written would let two settings collide without either looking wrong.
    #[test]
    fn modifier_order_does_not_change_a_binding() {
        assert_eq!(binding("cmd+shift+k"), binding("shift+cmd+k"));
        assert_eq!(binding("ctrl+alt+p"), binding("alt+ctrl+p"));
    }

    /// A binding a user cannot express is a binding they will not set.
    #[test]
    fn the_aliases_people_type_are_accepted() {
        for raw in ["cmd+k", "command+k", "meta+k", "super+k", "win+k"] {
            assert_eq!(binding(raw), binding("super+k"), "{raw}");
        }
        for raw in ["alt+k", "option+k", "opt+k"] {
            assert_eq!(binding(raw), binding("alt+k"), "{raw}");
        }
    }

    #[test]
    fn case_does_not_matter() {
        assert_eq!(binding("CMD+SHIFT+K"), binding("cmd+shift+k"));
    }

    #[test]
    fn a_repeated_modifier_is_not_two_modifiers() {
        assert_eq!(binding("cmd+cmd+k").modifiers().len(), 1);
    }

    /// A bare letter would fire whenever the user typed it.
    #[test]
    fn a_binding_needs_a_modifier() {
        let err = Binding::parse("k").expect_err("must refuse");
        assert!(err.to_string().contains("modifier"), "{err}");
    }

    #[test]
    fn a_binding_needs_a_key() {
        assert!(Binding::parse("cmd+shift").is_err());
        assert!(Binding::parse("").is_err());
        assert!(Binding::parse("+++").is_err());
    }

    #[test]
    fn two_non_modifier_keys_are_refused() {
        let err = Binding::parse("cmd+k+j").expect_err("must refuse");
        assert!(err.to_string().contains("more than one"), "{err}");
    }

    #[test]
    fn a_binding_renders_back_to_something_parseable() {
        let original = binding("shift+cmd+k");
        assert_eq!(binding(&original.to_string()), original);
    }

    /// The failure this registry exists to prevent.
    #[test]
    fn a_second_feature_cannot_take_a_claimed_binding() {
        let mut registry = HotkeyRegistry::new();
        registry
            .claim("dictation", binding("cmd+shift+d"))
            .expect("first");

        let err = registry
            .claim("assistant", binding("cmd+shift+d"))
            .expect_err("must refuse");

        match err {
            HotkeyError::Conflict { holder, binding } => {
                assert_eq!(holder, "dictation");
                assert!(binding.contains("shift"), "{binding}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            registry.holder_of(&binding("cmd+shift+d")),
            Some("dictation"),
            "the original holder keeps it"
        );
    }

    /// Settings get saved repeatedly; a no-op save must not fail.
    #[test]
    fn reclaiming_your_own_binding_is_idempotent() {
        let mut registry = HotkeyRegistry::new();
        registry
            .claim("dictation", binding("cmd+shift+d"))
            .expect("first");
        registry
            .claim("dictation", binding("cmd+shift+d"))
            .expect("again");
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn rebinding_frees_the_old_combination() {
        let mut registry = HotkeyRegistry::new();
        registry
            .claim("dictation", binding("cmd+shift+d"))
            .expect("claim");
        registry
            .rebind("dictation", binding("alt+space"))
            .expect("rebind");

        assert_eq!(registry.holder_of(&binding("cmd+shift+d")), None);
        assert_eq!(registry.holder_of(&binding("alt+space")), Some("dictation"));
        assert_eq!(registry.len(), 1, "one feature holds one binding");
    }

    /// A rebind that collides must leave the feature where it was, not on nothing.
    #[test]
    fn a_failed_rebind_does_not_strand_the_feature() {
        let mut registry = HotkeyRegistry::new();
        registry
            .claim("dictation", binding("cmd+shift+d"))
            .expect("claim");
        registry
            .claim("assistant", binding("cmd+slash"))
            .expect("claim");

        assert!(registry.rebind("dictation", binding("cmd+slash")).is_err());
        assert_eq!(
            registry.binding_for("dictation").map(ToString::to_string),
            Some("super+shift+d".to_string()),
            "it still has the binding it started with"
        );
    }

    #[test]
    fn releasing_a_feature_frees_its_binding() {
        let mut registry = HotkeyRegistry::new();
        registry
            .claim("dictation", binding("cmd+shift+d"))
            .expect("claim");
        registry.release("dictation");

        assert!(registry.is_empty());
        registry
            .claim("assistant", binding("cmd+shift+d"))
            .expect("now free");
    }

    #[test]
    fn claims_are_listable_for_a_settings_screen() {
        let mut registry = HotkeyRegistry::new();
        registry
            .claim("dictation", binding("cmd+shift+d"))
            .expect("claim");
        registry
            .claim("assistant", binding("cmd+slash"))
            .expect("claim");

        let claims = registry.claims();
        assert_eq!(claims.len(), 2);
        assert!(claims.iter().any(|(_, f)| *f == "dictation"));
    }

    /// A hardcoded hotkey that collides with somebody's IDE is an uninstall.
    #[test]
    fn the_combinations_the_defaults_avoid_are_recognised() {
        assert!(is_commonly_claimed(&binding("cmd+space")));
        assert!(
            is_commonly_claimed(&binding("space+cmd")),
            "order-independent"
        );
        assert!(is_commonly_claimed(&binding("ctrl+c")));
        assert!(!is_commonly_claimed(&binding("cmd+shift+d")));
    }

    /// Advisory, not enforced — it is the user's machine.
    #[test]
    fn a_commonly_claimed_binding_can_still_be_set_deliberately() {
        let mut registry = HotkeyRegistry::new();
        registry
            .claim("dictation", binding("cmd+space"))
            .expect("a user who wants it can have it");
    }

    #[test]
    fn every_avoided_default_is_itself_a_valid_binding() {
        for raw in AVOID_BY_DEFAULT {
            Binding::parse(raw).unwrap_or_else(|e| panic!("{raw} should parse: {e}"));
        }
    }
}
