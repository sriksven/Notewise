//! Version vectors.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Identifies one device in a user's account.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceId(String);

impl DeviceId {
    pub fn new(id: impl Into<String>) -> Self {
        DeviceId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How two versions relate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ordering {
    /// Identical.
    Equal,
    /// The left version descends from the right — it has strictly newer information.
    Descends,
    /// The right version descends from the left.
    Precedes,
    /// Neither descends from the other: both changed independently. A real conflict.
    Diverged,
}

/// A version vector: one counter per device that has written the record.
///
/// A vector rather than a single counter or a timestamp because only a vector can
/// distinguish "B already has A's change" from "A and B both edited independently". A
/// last-write-wins timestamp answers that question wrong whenever clocks disagree, and
/// silently discards the loser's edit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Version {
    counters: BTreeMap<DeviceId, u64>,
}

impl Version {
    pub fn new() -> Self {
        Self::default()
    }

    /// A version representing one edit on one device.
    pub fn initial(device: &DeviceId) -> Self {
        let mut version = Self::new();
        version.increment(device);
        version
    }

    /// Record a local edit.
    pub fn increment(&mut self, device: &DeviceId) {
        *self.counters.entry(device.clone()).or_insert(0) += 1;
    }

    pub fn counter_for(&self, device: &DeviceId) -> u64 {
        self.counters.get(device).copied().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.counters.is_empty()
    }

    /// Devices that have written this record.
    pub fn devices(&self) -> impl Iterator<Item = &DeviceId> {
        self.counters.keys()
    }

    /// Compare against another version.
    pub fn compare(&self, other: &Version) -> Ordering {
        let mut self_ahead = false;
        let mut other_ahead = false;

        // Every device either side knows about — a device missing from one side counts as 0.
        for device in self.counters.keys().chain(other.counters.keys()) {
            let mine = self.counter_for(device);
            let theirs = other.counter_for(device);

            if mine > theirs {
                self_ahead = true;
            } else if theirs > mine {
                other_ahead = true;
            }
        }

        match (self_ahead, other_ahead) {
            (false, false) => Ordering::Equal,
            (true, false) => Ordering::Descends,
            (false, true) => Ordering::Precedes,
            // Both sides have changes the other lacks.
            (true, true) => Ordering::Diverged,
        }
    }

    /// Merge two versions, taking the highest counter per device.
    ///
    /// The version of a record that incorporates both sides' history.
    pub fn merged(&self, other: &Version) -> Version {
        let mut counters = self.counters.clone();
        for (device, counter) in &other.counters {
            let entry = counters.entry(device.clone()).or_insert(0);
            *entry = (*entry).max(*counter);
        }
        Version { counters }
    }

