//! Folding a second workspace into this one.
//!
//! # Why this is a feature and not a migration
//!
//! [`crate::location`] adopts a workspace left at an old path when there is nothing at the new
//! one — a rename, which loses nothing. When *both* hold data it refuses and says so, because
//! combining two populated stores is not a rename. That refusal is correct, and this module is the
//! thing it refuses in favour of: an explicit action, run when the user asks, that reports exactly
//! what it did.
//!
//! # Why it is far simpler than it sounds
//!
//! Every primary key in this schema is a v4 UUID ([`crate::Id`]), so rows from two independently
//! created stores cannot collide. There is no id to remap and no edge to rewrite for the vast
//! majority of the data — the work is inserting in foreign-key order and handling the handful of
//! `UNIQUE` constraints that are *not* ids.
//!
//! There is exactly one real remapping case. `people` is unique on email, so the same colleague
//! recorded in both stores is two rows that have to become one, and everything pointing at the
//! discarded row has to point at the kept one instead.
//!
//! # What it will not do
//!
//! It does not write to the source. It runs in one transaction, so a failure leaves this workspace
//! exactly as it was. It never overwrites: where both stores configured the same thing — a
//! setting, a connector account — this workspace wins and the incoming value is skipped, because
//! the alternative is a merge that silently changes settings the user is currently looking at.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::params;

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::migrations::SUPPORTED_VERSION;

/// What a merge moved.
///
/// Reported per table rather than as a total, because "19 rows" tells a user nothing and "3
/// meetings" tells them whether it worked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeReport {
    pub meetings: usize,
    pub transcript_segments: usize,
    pub summaries: usize,
    pub decisions: usize,
    pub action_items: usize,
    pub notes: usize,
    pub tickets: usize,
    pub people_added: usize,
    /// People that already existed here, matched by email. Their incoming rows were discarded and
    /// everything referring to them was repointed at the copy already here.
    pub people_merged: usize,
    pub workspaces: usize,
    pub projects: usize,
    pub series: usize,
    pub participants: usize,
    pub email_drafts: usize,
    pub edges: usize,
    pub external_items: usize,
    /// Rows skipped because this workspace already had an equivalent: a duplicate external item, a
    /// setting, a connector account.
    pub skipped_conflicts: usize,
}

impl MergeReport {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// One line worth showing a user.
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "nothing to merge — that workspace held no content".to_string();
        }
        format!(
            "merged {} meetings, {} transcript segments, {} notes, {} tickets, {} summaries; \
             {} people added and {} matched to people already here",
            self.meetings,
            self.transcript_segments,
            self.notes,
            self.tickets,
            self.summaries,
            self.people_added,
            self.people_merged
        )
    }
}

/// Whether to keep the result.
///
/// [`MergeMode::Preview`] runs the real merge and then rolls it back, so the counts it reports come
/// from the same code that would apply them. Counting rows separately would be a second
/// implementation of the merge rules, and the two would eventually disagree — which is the exact
/// situation a preview exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMode {
    Preview,
    Apply,
}

/// Fold the workspace at `source` into `db`.
///
/// The source is opened read-only and never written to. Everything happens in one transaction on
/// the destination, so an error — or [`MergeMode::Preview`] — leaves it untouched.
pub fn merge_from(db: &Database, source: &Path, mode: MergeMode) -> Result<MergeReport> {
    if !source.is_file() {
        return Err(StorageError::Merge(format!(
            "no workspace at {}",
            source.display()
        )));
    }

    // Same file: nothing to do, and attaching it twice would deadlock on the write lock.
    if let Some(current) = db.path() {
        if same_file(current, source) {
            return Err(StorageError::Merge(
                "that is this workspace; there is nothing to merge into itself".to_string(),
            ));
        }
    }

    let source_version = source_schema_version(source)?;
    if source_version != SUPPORTED_VERSION {
        return Err(StorageError::Merge(format!(
            "that workspace is at schema v{source_version} and this build expects \
             v{SUPPORTED_VERSION}. Open it once with `--db {}` to migrate it, then merge.",
            source.display()
        )));
    }

    let conn = db.conn();

    // `?` cannot parameterise an ATTACH path in rusqlite's statement cache, so it is bound
    // through a prepared statement rather than formatted into the SQL.
    conn.execute("ATTACH DATABASE ?1 AS incoming", params![path_str(source)?])?;

    let result = merge_attached(conn, mode);

    // Detach whatever happened, so a failed merge does not leave the connection holding the other
    // file open — which would make a retry fail for a different reason than the first attempt.
    let _ = conn.execute("DETACH DATABASE incoming", []);

    result
}

