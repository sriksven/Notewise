//! Schema migrations.
//!
//! Migrations are append-only: never edit a shipped migration, add a new one. The applied
//! version is tracked in SQLite's built-in `user_version` pragma, which avoids a bootstrap
//! problem — no table needs to exist before we can read it.

use rusqlite::Connection;

use crate::error::{Result, StorageError};

/// Every migration, in order. Index + 1 is the schema version it produces.
const MIGRATIONS: &[&str] = &[
    // v1 — foundation: workspace, projects, meetings, transcripts, summaries, and the
    // generic edge table backing the `graph` crate.
    r#"
    CREATE TABLE workspaces (
        id          TEXT PRIMARY KEY NOT NULL,
        name        TEXT NOT NULL,
        created_at  TEXT NOT NULL,
        updated_at  TEXT NOT NULL
    );

    CREATE TABLE projects (
        id            TEXT PRIMARY KEY NOT NULL,
        workspace_id  TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
        name          TEXT NOT NULL,
        description   TEXT,
        created_at    TEXT NOT NULL,
        updated_at    TEXT NOT NULL
    );
    CREATE INDEX idx_projects_workspace ON projects(workspace_id);

    CREATE TABLE meetings (
        id          TEXT PRIMARY KEY NOT NULL,
        project_id  TEXT REFERENCES projects(id) ON DELETE SET NULL,
        title       TEXT NOT NULL,
        source      TEXT NOT NULL,
        started_at  TEXT NOT NULL,
        ended_at    TEXT,
        created_at  TEXT NOT NULL,
        updated_at  TEXT NOT NULL
    );
    CREATE INDEX idx_meetings_project ON meetings(project_id);
    CREATE INDEX idx_meetings_started ON meetings(started_at DESC);

    CREATE TABLE transcript_segments (
        id          TEXT PRIMARY KEY NOT NULL,
        meeting_id  TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
        speaker     TEXT,
        text        TEXT NOT NULL,
        start_ms    INTEGER NOT NULL,
        end_ms      INTEGER NOT NULL,
        confidence  REAL
    );
    CREATE INDEX idx_segments_meeting ON transcript_segments(meeting_id, start_ms);

    CREATE TABLE summaries (
        id          TEXT PRIMARY KEY NOT NULL,
        meeting_id  TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
        text        TEXT NOT NULL,
        model       TEXT NOT NULL,
        created_at  TEXT NOT NULL
    );
    CREATE INDEX idx_summaries_meeting ON summaries(meeting_id);

    CREATE TABLE decisions (
        id          TEXT PRIMARY KEY NOT NULL,
        summary_id  TEXT NOT NULL REFERENCES summaries(id) ON DELETE CASCADE,
        text        TEXT NOT NULL,
        reasoning   TEXT,
        decided_at  TEXT
    );
    CREATE INDEX idx_decisions_summary ON decisions(summary_id);

    CREATE TABLE action_items (
        id          TEXT PRIMARY KEY NOT NULL,
        summary_id  TEXT NOT NULL REFERENCES summaries(id) ON DELETE CASCADE,
        text        TEXT NOT NULL,
        owner       TEXT,
        due_at      TEXT,
        status      TEXT NOT NULL,
        created_at  TEXT NOT NULL,
        updated_at  TEXT NOT NULL
    );
    CREATE INDEX idx_action_items_summary ON action_items(summary_id);
    CREATE INDEX idx_action_items_status ON action_items(status, due_at);

    -- Generic association table backing the `graph` crate. Deliberately untyped at the
    -- SQL level: node kinds are validated in `graph`, so adding a new entity type does
    -- not require a schema migration.
    CREATE TABLE edges (
        id          TEXT PRIMARY KEY NOT NULL,
        from_kind   TEXT NOT NULL,
        from_id     TEXT NOT NULL,
        edge_kind   TEXT NOT NULL,
        to_kind     TEXT NOT NULL,
        to_id       TEXT NOT NULL,
        created_at  TEXT NOT NULL,
        UNIQUE (from_kind, from_id, edge_kind, to_kind, to_id)
    );
    CREATE INDEX idx_edges_from ON edges(from_kind, from_id);
    CREATE INDEX idx_edges_to ON edges(to_kind, to_id);
    "#,
    // v2 — workspace layer: notes, tickets, email drafts, notifications.
    r#"
    CREATE TABLE notes (
        id          TEXT PRIMARY KEY NOT NULL,
        project_id  TEXT REFERENCES projects(id) ON DELETE SET NULL,
        title       TEXT NOT NULL,
        body        TEXT NOT NULL,
        created_at  TEXT NOT NULL,
        updated_at  TEXT NOT NULL
    );
    CREATE INDEX idx_notes_project ON notes(project_id);

    CREATE TABLE tickets (
        id                 TEXT PRIMARY KEY NOT NULL,
        project_id         TEXT REFERENCES projects(id) ON DELETE SET NULL,
        title              TEXT NOT NULL,
        description        TEXT,
        status             TEXT NOT NULL,
        owner              TEXT,
        due_at             TEXT,
        external_provider  TEXT,
        external_id        TEXT,
        external_url       TEXT,
        created_at         TEXT NOT NULL,
        updated_at         TEXT NOT NULL
    );
    CREATE INDEX idx_tickets_project ON tickets(project_id);
    CREATE INDEX idx_tickets_status ON tickets(status, due_at);
    CREATE UNIQUE INDEX idx_tickets_external
        ON tickets(external_provider, external_id)
        WHERE external_provider IS NOT NULL;

    CREATE TABLE email_drafts (
        id          TEXT PRIMARY KEY NOT NULL,
        meeting_id  TEXT REFERENCES meetings(id) ON DELETE SET NULL,
        subject     TEXT NOT NULL,
        body        TEXT NOT NULL,
        recipients  TEXT NOT NULL,
        status      TEXT NOT NULL,
        variant     TEXT,
        created_at  TEXT NOT NULL,
        updated_at  TEXT NOT NULL
    );
    CREATE INDEX idx_drafts_meeting ON email_drafts(meeting_id);

    CREATE TABLE notifications (
        id            TEXT PRIMARY KEY NOT NULL,
        source_kind   TEXT NOT NULL,
        source_id     TEXT NOT NULL,
        recipient     TEXT NOT NULL,
        channel       TEXT NOT NULL,
        body          TEXT NOT NULL,
        read_at       TEXT,
        delivered_at  TEXT,
        created_at    TEXT NOT NULL
    );
    CREATE INDEX idx_notifications_recipient ON notifications(recipient, read_at);
    CREATE INDEX idx_notifications_source ON notifications(source_kind, source_id);
    "#,
    // v3 — full-text search across transcripts, notes, and tickets.
    //
    // External-content FTS would avoid duplicating text, but it requires the content
    // table's rowid to be stable, and ours are TEXT uuids. A standalone FTS table keeps
    // the sync explicit via triggers.
    r#"
    CREATE VIRTUAL TABLE search_index USING fts5(
        entity_kind UNINDEXED,
        entity_id   UNINDEXED,
        title,
        body,
        tokenize = 'unicode61'
    );

    CREATE TRIGGER notes_ai AFTER INSERT ON notes BEGIN
        INSERT INTO search_index(entity_kind, entity_id, title, body)
        VALUES ('note', new.id, new.title, new.body);
    END;
    CREATE TRIGGER notes_au AFTER UPDATE ON notes BEGIN
        DELETE FROM search_index WHERE entity_kind = 'note' AND entity_id = old.id;
        INSERT INTO search_index(entity_kind, entity_id, title, body)
        VALUES ('note', new.id, new.title, new.body);
    END;
    CREATE TRIGGER notes_ad AFTER DELETE ON notes BEGIN
        DELETE FROM search_index WHERE entity_kind = 'note' AND entity_id = old.id;
    END;

    CREATE TRIGGER tickets_ai AFTER INSERT ON tickets BEGIN
        INSERT INTO search_index(entity_kind, entity_id, title, body)
        VALUES ('ticket', new.id, new.title, COALESCE(new.description, ''));
    END;
    CREATE TRIGGER tickets_au AFTER UPDATE ON tickets BEGIN
        DELETE FROM search_index WHERE entity_kind = 'ticket' AND entity_id = old.id;
        INSERT INTO search_index(entity_kind, entity_id, title, body)
        VALUES ('ticket', new.id, new.title, COALESCE(new.description, ''));
    END;
    CREATE TRIGGER tickets_ad AFTER DELETE ON tickets BEGIN
        DELETE FROM search_index WHERE entity_kind = 'ticket' AND entity_id = old.id;
    END;

    CREATE TRIGGER segments_ai AFTER INSERT ON transcript_segments BEGIN
        INSERT INTO search_index(entity_kind, entity_id, title, body)
        VALUES ('transcript_segment', new.id, '', new.text);
    END;
    CREATE TRIGGER segments_ad AFTER DELETE ON transcript_segments BEGIN
        DELETE FROM search_index WHERE entity_kind = 'transcript_segment' AND entity_id = old.id;
    END;
    "#,
];

