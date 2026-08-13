//! Full-text search across notes, tickets, and transcript segments.

use rusqlite::Row;

use crate::db::Database;
use crate::error::Result;
use crate::id::Id;

/// One search result.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// Entity kind, matching `graph::NodeKind` naming: `note`, `ticket`, `transcript_segment`.
    pub entity_kind: String,
    pub entity_id: Id,
    pub title: String,
    /// Matching excerpt with the matched terms wrapped in `[` and `]`.
    pub snippet: String,
    /// FTS5 relevance. More negative is a better match.
    pub rank: f64,
}

#[derive(Debug)]
pub struct SearchRepository<'a> {
    db: &'a Database,
}

impl<'a> SearchRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Search everything indexed.
    ///
    /// The query is treated as a **literal phrase**, not FTS5 syntax. Passing raw user
    /// input to FTS5 lets stray punctuation produce a syntax error rather than zero
    /// results, which is a confusing failure for something typed into a search box.
    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<SearchHit>> {
        let Some(fts_query) = to_phrase_query(query) else {
            return Ok(Vec::new());
        };

        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT entity_kind, entity_id, title,
                    snippet(search_index, 3, '[', ']', '…', 24) AS snippet,
                    rank
             FROM search_index
             WHERE search_index MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![fts_query, limit], map_hit)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Search within a single entity kind.
    pub fn search_kind(&self, kind: &str, query: &str, limit: u32) -> Result<Vec<SearchHit>> {
        let Some(fts_query) = to_phrase_query(query) else {
            return Ok(Vec::new());
        };

        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT entity_kind, entity_id, title,
                    snippet(search_index, 3, '[', ']', '…', 24) AS snippet,
                    rank
             FROM search_index
             WHERE search_index MATCH ?1 AND entity_kind = ?2
             ORDER BY rank
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(rusqlite::params![fts_query, kind, limit], map_hit)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

/// Wrap user input as a quoted FTS5 phrase, escaping embedded quotes.
///
/// Returns `None` when the input has no searchable characters, so callers can
/// short-circuit instead of asking FTS5 to match nothing.
fn to_phrase_query(query: &str) -> Option<String> {
    let trimmed = query.trim();
    if trimmed.is_empty() || !trimmed.chars().any(|c| c.is_alphanumeric()) {
        return None;
    }
    // In FTS5 a double quote inside a phrase is escaped by doubling it.
    Some(format!("\"{}\"", trimmed.replace('"', "\"\"")))
}

fn map_hit(row: &Row<'_>) -> rusqlite::Result<SearchHit> {
    Ok(SearchHit {
        entity_kind: row.get(0)?,
        entity_id: row.get(1)?,
        title: row.get(2)?,
        snippet: row.get(3)?,
        rank: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::MeetingSource;
    use crate::repositories::{
        MeetingRepository, NewMeeting, NewNote, NewTicket, NewTranscriptSegment, NoteRepository,
        TicketRepository,
    };
    use chrono::{TimeZone, Utc};

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn seed(db: &Database) {
        NoteRepository::new(db)
            .create(NewNote {
                project_id: None,
                title: "Migration plan".into(),
                body: "We will migrate the database to Postgres next quarter.".into(),
            })
            .unwrap();

        TicketRepository::new(db)
            .create(NewTicket {
                project_id: None,
                title: "Postgres upgrade".into(),
                description: Some("Bump the cluster to 16.".into()),
                owner: None,
                due_at: None,
            })
            .unwrap();

        let meeting = MeetingRepository::new(db)
            .create(NewMeeting {
                project_id: None,
                title: "Infra sync".into(),
                source: MeetingSource::Microphone,
                started_at: Utc.timestamp_opt(1_700_000_000, 0).single().unwrap(),
            })
            .unwrap();
        MeetingRepository::new(db)
            .add_segment(NewTranscriptSegment {
                meeting_id: meeting.id,
                speaker: Some("Alex".into()),
                text: "Let's talk about the Postgres migration timeline.".into(),
                start_ms: 0,
                end_ms: 4000,
                confidence: None,
            })
            .unwrap();
    }

    #[test]
    fn finds_matches_across_every_indexed_kind() {
        let db = db();
        seed(&db);

        let hits = SearchRepository::new(&db).search("Postgres", 10).unwrap();
        let kinds: std::collections::HashSet<_> =
            hits.iter().map(|h| h.entity_kind.as_str()).collect();

        assert!(kinds.contains("note"), "got {kinds:?}");
        assert!(kinds.contains("ticket"), "got {kinds:?}");
        assert!(kinds.contains("transcript_segment"), "got {kinds:?}");
    }

    #[test]
    fn can_scope_search_to_one_kind() {
        let db = db();
        seed(&db);

        let hits = SearchRepository::new(&db)
            .search_kind("ticket", "Postgres", 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entity_kind, "ticket");
    }

    #[test]
    fn snippet_marks_the_matched_term() {
        let db = db();
        seed(&db);

        let hits = SearchRepository::new(&db)
            .search_kind("note", "Postgres", 10)
            .unwrap();
        assert!(
            hits[0].snippet.contains("[Postgres]"),
            "snippet should mark the match, got {:?}",
            hits[0].snippet
        );
    }

    #[test]
    fn updating_a_note_reindexes_it() {
        let db = db();
        let repo = NoteRepository::new(&db);
        let note = repo
            .create(NewNote {
                project_id: None,
                title: "Original".into(),
                body: "kumquat".into(),
            })
            .unwrap();

        let search = SearchRepository::new(&db);
        assert_eq!(search.search("kumquat", 10).unwrap().len(), 1);

        repo.update(note.id, "Original", "rhubarb").unwrap();
        assert_eq!(
            search.search("kumquat", 10).unwrap().len(),
            0,
            "stale text must not linger in the index"
        );
        assert_eq!(search.search("rhubarb", 10).unwrap().len(), 1);
    }

    #[test]
    fn deleting_a_note_removes_it_from_the_index() {
        let db = db();
        let repo = NoteRepository::new(&db);
        let note = repo
            .create(NewNote {
                project_id: None,
                title: "Doomed".into(),
                body: "ephemeral".into(),
            })
            .unwrap();

        repo.delete(note.id).unwrap();
        assert_eq!(
            SearchRepository::new(&db).search("ephemeral", 10).unwrap().len(),
            0
        );
    }

    #[test]
    fn punctuation_does_not_cause_a_syntax_error() {
        // Raw FTS5 would choke on several of these rather than returning no results.
        let db = db();
        seed(&db);
        let search = SearchRepository::new(&db);

        for query in ["\"unbalanced", "AND OR NOT", "foo*(bar)", "a:b", "--"] {
            assert!(
                search.search(query, 10).is_ok(),
                "query {query:?} should not error"
            );
        }
    }

    #[test]
    fn blank_and_symbol_only_queries_return_nothing() {
        let db = db();
        seed(&db);
        let search = SearchRepository::new(&db);

        for query in ["", "   ", "!!!", "***"] {
            assert!(search.search(query, 10).unwrap().is_empty(), "query {query:?}");
        }
    }

    #[test]
    fn search_is_case_insensitive() {
        let db = db();
        seed(&db);
        let search = SearchRepository::new(&db);
        assert_eq!(
            search.search("POSTGRES", 10).unwrap().len(),
            search.search("postgres", 10).unwrap().len()
        );
    }

    #[test]
    fn multi_word_input_matches_as_a_phrase() {
        let db = db();
        seed(&db);
        let search = SearchRepository::new(&db);

        assert_eq!(search.search("Postgres migration", 10).unwrap().len(), 1);
        assert_eq!(
            search.search("migration Postgres", 10).unwrap().len(),
            0,
            "reversed word order is not the same phrase"
        );
    }

    #[test]
    fn limit_is_respected() {
        let db = db();
        let repo = NoteRepository::new(&db);
        for i in 0..10 {
            repo.create(NewNote {
                project_id: None,
                title: format!("Note {i}"),
                body: "shared term".into(),
            })
            .unwrap();
        }

        assert_eq!(SearchRepository::new(&db).search("shared", 3).unwrap().len(), 3);
    }
}