fn merge_attached(conn: &rusqlite::Connection, mode: MergeMode) -> Result<MergeReport> {
    let mut report = MergeReport::default();

    conn.execute("BEGIN IMMEDIATE", [])?;
    let outcome = (|| -> Result<()> {
        // People first, and by hand, because this is the only table where two rows can mean one
        // person. Everything downstream needs the mapping before it can be inserted.
        let remap = merge_people(conn, &mut report)?;

        // Then in foreign-key order. `INSERT OR IGNORE` throughout: a primary key that already
        // exists means this merge has been run before, and re-running it should be a no-op rather
        // than an error.
        report.workspaces += insert_all(conn, "workspaces")?;
        report.projects += insert_all(conn, "projects")?;
        report.series += insert_all(conn, "meeting_series")?;
        report.meetings += insert_all(conn, "meetings")?;

        report.transcript_segments +=
            insert_remapping_person(conn, "transcript_segments", "speaker_id", &remap)?;
        report.participants +=
            insert_remapping_person(conn, "meeting_participants", "person_id", &remap)?;

        report.summaries += insert_all(conn, "summaries")?;
        report.decisions += insert_all(conn, "decisions")?;
        report.action_items += insert_remapping_person(conn, "action_items", "person_id", &remap)?;

        report.notes += insert_all(conn, "notes")?;
        report.tickets += insert_all(conn, "tickets")?;
        report.email_drafts += insert_all(conn, "email_drafts")?;
        report.edges += insert_all(conn, "edges")?;
        report.external_items += insert_all(conn, "external_items")?;

        // Deliberately not merged:
        //
        // `app_settings` and `connector_accounts` — this workspace's configuration wins. A merge
        // that changed the active backend or repointed a vault would be changing settings the user
        // is looking at, to values from a store they just described as secondary.
        //
        // `embeddings` — derived data, and labelled with the model that produced it. Copying
        // vectors risks mixing models; the indexing pass rebuilds them for free.
        //
        // `connector_outbox` — pending deliveries for a workspace that is being retired. Replaying
        // them would push artifacts to a vault or webhook a second time.
        //
        // `notifications` — about events the user has already lived through.
        //
        // `search_index*` — FTS5 shadow tables, repopulated by the triggers the inserts above fire.
        report.skipped_conflicts += count_skipped(conn)?;

        Ok(())
    })();

    match outcome {
        Ok(()) => {
            match mode {
                // Same work, discarded. The counts are real because the inserts were real.
                MergeMode::Preview => conn.execute("ROLLBACK", [])?,
                MergeMode::Apply => conn.execute("COMMIT", [])?,
            };
            Ok(report)
        }
        Err(e) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(e)
        }
    }
}

/// Insert every row of `table` that is not already here, keeping column order in step by naming
/// the columns explicitly rather than relying on `SELECT *`.
fn insert_all(conn: &rusqlite::Connection, table: &str) -> Result<usize> {
    let columns = columns_of(conn, table)?;
    let list = columns.join(", ");
    let sql =
        format!("INSERT OR IGNORE INTO main.{table} ({list}) SELECT {list} FROM incoming.{table}");
    Ok(conn.execute(&sql, [])?)
}

