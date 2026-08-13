//! Conflict resolution.
//!
//! The rule that matters: **a genuine conflict never silently discards a side.** Losing a
//! user's edit without telling them is the worst outcome sync can produce — worse than a
//! duplicate, which they can see and delete.

use serde::{Deserialize, Serialize};

use crate::version::{DeviceId, Ordering, Version};

/// What to do when two devices edited the same record independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// Keep both, as separate records.
    ///
    /// The default, and the only option that cannot lose work. A duplicate is visible and
    /// fixable; a silently dropped edit is neither.
    #[default]
    KeepBoth,

    /// Keep the local copy and discard the remote one.
    PreferLocal,

    /// Keep the remote copy and discard the local one.
    PreferRemote,

    /// Surface the conflict and change nothing until the user decides.
    AskUser,
}

impl ConflictPolicy {
    /// Whether this policy can discard an edit the user made.
    ///
    /// Used to warn before a destructive policy is selected.
    pub fn can_lose_data(&self) -> bool {
        matches!(
            self,
            ConflictPolicy::PreferLocal | ConflictPolicy::PreferRemote
        )
    }
}

/// The outcome of merging one record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Both sides already agree.
    Unchanged,
    /// The remote descends from the local copy — take it.
    TakeRemote { version: Version },
    /// The local copy descends from the remote — push it.
    KeepLocal { version: Version },
    /// A real conflict, resolved by policy.
    Resolved {
        policy: ConflictPolicy,
        version: Version,
        /// Whether an edit was discarded. Surfaced so the UI can tell the user.
        discarded_an_edit: bool,
    },
    /// A real conflict the user must resolve.
    NeedsUser { local: Version, remote: Version },
}

impl Resolution {
    /// Whether applying this requires writing anything locally.
    pub fn requires_local_write(&self) -> bool {
        matches!(
            self,
            Resolution::TakeRemote { .. } | Resolution::Resolved { .. }
        )
    }

    /// Whether this needs the user's attention.
    pub fn needs_attention(&self) -> bool {
        match self {
            Resolution::NeedsUser { .. } => true,
            Resolution::Resolved {
                discarded_an_edit, ..
            } => *discarded_an_edit,
            _ => false,
        }
    }
}

/// Merges record versions according to a policy.
#[derive(Debug, Clone)]
pub struct SyncEngine {
    device: DeviceId,
    policy: ConflictPolicy,
}

impl SyncEngine {
    pub fn new(device: DeviceId) -> Self {
        Self {
            device,
            policy: ConflictPolicy::default(),
        }
    }

