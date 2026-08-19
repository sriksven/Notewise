//! Keeping the audio a meeting was transcribed from.
//!
//! # Why this is off by default, and why that is the whole design
//!
//! Retaining audio means holding a recording of other people's voices on disk. Every other
//! privacy-shaped capability in this engine ships off — voiceprints, acoustic separation, the
//! `voice_print` columns that exist and are deliberately never written — and this is the most
//! sensitive of them. A user who updates must not discover later that the app started keeping
//! recordings.
//!
//! What it buys is two things that are otherwise impossible: hearing the moment a line was said,
//! and re-transcribing with a better model when the first pass invented something. The second is
//! the stronger reason — the `eval` crate exists because a real recording came back with four
//! invented segments, and without the audio there is no recourse.
//!
//! # Encryption: what this does not claim
//!
//! An earlier draft of the design said audio would use "the database's existing at-rest mechanism".
//! That mechanism is SQLCipher, which encrypts *the database file* — there is no primitive here for
//! encrypting a separate file, and no key to borrow.
//!
//! So retained audio is written in the clear, and the protections are the ones that do not need
//! cryptography: it is off unless chosen, bounded by a retention window, and really deleted. On a
//! default install that is consistent with everything else — the database holding the full
//! transcript is unencrypted too.
//!
//! What is *not* acceptable is silently weakening a choice the user made. If the database was opened
//! with SQLCipher, [`RetentionPolicy`] refuses to be enabled: plaintext audio beside an encrypted
//! transcript would defeat the encryption someone deliberately turned on. Encrypting the audio too
//! is the proper fix and is not implemented.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::id::Id;
use crate::repositories::SettingsRepository;

/// Setting key holding the retention choice.
pub const RETENTION_KEY: &str = "audio_retention";

/// How long retained audio is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionPolicy {
    /// Nothing is kept. The transcript path is exactly as it was before this module existed.
    Off,
    /// Kept until the meeting is purged from the trash.
    UntilDeleted,
    /// Kept for this many days after the meeting ended.
    Days(u32),
}

/// The default when nothing is stored.
///
/// Off, so the setting is a decision rather than an inheritance.
impl Default for RetentionPolicy {
    fn default() -> Self {
        Self::Off
    }
}

/// The window used when a user asks for days without saying how many.
///
/// Thirty, because the value of audio decays fast — seek and re-transcription are both used soon
/// after a meeting — and an unbounded default would quietly consume tens of gigabytes a year.
pub const DEFAULT_RETENTION_DAYS: u32 = 30;

impl RetentionPolicy {
    pub fn as_str(&self) -> String {
        match self {
            RetentionPolicy::Off => "off".to_string(),
            RetentionPolicy::UntilDeleted => "until_deleted".to_string(),
            RetentionPolicy::Days(n) => format!("days:{n}"),
        }
    }

    /// Parse a stored value.
    ///
    /// Anything unrecognised is [`RetentionPolicy::Off`]. A corrupt setting must not be read as
    /// permission to keep recordings — the safe direction is unambiguous here.
    pub fn parse(raw: &str) -> Self {
        let raw = raw.trim();
        if raw == "until_deleted" {
            return RetentionPolicy::UntilDeleted;
        }
        if let Some(days) = raw.strip_prefix("days:") {
            if let Ok(n) = days.trim().parse::<u32>() {
                if n > 0 {
                    return RetentionPolicy::Days(n);
                }
            }
        }
        RetentionPolicy::Off
    }

    pub fn keeps_anything(&self) -> bool {
        !matches!(self, RetentionPolicy::Off)
    }
}

/// The configured policy, or off.
pub fn retention_policy(db: &Database) -> RetentionPolicy {
    SettingsRepository::new(db)
        .get(RETENTION_KEY)
        .ok()
        .flatten()
        .map(|raw| RetentionPolicy::parse(&raw))
        .unwrap_or_default()
}

/// Change the policy.
///
/// Refuses to enable retention on an encrypted database. Writing plaintext audio beside an
/// encrypted transcript would silently undo the protection the user chose, and choosing for them is
/// not this function's call — so it says no and explains why.
pub fn set_retention_policy(db: &Database, policy: RetentionPolicy) -> Result<()> {
    if policy.keeps_anything() && db.is_encrypted() {
        return Err(StorageError::Refused(
            "this workspace is encrypted, and retained audio would be written unencrypted beside \
             it — which would defeat the encryption. Audio encryption is not implemented."
                .into(),
        ));
    }
    SettingsRepository::new(db).set(RETENTION_KEY, &policy.as_str())
}

