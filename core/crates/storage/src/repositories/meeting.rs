use chrono::{DateTime, Utc};
use rusqlite::Row;

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::id::Id;
use crate::models::{Meeting, MeetingSource, TranscriptSegment};

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
            created_at: now,
            updated_at: now,
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
            "SELECT id, project_id, title, source, started_at, ended_at, created_at, updated_at
             FROM meetings ORDER BY started_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit], map_meeting)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    pub fn list_in_project(&self, project_id: Id) -> Result<Vec<Meeting>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, source, started_at, ended_at, created_at, updated_at
             FROM meetings WHERE project_id = ?1 ORDER BY started_at DESC",
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
            "SELECT id, project_id, title, source, started_at, ended_at, created_at, updated_at
             FROM meetings WHERE ended_at IS NULL ORDER BY started_at DESC",
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

    pub fn delete(&self, id: Id) -> Result<()> {
        let changed = self
            .db
            .conn()
            .execute("DELETE FROM meetings WHERE id = ?1", rusqlite::params![id])?;
        if changed == 0 {
            return Err(StorageError::not_found("Meeting", id));
        }
        Ok(())
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
}

const SELECT_MEETING: &str =
    "SELECT id, project_id, title, source, started_at, ended_at, created_at, updated_at
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
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
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
}
