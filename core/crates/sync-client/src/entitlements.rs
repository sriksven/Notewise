//! What the user has paid for.
//!
//! # Where a check is allowed to be
//!
//! At the boundary where a request would leave the machine, and nowhere else. `sync-client` before
//! it syncs; a hosted-inference backend before it calls out. That is the whole list.
//!
//! No local feature has an entitlement check anywhere in its path — not disabled-when-unlicensed,
//! not degraded, not even *read*. A local feature that reads entitlement state is one refactor away
//! from depending on it, and the promise being protected is that recording, transcription,
//! summaries, search and every connector keep working forever regardless of what has been paid.
//!
//! This is enforced structurally rather than by intention: [`tests::no_local_crate_reads_entitlement_state`]
//! reads the source of every local crate and fails if one mentions this module.
//!
//! # Why it fails open
//!
//! A local-first product has to work on a plane, in a basement, and during our outage. An
//! entitlement system that fails closed converts our infrastructure problem into the user's lost
//! afternoon — and it does so for the paid features they are current on, which is the worst possible
//! moment to be wrong.
//!
//! So a cached grant is honoured while it is valid, and for a long grace period after refresh stops
//! working. Somebody could stay offline to keep Pro past expiry; that is an acceptable loss.
//! Punishing every honest user for it is not, and the features at stake need our servers anyway —
//! sync with nothing to sync to is not much of a theft.
//!
//! # Why the capability list is a closed enum
//!
//! A typo in a string is a silently ungated capability. A typo in a variant is a compile error. The
//! set being closed also means the things that *can* be gated are auditable in one place, which is
//! what makes the rule above checkable rather than aspirational.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::{Result, SyncError};

/// How long a grant keeps working after its expiry when refresh cannot reach us.
///
/// Thirty days. Long enough to cover an outage, a holiday, or a laptop that stays off a network for
/// a month; short enough that an abandoned subscription eventually stops.
pub const GRACE: Duration = Duration::days(30);

/// Something the user pays for.
///
/// Every variant is a thing that runs on our infrastructure. That is not a coincidence — it is the
/// rule: anything running on the user's machine is free, so it cannot appear here. A local
/// capability added to this enum would be a bug visible in review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaidCapability {
    /// Syncing a workspace between devices through our service.
    Sync,
    /// Transcription or completion running on our hardware rather than the user's.
    HostedInference,
    /// A shared workspace, comments, and external access.
    TeamWorkspace,
}

impl PaidCapability {
    pub const ALL: &'static [PaidCapability] = &[
        PaidCapability::Sync,
        PaidCapability::HostedInference,
        PaidCapability::TeamWorkspace,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            PaidCapability::Sync => "sync",
            PaidCapability::HostedInference => "hosted_inference",
            PaidCapability::TeamWorkspace => "team_workspace",
        }
    }
}

/// What the UI should say about the subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntitlementState {
    /// Current, and not near expiry.
    Active { until: DateTime<Utc> },
    /// Past expiry but inside the grace period.
    ///
    /// A separate state so the UI can say so. Showing "Active" until the moment everything stops is
    /// how a user finds out about billing from a broken feature.
    Grace { until: DateTime<Utc> },
    /// Past expiry and past grace.
    Expired,
    /// No grant, or one that did not verify.
    Unlicensed,
}

/// The signed part of a grant. Exactly what the issuer signs, byte for byte.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantPayload {
    /// Who it is for. Opaque to this crate.
    pub subject: String,
    pub capabilities: BTreeSet<PaidCapability>,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// A grant as stored on disk: the exact signed bytes, plus the signature over them.
///
/// The payload is kept as a string rather than re-serialized before verifying. Round-tripping
/// through a struct can change key order or number formatting, and then a perfectly good signature
/// fails to verify for reasons nobody can see.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedGrant {
    /// JSON of a [`GrantPayload`], verbatim.
    pub payload: String,
    /// Hex ed25519 signature over `payload`'s bytes.
    pub signature: String,
}

/// Answers what the user has paid for.
#[derive(Debug, Clone)]
pub struct Entitlements {
    grant: Option<GrantPayload>,
}

impl Entitlements {
    /// Nothing paid for. The free product works.
    pub fn unlicensed() -> Self {
        Self { grant: None }
    }