/// Where a meeting's audio belongs, given the directory audio is kept in.
///
/// One file per meeting, named by id: no title to sanitise, no collision to resolve, and a stray
/// file is traceable back to a row.
pub fn audio_path_for(dir: &Path, meeting_id: Id) -> PathBuf {
    dir.join(format!("{meeting_id}.wav"))
}

/// A meeting whose audio has outlived the policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredAudio {
    pub meeting_id: Id,
    pub path: String,
    pub bytes: i64,
}

/// Meetings whose audio the policy no longer covers.
///
/// Pure with respect to time — `now` is passed in — so every boundary is testable without waiting.
///
/// [`RetentionPolicy::UntilDeleted`] returns nothing: those files go when the meeting is purged,
/// which `MeetingRepository::delete` handles. [`RetentionPolicy::Off`] returns *everything*, because
/// turning retention off has to clear what was already kept — leaving it would mean a user who
/// switched the feature off still had the recordings.
pub fn expired(
    db: &Database,
    policy: RetentionPolicy,
    now: DateTime<Utc>,
) -> Result<Vec<ExpiredAudio>> {
    let conn = db.conn();

    let sql = match policy {
        RetentionPolicy::UntilDeleted => return Ok(Vec::new()),
        RetentionPolicy::Off => "SELECT id, audio_path, COALESCE(audio_bytes, 0)
               FROM meetings WHERE audio_path IS NOT NULL"
            .to_string(),
        RetentionPolicy::Days(_) => {
            // Measured from when the meeting ended, not when it was created: a long meeting should
            // not start expiring while it is still being recorded.
            "SELECT id, audio_path, COALESCE(audio_bytes, 0)
               FROM meetings
              WHERE audio_path IS NOT NULL
                AND ended_at IS NOT NULL
                AND ended_at < ?1"
                .to_string()
        }
    };

    let mut stmt = conn.prepare(&sql)?;
    let rows = match policy {
        RetentionPolicy::Days(days) => {
            let cutoff = now - Duration::days(i64::from(days));
            stmt.query_map(rusqlite::params![cutoff], map_expired)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        }
        _ => stmt
            .query_map([], map_expired)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    };

    Ok(rows)
}

fn map_expired(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExpiredAudio> {
    Ok(ExpiredAudio {
        meeting_id: row.get(0)?,
        path: row.get(1)?,
        bytes: row.get(2)?,
    })
}

/// What a sweep removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub deleted: usize,
    pub bytes_freed: i64,
    /// Files the policy covered that could not be removed. Their pointers are left in place, so a
    /// later sweep tries again.
    ///
    /// Returned rather than logged: `storage` has no logging dependency by design, and the caller
    /// is the one with somewhere to report it.
    pub failed: Vec<String>,
}