    /// Whether these versions conflict.
    pub fn conflicts_with(&self, other: &Version) -> bool {
        self.compare(other) == Ordering::Diverged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn laptop() -> DeviceId {
        DeviceId::new("laptop")
    }
    fn phone() -> DeviceId {
        DeviceId::new("phone")
    }

    #[test]
    fn a_fresh_version_is_empty() {
        assert!(Version::new().is_empty());
        assert_eq!(Version::new().counter_for(&laptop()), 0);
    }

    #[test]
    fn incrementing_advances_only_that_device() {
        let mut version = Version::new();
        version.increment(&laptop());
        version.increment(&laptop());

        assert_eq!(version.counter_for(&laptop()), 2);
        assert_eq!(version.counter_for(&phone()), 0);
    }

    #[test]
    fn identical_versions_are_equal() {
        let a = Version::initial(&laptop());
        let b = Version::initial(&laptop());
        assert_eq!(a.compare(&b), Ordering::Equal);
        assert!(!a.conflicts_with(&b));
    }

    #[test]
    fn a_later_edit_on_the_same_device_descends() {
        let earlier = Version::initial(&laptop());
        let mut later = earlier.clone();
        later.increment(&laptop());

        assert_eq!(later.compare(&earlier), Ordering::Descends);
        assert_eq!(earlier.compare(&later), Ordering::Precedes);
        assert!(
            !later.conflicts_with(&earlier),
            "sequential edits on one device are not a conflict"
        );
    }

    #[test]
    fn independent_edits_on_two_devices_diverge() {
        // The case a timestamp gets wrong: both edited from a common state.
        let base = Version::initial(&laptop());

        let mut laptop_edit = base.clone();
        laptop_edit.increment(&laptop());

        let mut phone_edit = base.clone();
        phone_edit.increment(&phone());

        assert_eq!(laptop_edit.compare(&phone_edit), Ordering::Diverged);
        assert!(laptop_edit.conflicts_with(&phone_edit));
    }

    #[test]
    fn an_edit_made_after_syncing_does_not_conflict() {
        // The distinction that matters: the phone already had the laptop's change.
        let mut laptop_edit = Version::initial(&laptop());
        laptop_edit.increment(&laptop());

        let mut phone_after_sync = laptop_edit.clone();
        phone_after_sync.increment(&phone());

        assert!(
            !phone_after_sync.conflicts_with(&laptop_edit),
            "the phone had already seen the laptop's edit"
        );
        assert_eq!(phone_after_sync.compare(&laptop_edit), Ordering::Descends);
    }

    #[test]
    fn merging_takes_the_highest_counter_per_device() {
        let mut a = Version::new();
        a.increment(&laptop());
        a.increment(&laptop());
        a.increment(&phone());

        let mut b = Version::new();
        b.increment(&laptop());
        b.increment(&phone());
        b.increment(&phone());
        b.increment(&phone());

        let merged = a.merged(&b);
        assert_eq!(merged.counter_for(&laptop()), 2);
        assert_eq!(merged.counter_for(&phone()), 3);
    }

    #[test]
    fn a_merged_version_descends_from_both_sides() {
        let base = Version::initial(&laptop());
        let mut left = base.clone();
        left.increment(&laptop());
        let mut right = base.clone();
        right.increment(&phone());

        let merged = left.merged(&right);

        assert_eq!(merged.compare(&left), Ordering::Descends);
        assert_eq!(merged.compare(&right), Ordering::Descends);
        assert!(!merged.conflicts_with(&left));
    }

    #[test]
    fn merging_is_commutative() {
        let mut a = Version::new();
        a.increment(&laptop());
        let mut b = Version::new();
        b.increment(&phone());

        assert_eq!(a.merged(&b), b.merged(&a));
    }

    #[test]
    fn merging_is_idempotent() {
        let version = Version::initial(&laptop());
        assert_eq!(version.merged(&version), version);
    }

    #[test]
    fn a_device_absent_from_one_side_counts_as_zero() {
        let laptop_only = Version::initial(&laptop());
        let phone_only = Version::initial(&phone());

        assert_eq!(laptop_only.compare(&phone_only), Ordering::Diverged);
    }

    #[test]
    fn an_empty_version_precedes_any_edit() {
        assert_eq!(
            Version::new().compare(&Version::initial(&laptop())),
            Ordering::Precedes
        );
    }

    #[test]
    fn three_devices_are_handled() {
        let desktop = DeviceId::new("desktop");
        let base = Version::initial(&laptop());

        let mut from_phone = base.clone();
        from_phone.increment(&phone());
        let mut from_desktop = base.clone();
        from_desktop.increment(&desktop);

        assert!(from_phone.conflicts_with(&from_desktop));

        let merged = from_phone.merged(&from_desktop);
        assert_eq!(merged.devices().count(), 3);
    }

    #[test]
    fn versions_round_trip_through_json() {
        let mut version = Version::initial(&laptop());
        version.increment(&phone());

        let json = serde_json::to_string(&version).unwrap();
        assert_eq!(serde_json::from_str::<Version>(&json).unwrap(), version);
    }
}