/// Insert `table`, translating `person_column` through the people mapping.
///
/// `SELECT *` would be shorter and would break silently the first time a migration adds a column
/// in a different position, so the column list is read from the destination and used for both
/// sides.
fn insert_remapping_person(
    conn: &rusqlite::Connection,
    table: &str,
    person_column: &str,
    remap: &HashMap<String, String>,
) -> Result<usize> {
    let columns = columns_of(conn, table)?;
    let list = columns.join(", ");

    // `COALESCE` over a CASE chain would need the mapping inlined into SQL. A temp table keeps the
    // statement fixed and the data bound, which is also what makes an empty mapping free.
    let projected: Vec<String> = columns
        .iter()
        .map(|c| {
            if c == person_column {
                format!("COALESCE((SELECT canonical FROM temp.person_remap WHERE legacy = t.{c}), t.{c}) AS {c}")
            } else {
                format!("t.{c}")
            }
        })
        .collect();

    let sql = format!(
        "INSERT OR IGNORE INTO main.{table} ({list}) SELECT {} FROM incoming.{table} t",
        projected.join(", ")
    );
    let _ = remap;
    Ok(conn.execute(&sql, [])?)
}

/// Reconcile `people`, returning the legacy-id → kept-id mapping for those that already existed.
fn merge_people(
    conn: &rusqlite::Connection,
    report: &mut MergeReport,
) -> Result<HashMap<String, String>> {
    conn.execute(
        "CREATE TEMP TABLE IF NOT EXISTS person_remap (legacy TEXT PRIMARY KEY, canonical TEXT NOT NULL)",
        [],
    )?;
    conn.execute("DELETE FROM temp.person_remap", [])?;

    // Matched on email only, and only when both sides have one. Two people called "Sam" are not
    // evidence of anything, and merging them would attribute one person's words to another.
    conn.execute(
        "INSERT OR IGNORE INTO temp.person_remap (legacy, canonical)
         SELECT i.id, m.id
           FROM incoming.people i
           JOIN main.people m ON m.email = i.email
          WHERE i.email IS NOT NULL AND TRIM(i.email) <> '' AND i.id <> m.id",
        [],
    )?;

    let merged: usize =
        conn.query_row("SELECT COUNT(*) FROM temp.person_remap", [], |r| r.get(0))?;
    report.people_merged = merged;

    // Everyone else comes over as themselves. Voice-print columns are copied as they stand: they
    // are the user's existing data, and this is not the place that decides whether to hold them.
    let columns = columns_of(conn, "people")?;
    let list = columns.join(", ");
    let added = conn.execute(
        &format!(
            "INSERT OR IGNORE INTO main.people ({list})
             SELECT {list} FROM incoming.people
              WHERE id NOT IN (SELECT legacy FROM temp.person_remap)"
        ),
        [],
    )?;
    report.people_added = added;

    let mut stmt = conn.prepare("SELECT legacy, canonical FROM temp.person_remap")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut remap = HashMap::new();
    for row in rows {
        let (legacy, canonical) = row?;
        remap.insert(legacy, canonical);
    }
    Ok(remap)
}

/// Rows the incoming workspace had that this one already covers.
fn count_skipped(conn: &rusqlite::Connection) -> Result<usize> {
    let settings: usize = conn.query_row(
        "SELECT COUNT(*) FROM incoming.app_settings i
          WHERE EXISTS (SELECT 1 FROM main.app_settings m WHERE m.key = i.key)",
        [],
        |r| r.get(0),
    )?;
    let accounts: usize = conn.query_row(
        "SELECT COUNT(*) FROM incoming.connector_accounts i
          WHERE EXISTS (SELECT 1 FROM main.connector_accounts m
                         WHERE m.connector_id = i.connector_id)",
        [],
        |r| r.get(0),
    )?;
    Ok(settings + accounts)
}

fn columns_of(conn: &rusqlite::Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA main.table_info({table})"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    let mut names = Vec::new();
    for row in rows {
        names.push(row?);
    }
    if names.is_empty() {
        return Err(StorageError::Merge(format!("table {table} has no columns")));
    }
    Ok(names)
}

