use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row};

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::id::Id;
use crate::models::{Meeting, MeetingSource, SpeakerSummary, TranscriptSegment};

use super::decode_enum;

#[derive(Debug, Clone)]
pub struct NewMeeting {
    pub project_id: Option<Id>,
    pub title: String,
    pub source: MeetingSource,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewTranscriptSegment {
    pub meeting_id: Id,
    pub speaker: Option<String>,
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub confidence: Option<f32>,
}

#[derive(Debug)]
pub struct MeetingRepository<'a> {
    db: &'a Database,
}

impl<'a> MeetingRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn create(&self, new: NewMeeting) -> Result<Meeting> {
        let now = Utc::now();
        let meeting = Meeting {
            id: Id::new(),
            project_id: new.project_id,
            title: new.title,
            source: new.source,
            started_at: new.started_at,
            ended_at: None,
            series_id: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        };

        self.db.conn().execute(
            "INSERT INTO meetings
                (id, project_id, title, source, started_at, ended_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7)",
            rusqlite::params![
                meeting.id,
                meeting.project_id,
                meeting.title,
                meeting.source.as_str(),
                meeting.started_at,
                meeting.created_at,
                meeting.updated_at
            ],
        )?;

        Ok(meeting)
    }

    pub fn get(&self, id: Id) -> Result<Meeting> {
        self.db
            .conn()
            .query_row(SELECT_MEETING, rusqlite::params![id], map_meeting)
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => StorageError::not_found("Meeting", id),
                other => other.into(),
            })
            .and_then(|r| r)
    }

    /// Most recent meetings first.
    pub fn list_recent(&self, limit: u32) -> Result<Vec<Meeting>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, source, started_at, ended_at, series_id, created_at, updated_at,
                    deleted_at
             FROM meetings WHERE deleted_at IS NULL ORDER BY started_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit], map_meeting)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    pub fn list_in_project(&self, project_id: Id) -> Result<Vec<Meeting>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, source, started_at, ended_at, series_id, created_at, updated_at,
                    deleted_at
             FROM meetings WHERE project_id = ?1 AND deleted_at IS NULL ORDER BY started_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![project_id], map_meeting)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    /// Meetings still recording — `ended_at` is null.
    pub fn list_active(&self) -> Result<Vec<Meeting>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, source, started_at, ended_at, series_id, created_at, updated_at,
                    deleted_at
             FROM meetings WHERE ended_at IS NULL AND deleted_at IS NULL ORDER BY started_at DESC",
        )?;
        let rows = stmt.query_map([], map_meeting)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    /// Mark a meeting as finished. Idempotent: ending an already-ended meeting keeps the
    /// original end time, so a duplicate stop event cannot rewrite history.
    pub fn end(&self, id: Id, ended_at: DateTime<Utc>) -> Result<Meeting> {
        let changed = self.db.conn().execute(
            "UPDATE meetings SET ended_at = ?2, updated_at = ?3
             WHERE id = ?1 AND ended_at IS NULL",
            rusqlite::params![id, ended_at, Utc::now()],
        )?;

        if changed == 0 {
            // Either the meeting is missing or it already ended; `get` distinguishes them.
            return self.get(id);
        }
        self.get(id)
    }

    pub fn set_project(&self, id: Id, project_id: Option<Id>) -> Result<Meeting> {
        let changed = self.db.conn().execute(
            "UPDATE meetings SET project_id = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, project_id, Utc::now()],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("Meeting", id));
        }
        self.get(id)
    }

    /// Move a meeting to the trash. Reversible with [`Self::restore`].
    ///
    /// The v10 trigger takes its transcript out of the search index as a side effect, so a
    /// trashed meeting stops answering questions — including through the agent, which reads
    /// search results.
    pub fn trash(&self, id: Id) -> Result<Meeting> {
        self.db.conn().execute(
            "UPDATE meetings SET deleted_at = ?2, updated_at = ?2
             WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![id, Utc::now()],
        )?;
        // No `changed` check: zero rows means missing or already trashed, and `get`
        // distinguishes them. Trashing twice keeps the first discard time.
        self.get(id)
    }

    pub fn restore(&self, id: Id) -> Result<Meeting> {
        self.db.conn().execute(
            "UPDATE meetings SET deleted_at = NULL, updated_at = ?2 WHERE id = ?1",
            rusqlite::params![id, Utc::now()],
        )?;
        self.get(id)
    }

    /// What is in the trash, most recently discarded first.
    pub fn list_trashed(&self) -> Result<Vec<Meeting>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, source, started_at, ended_at, series_id, created_at, updated_at,
                    deleted_at
             FROM meetings WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC",
        )?;
        let rows = stmt.query_map([], map_meeting)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    /// The ids in the trash, for a caller that must detach edges before destroying the rows.
    pub fn trashed_ids(&self) -> Result<Vec<Id>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare("SELECT id FROM meetings WHERE deleted_at IS NOT NULL")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Destroy a meeting and everything that cascades from it: transcript, summaries,
    /// decisions, action items, participants.
    ///
    /// Callers must detach its graph edges and drop its embeddings first — neither cascades,
    /// because neither table has a foreign key that could.
    /// Destroy a meeting and everything that cascades from it.
    ///
    /// Retained audio is unlinked first. It is a file rather than a row, so nothing cascades to it —
    /// deleting the row without deleting the file would leave a recording on disk that the user
    /// believes is gone and that no sweep will ever find, because the pointer to it went with the
    /// row. This is the irreversible half of the trash; the reversible half is [`Self::trash`],
    /// which deliberately keeps the audio so a restore is a whole restore.
    pub fn delete(&self, id: Id) -> Result<()> {
        // Read before the delete: after it, the path is unrecoverable.
        let audio: Option<String> = self
            .db
            .conn()
            .query_row(
                "SELECT audio_path FROM meetings WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();

        let changed = self
            .db
            .conn()
            .execute("DELETE FROM meetings WHERE id = ?1", rusqlite::params![id])?;
        if changed == 0 {
            return Err(StorageError::not_found("Meeting", id));
        }

        if let Some(path) = audio {
            // A file that is already gone is the outcome this wanted. Anything else is reported by
            // leaving it: the row is gone either way, and failing the delete now would be a meeting
            // that cannot be purged because of a file permission.
            let _ = std::fs::remove_file(path);
        }

        Ok(())
    }

    /// Meetings whose recorded span overlaps a window.
    ///
    /// Overlap rather than containment: a recording started late and running long still belongs to
    /// the event it began in, which is the whole point of matching by time.
    ///
    /// An unfinished meeting is treated as running until now, so a live recording can be matched to
    /// the event it is happening inside.
    pub fn overlapping(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Meeting>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, source, started_at, ended_at, series_id, created_at,
                    updated_at, deleted_at
             FROM meetings WHERE deleted_at IS NULL
                AND started_at < ?2
                AND COALESCE(ended_at, ?3) > ?1
              ORDER BY started_at",
        )?;
        let rows = stmt.query_map(rusqlite::params![from, to, Utc::now()], map_meeting)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    /// Append a transcript segment.
    pub fn add_segment(&self, new: NewTranscriptSegment) -> Result<TranscriptSegment> {
        let segment = TranscriptSegment {
            id: Id::new(),
            meeting_id: new.meeting_id,
            speaker: new.speaker,
            text: new.text,
            start_ms: new.start_ms,
            end_ms: new.end_ms,
            confidence: new.confidence,
        };

        self.db.conn().execute(
            "INSERT INTO transcript_segments
                (id, meeting_id, speaker, text, start_ms, end_ms, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                segment.id,
                segment.meeting_id,
                segment.speaker,
                segment.text,
                segment.start_ms,
                segment.end_ms,
                segment.confidence
            ],
        )?;

        Ok(segment)
    }

    /// Insert many segments in one transaction. Transcription emits segments in bursts;
    /// committing each one separately is dramatically slower.
    pub fn add_segments(&self, segments: Vec<NewTranscriptSegment>) -> Result<Vec<Id>> {
        let conn = self.db.conn();

        conn.execute_batch("BEGIN")?;
        let insert = || -> Result<Vec<Id>> {
            let mut ids = Vec::with_capacity(segments.len());
            let mut stmt = conn.prepare(
                "INSERT INTO transcript_segments
                    (id, meeting_id, speaker, text, start_ms, end_ms, confidence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for s in &segments {
                let id = Id::new();
                stmt.execute(rusqlite::params![
                    id,
                    s.meeting_id,
                    s.speaker,
                    s.text,
                    s.start_ms,
                    s.end_ms,
                    s.confidence
                ])?;
                ids.push(id);
            }
            Ok(ids)
        };

        match insert() {
            Ok(ids) => {
                conn.execute_batch("COMMIT")?;
                Ok(ids)
            }
            Err(e) => {
                conn.execute_batch("ROLLBACK")?;
                Err(e)
            }
        }
    }

    /// Transcript in chronological order.
    pub fn segments(&self, meeting_id: Id) -> Result<Vec<TranscriptSegment>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, meeting_id, speaker, text, start_ms, end_ms, confidence
             FROM transcript_segments WHERE meeting_id = ?1 ORDER BY start_ms",
        )?;
        let rows = stmt.query_map(rusqlite::params![meeting_id], map_segment)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Full transcript as plain text, one line per segment, speaker-prefixed when known.
    pub fn transcript_text(&self, meeting_id: Id) -> Result<String> {
        let segments = self.segments(meeting_id)?;
        let mut out = String::new();
        for s in segments {
            match &s.speaker {
                Some(speaker) => out.push_str(&format!("{speaker}: {}\n", s.text)),
                None => out.push_str(&format!("{}\n", s.text)),
            }
        }
        Ok(out)
    }

    /// Which meeting each of these segments belongs to.
    ///
    /// Exists for search. A hit on a transcript segment is only useful if it can be opened, and
    /// what a user wants to open is the meeting it was said in — the segment id on its own
    /// names a row nothing in the product can navigate to.
    ///
    /// Batched rather than looked up one at a time: a search returns up to a hundred hits, and
    /// a hundred round trips to answer one keystroke is a cost worth not paying.
    pub fn segment_meetings(&self, segment_ids: &[Id]) -> Result<Vec<(Id, Id)>> {
        if segment_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Placeholders are generated from the count and the ids are bound, so the ids
        // themselves never reach the SQL text.
        let placeholders = std::iter::repeat_n("?", segment_ids.len())
            .collect::<Vec<_>>()
            .join(", ");

        let conn = self.db.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT id, meeting_id FROM transcript_segments WHERE id IN ({placeholders})"
        ))?;

        let rows = stmt.query_map(rusqlite::params_from_iter(segment_ids), |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Attach a speaker label to a segment. Called by `diarization` after the fact.
    pub fn set_segment_speaker(&self, segment_id: Id, speaker: &str) -> Result<()> {
        let changed = self.db.conn().execute(
            "UPDATE transcript_segments SET speaker = ?2 WHERE id = ?1",
            rusqlite::params![segment_id, speaker],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("TranscriptSegment", segment_id));
        }
        Ok(())
    }

    /// Correct a mis-transcribed line.
    ///
    /// # Both indexes have to stay right, and only one does so on its own
    ///
    /// The v9 migration added `segments_au AFTER UPDATE ON transcript_segments`, so the full-text
    /// index re-indexes itself here. That was solved for the speaker column and generalises.
    ///
    /// The semantic index does not. `indexing` decides staleness by comparing an entity's
    /// `updated_at` against the newest chunk stored for it, and `transcript_segments` has no such
    /// column — so an edited segment would keep its old vector for ever, and search would keep
    /// finding the text the user just corrected.
    ///
    /// So the segment's embedding is deleted in the same transaction. A missing vector is already a
    /// state the indexing pass handles — it is what a never-indexed entity looks like — so the next
    /// pass rebuilds it and nothing new has to be understood.
    ///
    /// Adding `updated_at` to the table is the more general fix and was rejected: it would be
    /// written on every one of thousands of inserts per meeting to serve a rare update, and this
    /// path reuses machinery that already exists.
    pub fn set_segment_text(&self, segment_id: Id, text: &str) -> Result<()> {
        let conn = self.db.conn();
        let tx = conn.unchecked_transaction()?;

        let changed = tx.execute(
            "UPDATE transcript_segments SET text = ?2 WHERE id = ?1",
            rusqlite::params![segment_id, text],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("TranscriptSegment", segment_id));
        }

        tx.execute(
            "DELETE FROM embeddings WHERE entity_kind = 'transcript_segment' AND entity_id = ?1",
            rusqlite::params![segment_id],
        )?;

        tx.commit()?;
        Ok(())
    }

    /// Rename a meeting.
    ///
    /// `title` was set at `create` and never again, so a recording started from a hotkey kept
    /// whatever the caller guessed before the meeting happened.
    pub fn set_title(&self, id: Id, title: &str) -> Result<Meeting> {
        let changed = self.db.conn().execute(
            "UPDATE meetings SET title = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, title, Utc::now()],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("Meeting", id));
        }
        self.get(id)
    }

    /// The distinct voices in a meeting, in the order they were first heard.
    pub fn speakers(&self, meeting_id: Id) -> Result<Vec<SpeakerSummary>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            // The two-argument MAX is the scalar one — it floors durations at zero so a
            // segment with inverted timings cannot subtract from a speaker's total.
            "SELECT speaker, COUNT(*), SUM(MAX(end_ms - start_ms, 0)), MIN(start_ms)
             FROM transcript_segments WHERE meeting_id = ?1
             GROUP BY speaker ORDER BY MIN(start_ms)",
        )?;

        let rows = stmt.query_map(rusqlite::params![meeting_id], |row| {
            Ok(SpeakerSummary {
                label: row.get(0)?,
                segments: row.get(1)?,
                speaking_ms: row.get(2)?,
                first_at_ms: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Rename every segment attributed to one speaker, within a single meeting.
    ///
    /// # Renaming and merging are the same operation
    ///
    /// Renaming `Speaker 3` to a name nobody else has renames. Renaming it to `Dana` when
    /// `Dana` already exists *merges* the two — which is the fix for clustering having split
    /// one person in two, and the more common of the two actions in practice. Both are one
    /// `UPDATE`, so they cannot disagree with each other, and the caller does not have to know
    /// which one it is asking for.
    ///
    /// `from` is `None` to name the segments no diarizer ever labelled.
    ///
    /// # Why this is scoped to a meeting
    ///
    /// `Speaker 1` in Monday's standup and `Speaker 1` in Thursday's review are two anonymous
    /// clusters that happen to share a label, not one person. A workspace-wide rename would
    /// confidently attribute one person's words to another. Recognising a voice *across*
    /// meetings is what voiceprints are for, and that is opt-in for its own reasons.
    ///
    /// Returns how many segments changed. Zero means `from` labelled nothing here — reported
    /// rather than raised, so the boundary can decide whether a stale label is a 404 or a no-op.
    pub fn rename_speaker(&self, meeting_id: Id, from: Option<&str>, to: &str) -> Result<usize> {
        let to = to.trim();
        if to.is_empty() {
            return Err(StorageError::Invalid {
                what: "speaker",
                reason: "a speaker name cannot be blank".into(),
            });
        }
        if to.chars().count() > MAX_SPEAKER_NAME_CHARS {
            return Err(StorageError::Invalid {
                what: "speaker",
                reason: format!("a speaker name cannot exceed {MAX_SPEAKER_NAME_CHARS} characters"),
            });
        }

        // `IS` rather than `=` so that `from = None` matches SQL NULL. With `=` it would match
        // nothing and silently report zero changes.
        Ok(self.db.conn().execute(
            "UPDATE transcript_segments SET speaker = ?3
             WHERE meeting_id = ?1 AND speaker IS ?2",
            rusqlite::params![meeting_id, from, to],
        )?)
    }
}

/// Long enough for a full name with a role after it, short enough that a label cannot be used
/// to smuggle a paragraph into a transcript's speaker column.
pub const MAX_SPEAKER_NAME_CHARS: usize = 80;

const SELECT_MEETING: &str =
    "SELECT id, project_id, title, source, started_at, ended_at, series_id, created_at, updated_at,
                    deleted_at
     FROM meetings WHERE id = ?1";

fn map_meeting(row: &Row<'_>) -> rusqlite::Result<Result<Meeting>> {
    let source_raw: String = row.get(3)?;
    Ok((|| {
        Ok(Meeting {
            id: row.get(0)?,
            project_id: row.get(1)?,
            title: row.get(2)?,
            source: decode_enum("meetings.source", &source_raw, MeetingSource::parse)?,
            started_at: row.get(4)?,
            ended_at: row.get(5)?,
            series_id: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
            deleted_at: row.get(9)?,
        })
    })())
}

fn map_segment(row: &Row<'_>) -> rusqlite::Result<TranscriptSegment> {
    Ok(TranscriptSegment {
        id: row.get(0)?,
        meeting_id: row.get(1)?,
        speaker: row.get(2)?,
        text: row.get(3)?,
        start_ms: row.get(4)?,
        end_ms: row.get(5)?,
        confidence: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0)
            .single()
            .expect("valid timestamp")
    }

    fn meeting(db: &Database) -> Meeting {
        MeetingRepository::new(db)
            .create(NewMeeting {
                project_id: None,
                title: "Weekly sync".into(),
                source: MeetingSource::Combined,
                started_at: ts(1_700_000_000),
            })
            .expect("create meeting")
    }

    /// Trash is reversible, so a restore has to be a whole restore. Purge is the irreversible half
    /// and is the only thing that may destroy a recording.
    #[test]
    fn trash_keeps_the_audio_and_purge_destroys_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = db();
        let m = meeting(&db);
        let repo = MeetingRepository::new(&db);

        let path = dir.path().join("audio.wav");
        std::fs::write(&path, b"samples").expect("write");
        db.conn()
            .execute(
                "UPDATE meetings SET audio_path = ?2, audio_bytes = 7 WHERE id = ?1",
                rusqlite::params![m.id, path.to_str().unwrap()],
            )
            .expect("attach");

        repo.trash(m.id).expect("trash");
        assert!(
            path.exists(),
            "a restore after trashing must get the recording back too"
        );

        repo.restore(m.id).expect("restore");
        assert!(path.exists());

        repo.delete(m.id).expect("purge");
        assert!(
            !path.exists(),
            "purging is the irreversible half and must not leave a recording the user thinks is gone"
        );
    }

    /// Nothing cascades to a file, so a purge that could not find the row must still not strand one.
    #[test]
    fn purging_a_meeting_with_no_audio_is_unaffected() {
        let db = db();
        let m = meeting(&db);
        MeetingRepository::new(&db).delete(m.id).expect("purge");
    }

    #[test]
    fn a_meeting_can_be_renamed() {
        let db = db();
        let m = meeting(&db);
        let repo = MeetingRepository::new(&db);

        let renamed = repo.set_title(m.id, "Platform standup").expect("rename");
        assert_eq!(renamed.title, "Platform standup");
        assert_eq!(repo.get(m.id).expect("get").title, "Platform standup");
    }

    #[test]
    fn renaming_something_that_is_not_there_is_reported_not_panicked() {
        let db = db();
        let err = MeetingRepository::new(&db)
            .set_title(Id::new(), "x")
            .expect_err("must not succeed");
        assert!(matches!(err, StorageError::NotFound { .. }), "{err:?}");
    }

    /// Both indexes have to end up right, and only one manages that on its own.
    #[test]
    fn correcting_a_segment_updates_the_search_index_and_drops_its_vector() {
        let db = db();
        let m = meeting(&db);
        let repo = MeetingRepository::new(&db);

        let segment = repo
            .add_segment(NewTranscriptSegment {
                meeting_id: m.id,
                speaker: Some("Sam".into()),
                text: "we agreed to ship on Fryday".into(),
                start_ms: 0,
                end_ms: 1000,
                confidence: None,
            })
            .expect("segment");

        // A vector as the indexing pass would have left one.
        crate::EmbeddingRepository::new(&db)
            .replace_for_entity(
                "transcript_segment",
                segment.id,
                "nomic-embed-text:latest",
                vec![crate::NewEmbedding {
                    entity_kind: "transcript_segment".into(),
                    entity_id: segment.id,
                    chunk_index: 0,
                    text: "we agreed to ship on Fryday".into(),
                    vector: vec![0.1, 0.2],
                    model: "nomic-embed-text:latest".into(),
                    source_updated_at: ts(1_700_000_000),
                }],
            )
            .expect("embed");

        let search = crate::SearchRepository::new(&db);
        assert!(
            !search.search("Fryday", 10).expect("search").is_empty(),
            "the typo should be findable before it is corrected"
        );

        repo.set_segment_text(segment.id, "we agreed to ship on Friday")
            .expect("correct it");

        // FTS: the v9 `segments_au` trigger does this without being asked.
        assert!(
            search.search("Fryday", 10).expect("search").is_empty(),
            "the corrected text must not still be findable by the typo"
        );
        assert!(
            !search.search("Friday", 10).expect("search").is_empty(),
            "the correction must be findable"
        );

        // Semantic: nothing re-indexes on its own, so the stale vector has to be gone.
        assert_eq!(
            crate::EmbeddingRepository::new(&db)
                .count("nomic-embed-text:latest")
                .expect("count"),
            0,
            "a stale vector would keep answering with the text the user just corrected"
        );

        assert_eq!(
            repo.segments(m.id).expect("segments")[0].text,
            "we agreed to ship on Friday"
        );
    }

    #[test]
    fn correcting_a_segment_that_is_not_there_is_reported_not_panicked() {
        let db = db();
        let err = MeetingRepository::new(&db)
            .set_segment_text(Id::new(), "x")
            .expect_err("must not succeed");
        assert!(matches!(err, StorageError::NotFound { .. }), "{err:?}");
    }

    #[test]
    fn new_meeting_starts_in_recording_state() {
        let db = db();
        let m = meeting(&db);
        assert!(m.is_recording());
        assert_eq!(m.duration_ms(), None);
    }

    #[test]
    fn round_trips_meeting_including_source_enum() {
        let db = db();
        let repo = MeetingRepository::new(&db);
        let created = meeting(&db);
        let fetched = repo.get(created.id).unwrap();
        assert_eq!(fetched, created);
        assert_eq!(fetched.source, MeetingSource::Combined);
    }

    #[test]
    fn ending_a_meeting_sets_duration() {
        let db = db();
        let repo = MeetingRepository::new(&db);
        let m = meeting(&db);

        let ended = repo.end(m.id, ts(1_700_000_600)).unwrap();
        assert!(!ended.is_recording());
        assert_eq!(ended.duration_ms(), Some(600_000));
    }

    #[test]
    fn ending_twice_keeps_the_first_end_time() {
        let db = db();
        let repo = MeetingRepository::new(&db);
        let m = meeting(&db);

        let first = repo.end(m.id, ts(1_700_000_600)).unwrap();
        let second = repo.end(m.id, ts(1_700_009_999)).unwrap();
        assert_eq!(
            first.ended_at, second.ended_at,
            "a duplicate stop event must not rewrite the end time"
        );
    }

    #[test]
    fn list_active_only_returns_recording_meetings() {
        let db = db();
        let repo = MeetingRepository::new(&db);
        let ongoing = meeting(&db);
        let finished = meeting(&db);
        repo.end(finished.id, ts(1_700_000_600)).unwrap();

        let active = repo.list_active().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, ongoing.id);
    }

    #[test]
    fn segments_come_back_in_chronological_order() {
        let db = db();
        let repo = MeetingRepository::new(&db);
        let m = meeting(&db);

        for (start, text) in [(5000, "third"), (0, "first"), (2000, "second")] {
            repo.add_segment(NewTranscriptSegment {
                meeting_id: m.id,
                speaker: None,
                text: text.into(),
                start_ms: start,
                end_ms: start + 1000,
                confidence: None,
            })
            .unwrap();
        }

        let texts: Vec<_> = repo
            .segments(m.id)
            .unwrap()
            .into_iter()
            .map(|s| s.text)
            .collect();
        assert_eq!(texts, vec!["first", "second", "third"]);
    }

    #[test]
    fn batch_insert_returns_one_id_per_segment() {
        let db = db();
        let repo = MeetingRepository::new(&db);
        let m = meeting(&db);

        let batch: Vec<_> = (0..50)
            .map(|i| NewTranscriptSegment {
                meeting_id: m.id,
                speaker: None,
                text: format!("segment {i}"),
                start_ms: i * 1000,
                end_ms: i * 1000 + 900,
                confidence: Some(0.9),
            })
            .collect();

        let ids = repo.add_segments(batch).unwrap();
        assert_eq!(ids.len(), 50);
        assert_eq!(repo.segments(m.id).unwrap().len(), 50);
    }

    #[test]
    fn batch_insert_rolls_back_entirely_on_failure() {
        let db = db();
        let repo = MeetingRepository::new(&db);
        let m = meeting(&db);

        // Second segment references a meeting that does not exist, so the foreign key
        // fails mid-batch.
        let batch = vec![
            NewTranscriptSegment {
                meeting_id: m.id,
                speaker: None,
                text: "valid".into(),
                start_ms: 0,
                end_ms: 100,
                confidence: None,
            },
            NewTranscriptSegment {
                meeting_id: Id::new(),
                speaker: None,
                text: "orphan".into(),
                start_ms: 100,
                end_ms: 200,
                confidence: None,
            },
        ];

        assert!(repo.add_segments(batch).is_err());
        assert_eq!(
            repo.segments(m.id).unwrap().len(),
            0,
            "a failed batch must leave no segments behind"
        );
    }

    #[test]
    fn transcript_text_prefixes_known_speakers_only() {
        let db = db();
        let repo = MeetingRepository::new(&db);
        let m = meeting(&db);

        repo.add_segment(NewTranscriptSegment {
            meeting_id: m.id,
            speaker: Some("Alex".into()),
            text: "Shipping Friday.".into(),
            start_ms: 0,
            end_ms: 1000,
            confidence: None,
        })
        .unwrap();
        repo.add_segment(NewTranscriptSegment {
            meeting_id: m.id,
            speaker: None,
            text: "Unattributed line.".into(),
            start_ms: 1000,
            end_ms: 2000,
            confidence: None,
        })
        .unwrap();

        let text = repo.transcript_text(m.id).unwrap();
        assert_eq!(text, "Alex: Shipping Friday.\nUnattributed line.\n");
    }

    #[test]
    fn diarization_can_attach_speakers_after_the_fact() {
        let db = db();
        let repo = MeetingRepository::new(&db);
        let m = meeting(&db);

        let seg = repo
            .add_segment(NewTranscriptSegment {
                meeting_id: m.id,
                speaker: None,
                text: "Who said this?".into(),
                start_ms: 0,
                end_ms: 1000,
                confidence: None,
            })
            .unwrap();
        assert!(seg.speaker.is_none());

        repo.set_segment_speaker(seg.id, "Speaker 1").unwrap();
        assert_eq!(
            repo.segments(m.id).unwrap()[0].speaker.as_deref(),
            Some("Speaker 1")
        );
    }

    #[test]
    fn deleting_a_meeting_cascades_to_segments() {
        let db = db();
        let repo = MeetingRepository::new(&db);
        let m = meeting(&db);
        repo.add_segment(NewTranscriptSegment {
            meeting_id: m.id,
            speaker: None,
            text: "gone soon".into(),
            start_ms: 0,
            end_ms: 100,
            confidence: None,
        })
        .unwrap();

        repo.delete(m.id).unwrap();
        assert_eq!(repo.segments(m.id).unwrap().len(), 0);
    }

    #[test]
    fn corrupt_source_enum_is_reported_not_panicked() {
        let db = db();
        let m = meeting(&db);
        db.conn()
            .execute(
                "UPDATE meetings SET source = 'telepathy' WHERE id = ?1",
                rusqlite::params![m.id],
            )
            .unwrap();

        let err = MeetingRepository::new(&db)
            .get(m.id)
            .expect_err("unrecognized enum should surface as an error");
        assert!(matches!(err, StorageError::Corrupt { .. }), "got {err:?}");
    }

    /// Add a segment, optionally attributed.
    fn say(db: &Database, meeting_id: Id, speaker: Option<&str>, text: &str, start_ms: i64) {
        MeetingRepository::new(db)
            .add_segment(NewTranscriptSegment {
                meeting_id,
                speaker: speaker.map(str::to_string),
                text: text.into(),
                start_ms,
                end_ms: start_ms + 1_000,
                confidence: None,
            })
            .expect("add segment");
    }

    #[test]
    fn speakers_are_listed_in_the_order_they_were_first_heard() {
        let db = db();
        let m = meeting(&db);
        // Deliberately not in label order: Speaker 2 opens the meeting.
        say(&db, m.id, Some("Speaker 2"), "morning", 0);
        say(&db, m.id, Some("Speaker 1"), "morning", 2_000);
        say(&db, m.id, Some("Speaker 2"), "shall we start", 4_000);

        let speakers = MeetingRepository::new(&db).speakers(m.id).unwrap();
        let labels: Vec<_> = speakers.iter().map(|s| s.label.as_deref()).collect();

        assert_eq!(labels, vec![Some("Speaker 2"), Some("Speaker 1")]);
        assert_eq!(speakers[0].segments, 2);
        assert_eq!(speakers[0].speaking_ms, 2_000, "two one-second segments");
        assert_eq!(speakers[0].first_at_ms, 0);
    }

    /// Speaking time sums the segments rather than spanning first to last — someone who said
    /// two words an hour apart spoke for two seconds, not an hour.
    #[test]
    fn speaking_time_sums_segments_rather_than_spanning_them() {
        let db = db();
        let m = meeting(&db);
        say(&db, m.id, Some("Dana"), "hello", 0);
        say(&db, m.id, Some("Dana"), "bye", 3_600_000);

        let speakers = MeetingRepository::new(&db).speakers(m.id).unwrap();
        assert_eq!(speakers[0].speaking_ms, 2_000);
    }

    #[test]
    fn unattributed_segments_are_a_nameable_group() {
        let db = db();
        let m = meeting(&db);
        say(&db, m.id, None, "who said this", 0);

        let repo = MeetingRepository::new(&db);
        assert_eq!(repo.speakers(m.id).unwrap()[0].label, None);

        assert_eq!(repo.rename_speaker(m.id, None, "Priya").unwrap(), 1);
        assert_eq!(
            repo.speakers(m.id).unwrap()[0].label.as_deref(),
            Some("Priya")
        );
    }

    #[test]
    fn renaming_a_speaker_relabels_every_one_of_their_segments() {
        let db = db();
        let m = meeting(&db);
        say(&db, m.id, Some("Speaker 1"), "first", 0);
        say(&db, m.id, Some("Speaker 1"), "second", 2_000);
        say(&db, m.id, Some("Speaker 2"), "other", 4_000);

        let repo = MeetingRepository::new(&db);
        assert_eq!(
            repo.rename_speaker(m.id, Some("Speaker 1"), "Dana")
                .unwrap(),
            2
        );

        let by_speaker: Vec<_> = repo
            .segments(m.id)
            .unwrap()
            .into_iter()
            .map(|s| s.speaker.unwrap())
            .collect();
        assert_eq!(by_speaker, vec!["Dana", "Dana", "Speaker 2"]);
    }

    /// The fix for clustering having split one person in two, and the reason rename and merge
    /// are deliberately one operation.
    #[test]
    fn renaming_onto_an_existing_name_merges_the_two_speakers() {
        let db = db();
        let m = meeting(&db);
        say(&db, m.id, Some("Speaker 1"), "a", 0);
        say(&db, m.id, Some("Speaker 3"), "b", 2_000);
        say(&db, m.id, Some("Speaker 1"), "c", 4_000);

        let repo = MeetingRepository::new(&db);
        repo.rename_speaker(m.id, Some("Speaker 1"), "Dana")
            .unwrap();
        // Speaker 3 was the same person all along.
        repo.rename_speaker(m.id, Some("Speaker 3"), "Dana")
            .unwrap();

        let speakers = repo.speakers(m.id).unwrap();
        assert_eq!(speakers.len(), 1, "the two clusters should have merged");
        assert_eq!(speakers[0].label.as_deref(), Some("Dana"));
        assert_eq!(speakers[0].segments, 3);
    }

    /// `Speaker 1` in two meetings is two anonymous clusters, not one person.
    #[test]
    fn renaming_is_scoped_to_one_meeting() {
        let db = db();
        let monday = meeting(&db);
        let thursday = meeting(&db);
        say(&db, monday.id, Some("Speaker 1"), "standup", 0);
        say(&db, thursday.id, Some("Speaker 1"), "review", 0);

        let repo = MeetingRepository::new(&db);
        repo.rename_speaker(monday.id, Some("Speaker 1"), "Dana")
            .unwrap();

        assert_eq!(
            repo.segments(thursday.id).unwrap()[0].speaker.as_deref(),
            Some("Speaker 1"),
            "another meeting's identically-labelled cluster must be untouched"
        );
    }

    #[test]
    fn renaming_a_label_nobody_has_changes_nothing_and_says_so() {
        let db = db();
        let m = meeting(&db);
        say(&db, m.id, Some("Speaker 1"), "a", 0);

        let changed = MeetingRepository::new(&db)
            .rename_speaker(m.id, Some("Speaker 9"), "Dana")
            .unwrap();
        assert_eq!(changed, 0);
    }

    #[test]
    fn a_blank_speaker_name_is_rejected() {
        let db = db();
        let m = meeting(&db);
        say(&db, m.id, Some("Speaker 1"), "a", 0);

        let err = MeetingRepository::new(&db)
            .rename_speaker(m.id, Some("Speaker 1"), "   ")
            .expect_err("blank should be rejected");
        assert!(matches!(err, StorageError::Invalid { .. }), "got {err:?}");
    }

    #[test]
    fn an_overlong_speaker_name_is_rejected() {
        let db = db();
        let m = meeting(&db);
        say(&db, m.id, Some("Speaker 1"), "a", 0);

        let err = MeetingRepository::new(&db)
            .rename_speaker(
                m.id,
                Some("Speaker 1"),
                &"n".repeat(MAX_SPEAKER_NAME_CHARS + 1),
            )
            .expect_err("overlong should be rejected");
        assert!(matches!(err, StorageError::Invalid { .. }), "got {err:?}");
    }

    #[test]
    fn a_speaker_name_is_trimmed() {
        let db = db();
        let m = meeting(&db);
        say(&db, m.id, Some("Speaker 1"), "a", 0);

        let repo = MeetingRepository::new(&db);
        repo.rename_speaker(m.id, Some("Speaker 1"), "  Dana  ")
            .unwrap();
        assert_eq!(
            repo.speakers(m.id).unwrap()[0].label.as_deref(),
            Some("Dana")
        );
    }

    /// Migration v9 put the speaker in the search index. A rename that did not reach the index
    /// would leave the old name findable and the new one invisible — the sort of drift nobody
    /// notices until a search for a colleague returns nothing.
    #[test]
    fn renaming_a_speaker_updates_the_search_index() {
        let db = db();
        let m = meeting(&db);
        say(&db, m.id, Some("Speaker 1"), "the quarterly numbers", 0);

        let hits = |term: &str| -> i64 {
            db.conn()
                .query_row(
                    "SELECT COUNT(*) FROM search_index WHERE search_index MATCH ?1",
                    rusqlite::params![term],
                    |row| row.get(0),
                )
                .unwrap()
        };

        assert_eq!(hits("\"Speaker 1\""), 1);

        MeetingRepository::new(&db)
            .rename_speaker(m.id, Some("Speaker 1"), "Dana")
            .unwrap();

        assert_eq!(hits("Dana"), 1, "the new name should be findable");
        assert_eq!(hits("\"Speaker 1\""), 0, "the old name should not be");
    }
}