/// Schema version this build understands.
pub const SUPPORTED_VERSION: u32 = MIGRATIONS.len() as u32;

/// Read the schema version currently recorded in the database.
pub fn current_version(conn: &Connection) -> Result<u32> {
    let version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    Ok(version)
}

/// Apply every migration newer than the database's current version.
///
/// Each migration runs inside a transaction, so a failure part-way leaves the database at
/// the previous version rather than in a half-migrated state.
pub fn migrate(conn: &mut Connection) -> Result<u32> {
    let from = current_version(conn)?;

    if from > SUPPORTED_VERSION {
        return Err(StorageError::SchemaTooNew {
            found: from,
            supported: SUPPORTED_VERSION,
        });
    }

    for (index, sql) in MIGRATIONS.iter().enumerate() {
        let version = index as u32 + 1;
        if version <= from {
            continue;
        }

        let tx = conn.transaction()?;
        tx.execute_batch(sql)
            .map_err(|source| StorageError::Migration { version, source })?;
        // `user_version` does not accept a bound parameter, and `version` is derived from
        // a compile-time array index rather than user input.
        tx.pragma_update(None, "user_version", version)
            .map_err(|source| StorageError::Migration { version, source })?;
        tx.commit()?;
    }

    current_version(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        Connection::open_in_memory().expect("in-memory db")
    }

    #[test]
    fn fresh_database_starts_at_version_zero() {
        let conn = fresh();
        assert_eq!(current_version(&conn).unwrap(), 0);
    }

    #[test]
    fn migrate_reaches_supported_version() {
        let mut conn = fresh();
        let version = migrate(&mut conn).unwrap();
        assert_eq!(version, SUPPORTED_VERSION);
        assert!(SUPPORTED_VERSION >= 3, "expected at least 3 migrations");
    }

    #[test]
    fn migrate_is_idempotent() {
        let mut conn = fresh();
        migrate(&mut conn).unwrap();
        let second = migrate(&mut conn).unwrap();
        assert_eq!(second, SUPPORTED_VERSION);
    }

    #[test]
    fn all_expected_tables_exist_after_migration() {
        let mut conn = fresh();
        migrate(&mut conn).unwrap();

        for table in [
            "workspaces",
            "projects",
            "meetings",
            "transcript_segments",
            "summaries",
            "decisions",
            "action_items",
            "edges",
            "notes",
            "tickets",
            "email_drafts",
            "notifications",
            "search_index",
        ] {
            let count: u32 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table '{table}' missing after migration");
        }
    }

    #[test]
    fn rejects_database_from_a_newer_build() {
        let mut conn = fresh();
        conn.pragma_update(None, "user_version", SUPPORTED_VERSION + 5)
            .unwrap();

        let err = migrate(&mut conn).expect_err("should refuse a newer schema");
        assert!(matches!(err, StorageError::SchemaTooNew { .. }), "got {err:?}");
    }

    #[test]
    fn partial_migration_does_not_advance_version() {
        // Pre-create a table that migration v1 also creates, so v1 fails part-way.
        let mut conn = fresh();
        conn.execute_batch("CREATE TABLE meetings (id TEXT);").unwrap();

        let err = migrate(&mut conn).expect_err("should fail on conflicting table");
        assert!(matches!(err, StorageError::Migration { version: 1, .. }), "got {err:?}");
        assert_eq!(
            current_version(&conn).unwrap(),
            0,
            "version must not advance when a migration fails"
        );
    }
}