fn source_schema_version(source: &Path) -> Result<u32> {
    let conn = rusqlite::Connection::open_with_flags(
        source,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| StorageError::Merge(format!("path is not valid UTF-8: {}", path.display())))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::{MeetingRepository, NewMeeting, NewNote, NoteRepository};
    use crate::{Id, MeetingSource, NewPerson, PersonRepository};

    fn workspace(dir: &std::path::Path, name: &str) -> (Database, std::path::PathBuf) {
        let path = dir.join(format!("{name}.db"));
        let db = Database::open(&path).expect("open");
        (db, path)
    }

    fn add_meeting(db: &Database, title: &str) -> Id {
        MeetingRepository::new(db)
            .create(NewMeeting {
                project_id: None,
                title: title.into(),
                source: MeetingSource::Microphone,
                started_at: chrono::Utc::now(),
            })
            .expect("meeting")
            .id
    }

    #[test]
    fn a_meeting_from_the_other_workspace_arrives() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (canonical, _) = workspace(dir.path(), "canonical");
        let (legacy, legacy_path) = workspace(dir.path(), "legacy");

        add_meeting(&canonical, "here already");
        add_meeting(&legacy, "stranded");
        drop(legacy);

        let report = merge_from(&canonical, &legacy_path, MergeMode::Apply).expect("merge");
        assert_eq!(report.meetings, 1);

        let titles: Vec<String> = MeetingRepository::new(&canonical)
            .list_recent(10)
            .expect("list")
            .into_iter()
            .map(|m| m.title)
            .collect();
        assert!(titles.contains(&"stranded".to_string()), "{titles:?}");
        assert!(titles.contains(&"here already".to_string()), "{titles:?}");
    }

    /// A preview that changes anything is worse than no preview: it is the one command here whose
    /// whole promise is that it is safe to run before you have decided.
    #[test]
    fn a_preview_reports_the_real_counts_and_changes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (canonical, _) = workspace(dir.path(), "canonical");
        let (legacy, legacy_path) = workspace(dir.path(), "legacy");
        add_meeting(&legacy, "stranded");
        NoteRepository::new(&legacy)
            .create(NewNote {
                project_id: None,
                title: "stranded note".into(),
                body: "body".into(),
            })
            .expect("note");
        drop(legacy);

        let preview = merge_from(&canonical, &legacy_path, MergeMode::Preview).expect("preview");
        assert_eq!(preview.meetings, 1, "the counts must be real");
        assert_eq!(preview.notes, 1);

        assert_eq!(
            MeetingRepository::new(&canonical)
                .list_recent(10)
                .unwrap()
                .len(),
            0,
            "a preview must not write anything"
        );
        assert_eq!(
            NoteRepository::new(&canonical)
                .list_recent(10)
                .unwrap()
                .len(),
            0,
            "a preview must not write anything"
        );

        // And applying afterwards still works — the rollback must not have left the connection or
        // the temp mapping table in a state that breaks the real run.
        let applied = merge_from(&canonical, &legacy_path, MergeMode::Apply).expect("apply");
        assert_eq!(applied.meetings, 1);
        assert_eq!(
            MeetingRepository::new(&canonical)
                .list_recent(10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn merging_twice_changes_nothing_the_second_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (canonical, _) = workspace(dir.path(), "canonical");
        let (legacy, legacy_path) = workspace(dir.path(), "legacy");
        add_meeting(&legacy, "stranded");
        drop(legacy);

        let first = merge_from(&canonical, &legacy_path, MergeMode::Apply).expect("first");
        assert_eq!(first.meetings, 1);

        let second = merge_from(&canonical, &legacy_path, MergeMode::Apply).expect("second");
        assert_eq!(
            second.meetings, 0,
            "ids are UUIDs, so a re-run must be a no-op rather than a duplicate"
        );
        assert_eq!(
            MeetingRepository::new(&canonical)
                .list_recent(10)
                .unwrap()
                .len(),
            1
        );
    }

    /// The one real remapping case: the same colleague in both stores is one person afterwards.
    #[test]
    fn a_person_in_both_workspaces_is_merged_by_email() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (canonical, _) = workspace(dir.path(), "canonical");
        let (legacy, legacy_path) = workspace(dir.path(), "legacy");

        PersonRepository::new(&canonical)
            .create(NewPerson {
                display_name: "Priya".into(),
                email: Some("priya@example.com".into()),
            })
            .expect("canonical person");
        PersonRepository::new(&legacy)
            .create(NewPerson {
                display_name: "Priya R".into(),
                email: Some("priya@example.com".into()),
            })
            .expect("legacy person");
        drop(legacy);

        let report = merge_from(&canonical, &legacy_path, MergeMode::Apply).expect("merge");
        assert_eq!(report.people_merged, 1);
        assert_eq!(report.people_added, 0);
        assert_eq!(
            PersonRepository::new(&canonical)
                .list()
                .expect("list")
                .len(),
            1,
            "one email must mean one person"
        );
    }

    #[test]
    fn people_without_an_email_are_never_matched_together() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (canonical, _) = workspace(dir.path(), "canonical");
        let (legacy, legacy_path) = workspace(dir.path(), "legacy");

        for (db, name) in [(&canonical, "Speaker 1"), (&legacy, "Speaker 1")] {
            PersonRepository::new(db)
                .create(NewPerson {
                    display_name: name.into(),
                    email: None,
                })
                .expect("person");
        }
        drop(legacy);

        let report = merge_from(&canonical, &legacy_path, MergeMode::Apply).expect("merge");
        assert_eq!(
            report.people_added, 1,
            "two anonymous speakers are not evidence of one person"
        );
        assert_eq!(report.people_merged, 0);
    }

    #[test]
    fn notes_come_across_too() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (canonical, _) = workspace(dir.path(), "canonical");
        let (legacy, legacy_path) = workspace(dir.path(), "legacy");

        NoteRepository::new(&legacy)
            .create(NewNote {
                project_id: None,
                title: "stranded note".into(),
                body: "body".into(),
            })
            .expect("note");
        drop(legacy);

        let report = merge_from(&canonical, &legacy_path, MergeMode::Apply).expect("merge");
        assert_eq!(report.notes, 1);
        assert_eq!(
            NoteRepository::new(&canonical)
                .list_recent(10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn this_workspaces_settings_win() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (canonical, _) = workspace(dir.path(), "canonical");
        let (legacy, legacy_path) = workspace(dir.path(), "legacy");

        let keep = crate::SettingsRepository::new(&canonical);
        keep.set("ai_backend_model", "llama3.1:8b").expect("set");
        crate::SettingsRepository::new(&legacy)
            .set("ai_backend_model", "something-else")
            .expect("set");
        drop(legacy);

        let report = merge_from(&canonical, &legacy_path, MergeMode::Apply).expect("merge");
        assert!(report.skipped_conflicts >= 1);
        assert_eq!(
            keep.get("ai_backend_model").unwrap().as_deref(),
            Some("llama3.1:8b"),
            "a merge must not change a setting the user is looking at"
        );
    }

    #[test]
    fn merging_a_workspace_into_itself_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (canonical, path) = workspace(dir.path(), "canonical");
        let err = merge_from(&canonical, &path, MergeMode::Apply).expect_err("must refuse");
        assert!(matches!(err, StorageError::Merge(_)), "{err:?}");
    }

    #[test]
    fn a_missing_source_is_reported_not_panicked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (canonical, _) = workspace(dir.path(), "canonical");
        let err = merge_from(&canonical, &dir.path().join("nope.db"), MergeMode::Apply)
            .expect_err("must refuse");
        assert!(matches!(err, StorageError::Merge(_)), "{err:?}");
    }

    #[test]
    fn an_empty_workspace_merges_to_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (canonical, _) = workspace(dir.path(), "canonical");
        let (legacy, legacy_path) = workspace(dir.path(), "legacy");
        drop(legacy);

        let report = merge_from(&canonical, &legacy_path, MergeMode::Apply).expect("merge");
        assert!(report.is_empty(), "{report:?}");
        assert!(report.summary().contains("nothing to merge"));
    }
}