    pub fn with_policy(mut self, policy: ConflictPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn device(&self) -> &DeviceId {
        &self.device
    }

    pub fn policy(&self) -> ConflictPolicy {
        self.policy
    }

    /// Record a local edit, advancing the version.
    pub fn edit(&self, version: &Version) -> Version {
        let mut next = version.clone();
        next.increment(&self.device);
        next
    }

    /// Decide what to do with a local and a remote version of one record.
    pub fn resolve(&self, local: &Version, remote: &Version) -> Resolution {
        match local.compare(remote) {
            Ordering::Equal => Resolution::Unchanged,

            // The remote has everything local has, plus more.
            Ordering::Precedes => Resolution::TakeRemote {
                version: remote.clone(),
            },

            // Local is ahead; the remote has nothing new.
            Ordering::Descends => Resolution::KeepLocal {
                version: local.clone(),
            },

            Ordering::Diverged => match self.policy {
                ConflictPolicy::AskUser => Resolution::NeedsUser {
                    local: local.clone(),
                    remote: remote.clone(),
                },
                // Keeping both writes a version descending from each side, so the conflict
                // is not rediscovered on the next sync.
                ConflictPolicy::KeepBoth => Resolution::Resolved {
                    policy: ConflictPolicy::KeepBoth,
                    version: local.merged(remote),
                    discarded_an_edit: false,
                },
                ConflictPolicy::PreferLocal => Resolution::Resolved {
                    policy: ConflictPolicy::PreferLocal,
                    version: local.merged(remote),
                    discarded_an_edit: true,
                },
                ConflictPolicy::PreferRemote => Resolution::Resolved {
                    policy: ConflictPolicy::PreferRemote,
                    version: local.merged(remote),
                    discarded_an_edit: true,
                },
            },
        }
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

    fn engine() -> SyncEngine {
        SyncEngine::new(laptop())
    }

    /// A version pair that genuinely diverged from a common base.
    fn diverged() -> (Version, Version) {
        let base = Version::initial(&laptop());

        let mut local = base.clone();
        local.increment(&laptop());

        let mut remote = base;
        remote.increment(&phone());

        (local, remote)
    }

    #[test]
    fn the_default_policy_cannot_lose_data() {
        assert_eq!(ConflictPolicy::default(), ConflictPolicy::KeepBoth);
        assert!(!ConflictPolicy::default().can_lose_data());
    }

    #[test]
    fn destructive_policies_are_flagged_as_such() {
        assert!(ConflictPolicy::PreferLocal.can_lose_data());
        assert!(ConflictPolicy::PreferRemote.can_lose_data());
        assert!(!ConflictPolicy::KeepBoth.can_lose_data());
        assert!(!ConflictPolicy::AskUser.can_lose_data());
    }

    #[test]
    fn identical_versions_need_no_work() {
        let version = Version::initial(&laptop());
        let resolution = engine().resolve(&version, &version);

        assert_eq!(resolution, Resolution::Unchanged);
        assert!(!resolution.requires_local_write());
        assert!(!resolution.needs_attention());
    }

    #[test]
    fn a_newer_remote_is_taken() {
        let local = Version::initial(&laptop());
        let mut remote = local.clone();
        remote.increment(&phone());

        let resolution = engine().resolve(&local, &remote);

        assert!(matches!(resolution, Resolution::TakeRemote { .. }));
        assert!(resolution.requires_local_write());
        assert!(!resolution.needs_attention(), "not a conflict");
    }

    #[test]
    fn a_newer_local_is_kept_without_a_write() {
        let remote = Version::initial(&laptop());
        let local = engine().edit(&remote);

        let resolution = engine().resolve(&local, &remote);

        assert!(matches!(resolution, Resolution::KeepLocal { .. }));
        assert!(!resolution.requires_local_write());
    }

    #[test]
    fn the_default_policy_keeps_both_sides_of_a_conflict() {
        let (local, remote) = diverged();
        let resolution = engine().resolve(&local, &remote);

        match resolution {
            Resolution::Resolved {
                policy,
                discarded_an_edit,
                ..
            } => {
                assert_eq!(policy, ConflictPolicy::KeepBoth);
                assert!(!discarded_an_edit, "the default must never drop an edit");
            }
            other => panic!("expected a resolved conflict, got {other:?}"),
        }
    }

    #[test]
    fn a_destructive_policy_reports_that_it_discarded_an_edit() {
        // The user must be told; silently losing their work is the worst outcome.
        let (local, remote) = diverged();

        for policy in [ConflictPolicy::PreferLocal, ConflictPolicy::PreferRemote] {
            let resolution = engine().with_policy(policy).resolve(&local, &remote);

            match resolution {
                Resolution::Resolved {
                    discarded_an_edit, ..
                } => assert!(discarded_an_edit, "{policy:?} silently dropped an edit"),
                other => panic!("expected a resolved conflict, got {other:?}"),
            }
            assert!(resolution.needs_attention());
        }
    }

    #[test]
    fn ask_user_changes_nothing_until_the_user_decides() {
        let (local, remote) = diverged();
        let resolution = engine()
            .with_policy(ConflictPolicy::AskUser)
            .resolve(&local, &remote);

        assert!(matches!(resolution, Resolution::NeedsUser { .. }));
        assert!(
            !resolution.requires_local_write(),
            "nothing may be written before the user chooses"
        );
        assert!(resolution.needs_attention());
    }

    #[test]
    fn resolving_a_conflict_does_not_leave_it_to_be_rediscovered() {
        // The merged version must descend from both sides, or the next sync sees the same
        // conflict again and the user is asked forever.
        let (local, remote) = diverged();

        let Resolution::Resolved { version, .. } = engine().resolve(&local, &remote) else {
            panic!("expected a resolution");
        };

        assert!(!version.conflicts_with(&local));
        assert!(!version.conflicts_with(&remote));
        assert_eq!(
            engine().resolve(&version, &remote),
            Resolution::KeepLocal {
                version: version.clone()
            }
        );
    }

    #[test]
    fn an_edit_advances_only_the_local_device() {
        let engine = engine();
        let before = Version::initial(&phone());
        let after = engine.edit(&before);

        assert_eq!(after.counter_for(&laptop()), 1);
        assert_eq!(
            after.counter_for(&phone()),
            before.counter_for(&phone()),
            "another device's counter must not move"
        );
    }

    #[test]
    fn sequential_edits_across_devices_never_conflict() {
        // laptop edits, syncs, phone edits from the synced state: no conflict at any point.
        let laptop_engine = SyncEngine::new(laptop());
        let phone_engine = SyncEngine::new(phone());

        let v1 = laptop_engine.edit(&Version::new());
        assert!(matches!(
            phone_engine.resolve(&Version::new(), &v1),
            Resolution::TakeRemote { .. }
        ));

        let v2 = phone_engine.edit(&v1);
        assert!(matches!(
            laptop_engine.resolve(&v1, &v2),
            Resolution::TakeRemote { .. }
        ));
    }

    #[test]
    fn a_first_sync_of_a_new_record_is_not_a_conflict() {
        // Local has nothing; the remote has a record. This must not look like divergence.
        let resolution = engine().resolve(&Version::new(), &Version::initial(&phone()));
        assert!(matches!(resolution, Resolution::TakeRemote { .. }));
    }

    #[test]
    fn policies_round_trip_through_json() {
        for policy in [
            ConflictPolicy::KeepBoth,
            ConflictPolicy::PreferLocal,
            ConflictPolicy::PreferRemote,
            ConflictPolicy::AskUser,
        ] {
            let json = serde_json::to_string(&policy).unwrap();
            assert_eq!(
                serde_json::from_str::<ConflictPolicy>(&json).unwrap(),
                policy
            );
        }
    }
}