/// Delete audio the policy no longer covers.
///
/// # The order matters
///
/// The file is unlinked *before* the pointer is cleared. A crash between the two leaves a pointer to
/// a missing file, which reads as "no audio" and is corrected by the next sweep. The other order
/// would leave a file with no pointer — an orphan nothing will ever clean up, holding a recording
/// the user believes is gone.
pub fn sweep(db: &Database, policy: RetentionPolicy, now: DateTime<Utc>) -> Result<SweepReport> {
    let mut report = SweepReport::default();

    for item in expired(db, policy, now)? {
        let path = Path::new(&item.path);
        match std::fs::remove_file(path) {
            // Already gone is success: the goal is that no audio remains, not that this call was
            // the one to remove it.
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                report.failed.push(item.path.clone());
                continue;
            }
        }

        db.conn().execute(
            "UPDATE meetings SET audio_path = NULL, audio_bytes = NULL WHERE id = ?1",
            rusqlite::params![item.meeting_id],
        )?;
        report.deleted += 1;
        report.bytes_freed += item.bytes;
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::{MeetingRepository, NewMeeting};
    use crate::MeetingSource;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn ended_meeting(db: &Database, ago_days: i64) -> Id {
        let repo = MeetingRepository::new(db);
        let started = Utc::now() - Duration::days(ago_days);
        let m = repo
            .create(NewMeeting {
                project_id: None,
                title: "Standup".into(),
                source: MeetingSource::Microphone,
                started_at: started,
            })
            .expect("meeting");
        repo.end(m.id, started + Duration::minutes(30))
            .expect("end");
        m.id
    }

    fn attach(db: &Database, id: Id, path: &Path) {
        std::fs::write(path, b"not really audio").expect("write");
        db.conn()
            .execute(
                "UPDATE meetings SET audio_path = ?2, audio_bytes = ?3 WHERE id = ?1",
                rusqlite::params![id, path.to_str().unwrap(), 16i64],
            )
            .expect("attach");
    }

    #[test]
    fn a_policy_round_trips_and_anything_unrecognised_is_off() {
        for policy in [
            RetentionPolicy::Off,
            RetentionPolicy::UntilDeleted,
            RetentionPolicy::Days(30),
        ] {
            assert_eq!(RetentionPolicy::parse(&policy.as_str()), policy);
        }

        // A corrupt setting must never be read as permission to keep recordings.
        for raw in ["", "  ", "yes", "days:", "days:0", "days:-1", "forever"] {
            assert_eq!(RetentionPolicy::parse(raw), RetentionPolicy::Off, "{raw:?}");
        }
    }

    #[test]
    fn retention_is_off_until_someone_turns_it_on() {
        let db = db();
        assert_eq!(retention_policy(&db), RetentionPolicy::Off);
        assert!(!RetentionPolicy::Off.keeps_anything());

        set_retention_policy(&db, RetentionPolicy::Days(7)).expect("set");
        assert_eq!(retention_policy(&db), RetentionPolicy::Days(7));
    }

    #[test]
    fn a_days_policy_expires_only_what_is_older_than_the_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = db();

        let old = ended_meeting(&db, 40);
        let recent = ended_meeting(&db, 3);
        attach(&db, old, &dir.path().join("old.wav"));
        attach(&db, recent, &dir.path().join("recent.wav"));

        let due = expired(&db, RetentionPolicy::Days(30), Utc::now()).expect("expired");
        assert_eq!(due.len(), 1, "{due:?}");
        assert_eq!(due[0].meeting_id, old);
    }

    #[test]
    fn until_deleted_never_expires_on_a_timer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = db();
        let old = ended_meeting(&db, 400);
        attach(&db, old, &dir.path().join("old.wav"));

        assert!(expired(&db, RetentionPolicy::UntilDeleted, Utc::now())
            .expect("expired")
            .is_empty());
    }

    /// Turning the feature off has to clear what was already kept, or a user who switched it off
    /// still has the recordings.
    #[test]
    fn switching_retention_off_expires_everything() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = db();
        let recent = ended_meeting(&db, 1);
        attach(&db, recent, &dir.path().join("recent.wav"));

        let due = expired(&db, RetentionPolicy::Off, Utc::now()).expect("expired");
        assert_eq!(due.len(), 1);
    }

    #[test]
    fn a_sweep_deletes_the_file_and_clears_the_pointer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = db();
        let old = ended_meeting(&db, 40);
        let path = dir.path().join("old.wav");
        attach(&db, old, &path);

        let report = sweep(&db, RetentionPolicy::Days(30), Utc::now()).expect("sweep");
        assert_eq!(report.deleted, 1);
        assert_eq!(report.bytes_freed, 16);
        assert!(!path.exists(), "the file must be gone");

        let remaining: Option<String> = db
            .conn()
            .query_row(
                "SELECT audio_path FROM meetings WHERE id = ?1",
                rusqlite::params![old],
                |r| r.get(0),
            )
            .expect("query");
        assert_eq!(remaining, None, "the pointer must be cleared");
    }

    /// A pointer to a file someone already removed reads as "no audio", and the sweep tidies it.
    #[test]
    fn a_missing_file_is_not_a_failure() {
        let db = db();
        let old = ended_meeting(&db, 40);
        db.conn()
            .execute(
                "UPDATE meetings SET audio_path = '/nowhere/gone.wav', audio_bytes = 5 WHERE id = ?1",
                rusqlite::params![old],
            )
            .expect("attach");

        let report = sweep(&db, RetentionPolicy::Days(30), Utc::now()).expect("sweep");
        assert_eq!(report.deleted, 1);
        assert!(report.failed.is_empty());
    }

    #[test]
    fn a_sweep_with_nothing_to_do_reports_nothing() {
        let db = db();
        let report = sweep(&db, RetentionPolicy::Days(30), Utc::now()).expect("sweep");
        assert_eq!(report, SweepReport::default());
    }

    #[test]
    fn one_file_per_meeting_named_by_id() {
        let id = Id::new();
        let path = audio_path_for(Path::new("/data/audio"), id);
        assert_eq!(path, PathBuf::from(format!("/data/audio/{id}.wav")));
    }
}
