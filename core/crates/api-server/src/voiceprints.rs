//! Storing what a person sounds like, and the switch that decides whether to.
//!
//! # Why this is a setting and not just a feature
//!
//! A voice print is a biometric identifier, and the people it identifies are the *other*
//! participants in a meeting. They did not install this app, were not asked, and in most cases
//! will never know the recording happened at all. That is a different situation from every other
//! thing the product stores, all of which is the user's own material.
//!
//! So it is off until switched on, and the switch says what it does. Off is also what the
//! comparable products do: Meetily, checked directly, keeps a per-meeting `speaker` label and has
//! no person table, no voice column and no biometric storage of any kind.
//!
//! What being on buys is the thing labels cannot: recognising the same colleague across meetings,
//! including in-person ones where no platform can be asked who was talking.
//!
//! # What is stored
//!
//! A vector of floats and the name of the model that produced it — never audio. The vector cannot
//! be played back or reconstructed into speech. It is still an identifier, which is the point of
//! the switch.

use serde::{Deserialize, Serialize};

/// The setting key, alongside the other engine-side preferences.
pub const ENABLED_KEY: &str = "voiceprints_enabled";

/// Whether voice prints may be stored.
///
/// Absent means off. A missing setting must never be read as consent — an upgrade that
/// introduced this key should not silently begin enrolling people.
pub fn enabled(settings: &notewise_storage::SettingsRepository<'_>) -> bool {
    settings
        .get(ENABLED_KEY)
        .ok()
        .flatten()
        .is_some_and(|v| v == "true")
}

#[derive(Debug, Serialize)]
pub struct VoiceprintStatus {
    pub enabled: bool,
    /// How many people currently have one stored.
    ///
    /// Shown so the switch is not the only thing a user can see. "Off" with eleven prints still
    /// on disk would be a misleading screen.
    pub stored: usize,
}

#[derive(Debug, Deserialize)]
pub struct SetEnabled {
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use notewise_storage::{Database, SettingsRepository};

    #[test]
    fn a_missing_setting_is_off() {
        let db = Database::open_in_memory().expect("db");
        let settings = SettingsRepository::new(&db);
        assert!(
            !enabled(&settings),
            "an upgrade that adds this key must not start enrolling people by itself"
        );
    }

    #[test]
    fn only_an_explicit_true_is_on() {
        let db = Database::open_in_memory().expect("db");
        let settings = SettingsRepository::new(&db);

        for value in ["false", "", "1", "yes", "TRUE"] {
            settings.set(ENABLED_KEY, value).expect("set");
            assert!(!enabled(&settings), "{value:?} must not read as consent");
        }

        settings.set(ENABLED_KEY, "true").expect("set");
        assert!(enabled(&settings));
    }
}