    /// Verify a grant and hold it.
    ///
    /// A grant that does not verify is treated as no grant at all — never as active. The failure is
    /// returned so a caller can log it, but the resulting `Entitlements` is safe either way.
    pub fn verify(signed: &SignedGrant, key: &VerifyingKey) -> Result<Self> {
        let raw = hex::decode(signed.signature.trim())
            .map_err(|_| SyncError::Entitlement("the grant signature is not hex".into()))?;
        let bytes: [u8; 64] = raw.try_into().map_err(|_| {
            SyncError::Entitlement("the grant signature is the wrong length".into())
        })?;

        key.verify(signed.payload.as_bytes(), &Signature::from_bytes(&bytes))
            .map_err(|_| SyncError::Entitlement("the grant signature does not verify".into()))?;

        let payload: GrantPayload = serde_json::from_str(&signed.payload)
            .map_err(|e| SyncError::Entitlement(format!("the grant is malformed: {e}")))?;

        if payload.expires_at < payload.issued_at {
            return Err(SyncError::Entitlement(
                "the grant expires before it was issued".into(),
            ));
        }

        Ok(Self {
            grant: Some(payload),
        })
    }

    /// Load and verify the grant cached at `path`.
    ///
    /// A missing file is [`Entitlements::unlicensed`], not an error: not having paid is an ordinary
    /// state, and the whole product works in it.
    pub fn load(path: &Path, key: &VerifyingKey) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::unlicensed());
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|e| SyncError::Entitlement(format!("could not read the grant: {e}")))?;
        let signed: SignedGrant = serde_json::from_str(&raw)
            .map_err(|e| SyncError::Entitlement(format!("the cached grant is malformed: {e}")))?;
        Self::verify(&signed, key)
    }

    /// Where the cached grant lives, given the workspace directory.
    pub fn cache_path(dir: &Path) -> PathBuf {
        dir.join("entitlement.json")
    }

    /// The state as of `now`.
    ///
    /// `now` is passed in rather than read here, so every boundary — the day before expiry, the day
    /// after, the last day of grace — is testable without waiting for it.
    pub fn state_at(&self, now: DateTime<Utc>) -> EntitlementState {
        let Some(grant) = &self.grant else {
            return EntitlementState::Unlicensed;
        };

        // A clock moved backwards must not extend anything. Taking the later of now and the grant's
        // own issue time means winding the system clock back can at worst make a grant look newer
        // than it is by its own timestamp, which the issuer controls.
        let now = now.max(grant.issued_at);

        if now <= grant.expires_at {
            return EntitlementState::Active {
                until: grant.expires_at,
            };
        }

        let grace_ends = grant.expires_at + GRACE;
        if now <= grace_ends {
            EntitlementState::Grace { until: grace_ends }
        } else {
            EntitlementState::Expired
        }
    }

    /// Whether a paid capability is available as of `now`.
    ///
    /// True during grace, which is the whole point of grace — see the module docs on failing open.
    pub fn granted_at(&self, capability: PaidCapability, now: DateTime<Utc>) -> bool {
        let Some(grant) = &self.grant else {
            return false;
        };
        if !grant.capabilities.contains(&capability) {
            return false;
        }
        matches!(
            self.state_at(now),
            EntitlementState::Active { .. } | EntitlementState::Grace { .. }
        )
    }

    /// Convenience over [`Self::granted_at`] using the system clock.
    pub fn granted(&self, capability: PaidCapability) -> bool {
        self.granted_at(capability, Utc::now())
    }

    /// Convenience over [`Self::state_at`] using the system clock.
    pub fn state(&self) -> EntitlementState {
        self.state_at(Utc::now())
    }

    /// What was granted, whether or not it is current. For a settings screen.
    pub fn capabilities(&self) -> BTreeSet<PaidCapability> {
        self.grant
            .as_ref()
            .map(|g| g.capabilities.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn keypair() -> (SigningKey, VerifyingKey) {
        // A fixed seed: these tests are about grant logic, not key generation, and a deterministic
        // key makes a failure reproducible.
        let signing = SigningKey::from_bytes(&[7u8; 32]);
        (signing.clone(), signing.verifying_key())
    }

    fn grant(
        signing: &SigningKey,
        capabilities: &[PaidCapability],
        issued: DateTime<Utc>,
        expires: DateTime<Utc>,
    ) -> SignedGrant {
        let payload = GrantPayload {
            subject: "someone@example.com".into(),
            capabilities: capabilities.iter().copied().collect(),
            issued_at: issued,
            expires_at: expires,
        };
        let json = serde_json::to_string(&payload).expect("serialize");
        let signature = signing.sign(json.as_bytes());
        SignedGrant {
            payload: json,
            signature: hex::encode(signature.to_bytes()),
        }
    }

    fn at(days: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + days * 86_400, 0).expect("timestamp")
    }

    #[test]
    fn with_no_grant_nothing_is_paid_for_and_that_is_not_an_error() {
        let e = Entitlements::unlicensed();
        assert_eq!(e.state_at(at(0)), EntitlementState::Unlicensed);
        for capability in PaidCapability::ALL {
            assert!(!e.granted_at(*capability, at(0)), "{capability:?}");
        }
    }

    #[test]
    fn a_current_grant_grants_only_what_it_names() {
        let (signing, key) = keypair();
        let signed = grant(&signing, &[PaidCapability::Sync], at(0), at(30));
        let e = Entitlements::verify(&signed, &key).expect("verifies");

        assert_eq!(
            e.state_at(at(1)),
            EntitlementState::Active { until: at(30) }
        );
        assert!(e.granted_at(PaidCapability::Sync, at(1)));
        assert!(
            !e.granted_at(PaidCapability::HostedInference, at(1)),
            "a grant for sync must not confer hosted inference"
        );
    }

    /// The boundaries, in one place, because off-by-one here means either cutting someone off a day
    /// early or handing out a free month.
    #[test]
    fn expiry_and_grace_boundaries() {
        let (signing, key) = keypair();
        let e = Entitlements::verify(
            &grant(&signing, &[PaidCapability::Sync], at(0), at(30)),
            &key,
        )
        .expect("verifies");

        assert!(matches!(
            e.state_at(at(30)),
            EntitlementState::Active { .. }
        ));
        // One second past expiry is grace, not death.
        assert!(matches!(
            e.state_at(at(30) + Duration::seconds(1)),
            EntitlementState::Grace { .. }
        ));
        assert!(matches!(e.state_at(at(60)), EntitlementState::Grace { .. }));
        assert!(matches!(
            e.state_at(at(30) + GRACE + Duration::seconds(1)),
            EntitlementState::Expired
        ));
    }

    #[test]
    fn a_capability_stays_available_through_grace() {
        let (signing, key) = keypair();
        let e = Entitlements::verify(
            &grant(&signing, &[PaidCapability::Sync], at(0), at(30)),
            &key,
        )
        .expect("verifies");

        assert!(
            e.granted_at(PaidCapability::Sync, at(45)),
            "failing closed during grace turns our outage into the user's problem"
        );
        assert!(!e.granted_at(PaidCapability::Sync, at(200)));
    }

    #[test]
    fn a_forged_grant_is_never_active() {
        let (signing, key) = keypair();
        let mut signed = grant(&signing, &[PaidCapability::Sync], at(0), at(30));

        // The obvious attack: widen the grant and keep the old signature. Tampering is asserted
        // rather than assumed — an earlier version of this test "forged" a payload it had not
        // actually changed, and passed for the wrong reason.
        let original = signed.payload.clone();
        signed.payload = signed
            .payload
            .replace("[\"sync\"]", "[\"sync\",\"hosted_inference\"]");
        assert_ne!(
            signed.payload, original,
            "the tamper must actually change it"
        );

        let err = Entitlements::verify(&signed, &key).expect_err("must not verify");
        assert!(matches!(err, SyncError::Entitlement(_)), "{err:?}");

        // And the same for extending the expiry, whatever serde's date format happens to be.
        let mut extended = grant(&signing, &[PaidCapability::Sync], at(0), at(30));
        let honest: GrantPayload = serde_json::from_str(&extended.payload).expect("parse");
        let mut greedy = honest.clone();
        greedy.expires_at = at(9_999);
        extended.payload = serde_json::to_string(&greedy).expect("serialize");
        assert_ne!(extended.payload, signed.payload);

        assert!(
            Entitlements::verify(&extended, &key).is_err(),
            "a re-signed-by-nobody expiry extension must be refused"
        );
    }

    #[test]
    fn a_grant_signed_by_the_wrong_key_is_rejected() {
        let (signing, _) = keypair();
        let other = SigningKey::from_bytes(&[9u8; 32]).verifying_key();
        let signed = grant(&signing, &[PaidCapability::Sync], at(0), at(30));

        assert!(Entitlements::verify(&signed, &other).is_err());
    }

    #[test]
    fn a_malformed_signature_is_reported_not_panicked() {
        let (signing, key) = keypair();
        let mut signed = grant(&signing, &[PaidCapability::Sync], at(0), at(30));

        for bad in ["", "not-hex", "aabb"] {
            signed.signature = bad.into();
            assert!(Entitlements::verify(&signed, &key).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_grant_that_expires_before_it_was_issued_is_rejected() {
        let (signing, key) = keypair();
        let signed = grant(&signing, &[PaidCapability::Sync], at(30), at(0));
        assert!(Entitlements::verify(&signed, &key).is_err());
    }

    /// Winding the clock back must not extend grace indefinitely.
    #[test]
    fn a_backwards_clock_cannot_resurrect_an_expired_grant() {
        let (signing, key) = keypair();
        // Issued far in the future relative to the "now" we will ask about.
        let e = Entitlements::verify(
            &grant(&signing, &[PaidCapability::Sync], at(100), at(130)),
            &key,
        )
        .expect("verifies");

        // A user who sets their clock to before the issue date gets the issue date, not their lie.
        assert!(matches!(e.state_at(at(0)), EntitlementState::Active { .. }));
        // And they cannot make an exhausted grace period fresh again.
        let expired = Entitlements::verify(
            &grant(&signing, &[PaidCapability::Sync], at(0), at(1)),
            &key,
        )
        .expect("verifies");
        assert_eq!(expired.state_at(at(500)), EntitlementState::Expired);
    }

    #[test]
    fn a_missing_cache_file_is_unlicensed_not_an_error() {
        let (_, key) = keypair();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = Entitlements::cache_path(dir.path());

        let e = Entitlements::load(&path, &key).expect("loads");
        assert_eq!(e.state_at(at(0)), EntitlementState::Unlicensed);
    }

    #[test]
    fn a_cached_grant_round_trips_through_disk() {
        let (signing, key) = keypair();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = Entitlements::cache_path(dir.path());

        let signed = grant(&signing, &[PaidCapability::Sync], at(0), at(30));
        std::fs::write(&path, serde_json::to_string(&signed).expect("json")).expect("write");

        let e = Entitlements::load(&path, &key).expect("loads");
        assert!(e.granted_at(PaidCapability::Sync, at(1)));
    }

    #[test]
    fn a_corrupt_cache_file_is_reported_and_grants_nothing() {
        let (_, key) = keypair();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = Entitlements::cache_path(dir.path());
        std::fs::write(&path, "{ not json").expect("write");

        assert!(Entitlements::load(&path, &key).is_err());
    }

    /// The structural rule T3 rests on, checked rather than trusted.
    ///
    /// A local feature that merely *reads* entitlement state is one refactor away from depending on
    /// it, and the promise is that recording, transcription, summaries, search and every connector
    /// keep working forever regardless of billing. So no local crate may mention this module at all.
    #[test]
    fn no_local_crate_reads_entitlement_state() {
        let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates dir");

        // Every crate that runs entirely on the user's machine.
        let local = [
            "storage",
            "graph",
            "ai-router",
            "connectors",
            "recorder",
            "transcription",
            "diarization",
            "audio-capture",
            "macos-permissions",
            "eval",
        ];

        let mut offenders = Vec::new();
        for name in local {
            let src = crates_dir.join(name).join("src");
            if !src.is_dir() {
                continue;
            }
            visit(&src, &mut |path, text| {
                if text.contains("entitlement") || text.contains("Entitlements") {
                    offenders.push(path.display().to_string());
                }
            });
        }

        assert!(
            offenders.is_empty(),
            "a local crate must not read entitlement state: {offenders:?}"
        );
    }

    fn visit(dir: &Path, f: &mut impl FnMut(&Path, &str)) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, f);
            } else if path.extension().is_some_and(|e| e == "rs") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    f(&path, &text);
                }
            }
        }
    }
}
