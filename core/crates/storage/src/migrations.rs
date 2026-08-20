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
    // v4 — application-level configuration. A generic key/value table rather than an
    // `onboarding` table: this is not a domain object, it has no edges, and the next setting
    // to need persisting should not cost another migration.
    r#"
    CREATE TABLE app_settings (
        key        TEXT PRIMARY KEY,
        value      TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    "#,
    // v5 — the connector seam: external artifact records, per-connector account state, and
    // the outbound delivery queue. No tokens live here; credentials go to the OS keychain.
    //
    // Timestamps are TEXT in the format rusqlite's chrono impl emits for `DateTime<Utc>`:
    // `%F %T%.f%:z`, e.g. `2026-01-01 00:00:00+00:00`. That matters more here than elsewhere
    // in this schema, because `next_attempt_at` is compared and ordered rather than merely
    // displayed, and TEXT comparison is lexicographic. An RFC3339 `Z` literal sorts before
    // every space-separated value at byte 10, so a row written in that form would be due
    // forever and never claimed. Do not hand-write timestamp literals in another format, and
    // do not reach for DEFAULT CURRENT_TIMESTAMP — SQLite emits a third, also-incompatible
    // form with no offset.
    r#"
    CREATE TABLE external_items (
        id              TEXT PRIMARY KEY NOT NULL,
        connector_id    TEXT NOT NULL,
        external_id     TEXT NOT NULL,
        url             TEXT,
        title           TEXT,
        remote_version  TEXT,
        last_synced_at  TEXT NOT NULL,
        created_at      TEXT NOT NULL
    );
    CREATE UNIQUE INDEX idx_external_items_identity
        ON external_items(connector_id, external_id);

    CREATE TABLE connector_accounts (
        connector_id   TEXT PRIMARY KEY NOT NULL,
        account_label  TEXT,
        scopes         TEXT NOT NULL,
        status         TEXT NOT NULL,
        connected_at   TEXT NOT NULL,
        cursor         TEXT
    );

    CREATE TABLE connector_outbox (
        id               TEXT PRIMARY KEY NOT NULL,
        connector_id     TEXT NOT NULL,
        node_kind        TEXT NOT NULL,
        node_id          TEXT NOT NULL,
        operation        TEXT NOT NULL,
        payload          TEXT NOT NULL,
        idempotency_key  TEXT NOT NULL,
        status           TEXT NOT NULL,
        attempts         INTEGER NOT NULL DEFAULT 0,
        last_error       TEXT,
        next_attempt_at  TEXT NOT NULL,
        leased_until     TEXT,
        created_at       TEXT NOT NULL
    );
    CREATE UNIQUE INDEX idx_outbox_idempotency ON connector_outbox(idempotency_key);

    -- Partial, and keyed on next_attempt_at alone. A composite (status, next_attempt_at)
    -- index cannot serve the drain query's ORDER BY, because `status IN (...)` splits the
    -- scan into two disjoint ranges and SQLite falls back to a temp b-tree — which also
    -- defeats the LIMIT, sorting every due row before returning a handful. Partial also
    -- keeps completed rows out of the index entirely; they are retained forever but never
    -- looked up by status.
    CREATE INDEX idx_outbox_ready ON connector_outbox(next_attempt_at)
        WHERE status IN ('pending', 'in_flight');
    CREATE INDEX idx_outbox_failed ON connector_outbox(created_at DESC)
        WHERE status = 'failed';
    "#,
    // v6 — people, meeting series, and work items that outlive their summary.
    //
    // Three changes in one migration because they interlock:
    //
    //   1. `people` gives a person a row, so speaker attribution and action-item ownership
    //      can point at an identity instead of repeating a display name as free text.
    //   2. `meeting_series` threads recurring meetings, which is what turns "still open
    //      three standups later" into a traversal rather than a string match on titles.
    //   3. `action_items` and `decisions` move from `summary_id` to `meeting_id`.
    //
    // (3) defuses a landmine rather than fixing an active bug, and the distinction matters
    // to anyone auditing this later. Both tables were ON DELETE CASCADE from `summaries`.
    // Nothing deletes a summary today — `summarize_meeting` appends a new row and leaves
    // the old one — so no data has actually been lost. But that is the only thing standing
    // between the old schema and silent loss: the obvious next change to summarisation
    // (replace on regenerate, or prune old summaries so they stop accumulating) would have
    // deleted every action item derived from that summary, taking with it the owner, due
    // date and status a user had set by hand. Ownership belonged on the meeting either way;
    // work outlives the summary that first described it. `summary_id` is kept as nullable
    // provenance and degrades to NULL rather than taking the row with it.
    //
    // The rewrite runs with foreign keys still enabled. `migrate` wraps each migration in a
    // transaction and `PRAGMA foreign_keys` is a no-op inside one, so the usual
    // disable-around-a-table-rewrite recipe is not available here. It is safe in this case
    // only because no other table references `action_items` or `decisions`: the reason that
    // recipe exists is to stop other tables' REFERENCES clauses from following the
    // temporary name through the rename. Verify that premise still holds before reusing
    // this pattern on a table that something else points at.
    r#"
    CREATE TABLE people (
        id           TEXT PRIMARY KEY NOT NULL,
        display_name TEXT NOT NULL,
        email        TEXT,
        -- Voiceprint columns exist so speaker enrollment has somewhere to land without a
        -- second migration. NOTHING WRITES THEM YET, deliberately: a speaker embedding is a
        -- biometric identifier for someone who is usually not the user of this machine and
        -- who never consented to being enrolled. Populating these is gated on an explicit
        -- consent and encryption-at-rest decision.
        voice_print  BLOB,
        voice_dims   INTEGER,
        -- Which embedding model produced `voice_print`. Cosine distance between vectors
        -- from different models is meaningless and the bytes do not say which model made
        -- them, so without this a model swap silently produces confident wrong matches on
        -- named people. `voice_dims` does not cover it — two models can share a width.
        voice_model  TEXT,
        created_at   TEXT NOT NULL,
        updated_at   TEXT NOT NULL
    );
    -- Partial, so any number of people may have no email while a known address stays unique.
    CREATE UNIQUE INDEX idx_people_email ON people(email) WHERE email IS NOT NULL;

    CREATE TABLE meeting_participants (
        meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
        person_id  TEXT NOT NULL REFERENCES people(id)   ON DELETE CASCADE,
        role       TEXT,
        PRIMARY KEY (meeting_id, person_id)
    );
    CREATE INDEX idx_participants_person ON meeting_participants(person_id);

    CREATE TABLE meeting_series (
        id         TEXT PRIMARY KEY NOT NULL,
        title      TEXT NOT NULL,
        project_id TEXT REFERENCES projects(id) ON DELETE SET NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    ALTER TABLE meetings ADD COLUMN series_id TEXT REFERENCES meeting_series(id);
    CREATE INDEX idx_meetings_series ON meetings(series_id, started_at DESC);

    -- Paired with the existing free-text `speaker`, never derived from it. Free text
    -- survives when attribution is anonymous or a platform tap gives no identity; the
    -- foreign key is set only when there is a real person to point at. Both stay nullable.
    ALTER TABLE transcript_segments ADD COLUMN speaker_id TEXT REFERENCES people(id);
    CREATE INDEX idx_segments_speaker ON transcript_segments(speaker_id);

    CREATE TABLE action_items_v6 (
        id              TEXT PRIMARY KEY NOT NULL,
        meeting_id      TEXT NOT NULL REFERENCES meetings(id)  ON DELETE CASCADE,
        summary_id      TEXT          REFERENCES summaries(id) ON DELETE SET NULL,
        text            TEXT NOT NULL,
        owner           TEXT,
        owner_person_id TEXT REFERENCES people(id) ON DELETE SET NULL,
        due_at          TEXT,
        status          TEXT NOT NULL,
        created_at      TEXT NOT NULL,
        updated_at      TEXT NOT NULL
    );
    INSERT INTO action_items_v6
        (id, meeting_id, summary_id, text, owner, due_at, status, created_at, updated_at)
    SELECT a.id, s.meeting_id, a.summary_id, a.text, a.owner, a.due_at, a.status,
           a.created_at, a.updated_at
      FROM action_items a
      JOIN summaries s ON s.id = a.summary_id;
    DROP TABLE action_items;
    ALTER TABLE action_items_v6 RENAME TO action_items;
    CREATE INDEX idx_action_items_meeting ON action_items(meeting_id);
    CREATE INDEX idx_action_items_summary ON action_items(summary_id);
    CREATE INDEX idx_action_items_status  ON action_items(status, due_at);
    CREATE INDEX idx_action_items_owner   ON action_items(owner_person_id);

    CREATE TABLE decisions_v6 (
        id         TEXT PRIMARY KEY NOT NULL,
        meeting_id TEXT NOT NULL REFERENCES meetings(id)  ON DELETE CASCADE,
        summary_id TEXT          REFERENCES summaries(id) ON DELETE SET NULL,
        text       TEXT NOT NULL,
        reasoning  TEXT,
        decided_at TEXT
    );
    INSERT INTO decisions_v6 (id, meeting_id, summary_id, text, reasoning, decided_at)
    SELECT d.id, s.meeting_id, d.summary_id, d.text, d.reasoning, d.decided_at
      FROM decisions d
      JOIN summaries s ON s.id = d.summary_id;
    DROP TABLE decisions;
    ALTER TABLE decisions_v6 RENAME TO decisions;
    CREATE INDEX idx_decisions_meeting ON decisions(meeting_id);
    CREATE INDEX idx_decisions_summary ON decisions(summary_id);
    "#,
    // v7 — a recoverable delete for notes.
    //
    // A note is frequently the only copy of something a person typed, and the previous
    // `DELETE FROM notes` was unrecoverable behind a single confirm dialog. Soft delete
    // makes the destructive step reversible; emptying the trash is the irreversible one,
    // and is now the only path that reaches `DELETE`.
    //
    // Only notes get this. Meetings own transcripts and audio and want a different
    // conversation; tickets mirror external systems where deletion has to propagate. Adding
    // a `deleted_at` to every table "for symmetry" would mean every query in the codebase
    // grows a filter it does not need.
    r#"
    ALTER TABLE notes ADD COLUMN deleted_at TEXT;
    -- Partial: the index exists to find the few trashed notes, not to catalogue the many
    -- live ones. A full index here would be almost entirely NULLs.
    CREATE INDEX idx_notes_deleted ON notes(deleted_at) WHERE deleted_at IS NOT NULL;

    -- Trashing is an UPDATE, so without this the note would stay in the search index and
    -- keep surfacing in results after the user deleted it. Restoring re-indexes it, because
    -- that is an UPDATE too.
    DROP TRIGGER notes_au;
    CREATE TRIGGER notes_au AFTER UPDATE ON notes BEGIN
        DELETE FROM search_index WHERE entity_kind = 'note' AND entity_id = old.id;
        INSERT INTO search_index(entity_kind, entity_id, title, body)
        SELECT 'note', new.id, new.title, new.body WHERE new.deleted_at IS NULL;
    END;
    "#,
    // v8 — vectors, so search can find "cost structure" when you asked about "pricing".
    //
    // Derived data, entirely. Every row here can be rebuilt from the entity it points at, and
    // deleting the whole table costs an indexing run and nothing else. That is why there are
    // no foreign keys: the referenced kinds are heterogeneous, exactly as in `edges`, and a
    // stale row is discarded on read rather than being a corruption.
    //
    // `model` is the column that matters. Cosine distance between vectors from two different
    // models is not a small error — it is meaningless — and the bytes do not say which model
    // produced them. Without this, switching from nomic-embed-text to bge-m3 yields confident
    // nonsense instead of an obvious miss. `dims` does not cover it: two models can share a
    // width.
    //
    // `source_updated_at` is how staleness is detected. An entity edited after its chunk was
    // embedded needs re-embedding, and comparing timestamps is cheaper than hashing every
    // transcript on every indexing pass.
    //
    // No vector index. There is no ANN structure here and no sqlite-vec extension, because
    // `rusqlite` is `bundled` on purpose — loading an extension would break the reproducible
    // build across platforms that decision buys. Similarity is a linear scan in Rust, which
    // is microseconds for the thousands of chunks a personal workspace holds and would need
    // revisiting at a scale this product does not target.
    r#"
    CREATE TABLE embeddings (
        id                TEXT PRIMARY KEY NOT NULL,
        entity_kind       TEXT NOT NULL,
        entity_id         TEXT NOT NULL,
        chunk_index       INTEGER NOT NULL,
        text              TEXT NOT NULL,
        -- Raw little-endian f32s. A BLOB rather than JSON: 768 floats are 3 KB packed and
        -- about 12 KB as text, and the whole table is read on every query.
        vector            BLOB NOT NULL,
        dims              INTEGER NOT NULL,
        model             TEXT NOT NULL,
        source_updated_at TEXT NOT NULL,
        created_at        TEXT NOT NULL,
        UNIQUE (entity_kind, entity_id, chunk_index, model)
    );
    CREATE INDEX idx_embeddings_entity ON embeddings(entity_kind, entity_id);
    CREATE INDEX idx_embeddings_model  ON embeddings(model);
    "#,
    // v9 — put the speaker in the search index.
    //
    // Since v3 the segment triggers have written `''` into the indexed `title` column and only
    // `new.text` into the body, so **who said something has never been searchable**. "What did
    // Sam commit to?" found nothing unless another speaker happened to say the word "Sam" out
    // loud. That is one of the most natural questions to put to a transcript, and it is the
    // one piece of structure a transcript has that prose does not.
    //
    // The missing UPDATE trigger matters as much as the wrong INSERT one. Speaker attribution
    // arrives *after* the segment does — diarization assigns it when a recording ends, and the
    // browser extension can name someone later still — so a segment indexed at insert with no
    // speaker would never pick one up. Without `segments_au`, every name assigned after the
    // fact was invisible to search.
    r#"
    DROP TRIGGER segments_ai;
    CREATE TRIGGER segments_ai AFTER INSERT ON transcript_segments BEGIN
        INSERT INTO search_index(entity_kind, entity_id, title, body)
        VALUES ('transcript_segment', new.id, COALESCE(new.speaker, ''), new.text);
    END;

    CREATE TRIGGER segments_au AFTER UPDATE ON transcript_segments BEGIN
        DELETE FROM search_index
         WHERE entity_kind = 'transcript_segment' AND entity_id = old.id;
        INSERT INTO search_index(entity_kind, entity_id, title, body)
        VALUES ('transcript_segment', new.id, COALESCE(new.speaker, ''), new.text);
    END;

    -- Rebuild what the old triggers wrote. Delete-then-insert rather than UPDATE: an FTS5
    -- table stores a tokenized index rather than the row, and replacing the entry is the
    -- documented way to change one.
    DELETE FROM search_index WHERE entity_kind = 'transcript_segment';
    INSERT INTO search_index(entity_kind, entity_id, title, body)
    SELECT 'transcript_segment', id, COALESCE(speaker, ''), text FROM transcript_segments;
    "#,
    // v10 — a recoverable delete for meetings.
    //
    // Meetings were undeletable from the UI, which was defensible while there was no trash and
    // no undo: a meeting owns its transcript, its summaries, its decisions and its action
    // items, and every one of those cascades. Destroying all of it behind a single confirm
    // dialog is not a button worth having.
    //
    // With a trash it becomes ordinary. The reversible step hides the meeting; the
    // irreversible one is emptying the trash, and it is the only path that reaches a `DELETE`.
    //
    // The segment triggers are conditional on the *meeting's* state, which is why they need
    // rewriting again one migration after v9. A trashed meeting whose lines stayed in the
    // index would keep answering questions — including through the agent, which reads search
    // results — after the user had deleted it. That is the same failure the v7 note trigger
    // exists to prevent, one level up.
    r#"
    ALTER TABLE meetings ADD COLUMN deleted_at TEXT;
    CREATE INDEX idx_meetings_deleted ON meetings(deleted_at) WHERE deleted_at IS NOT NULL;

    -- Segments follow their meeting into and out of the trash. `segments_ai` gains the same
    -- condition so importing into an already-trashed meeting cannot resurrect it in search.
    DROP TRIGGER segments_ai;
    CREATE TRIGGER segments_ai AFTER INSERT ON transcript_segments BEGIN
        INSERT INTO search_index(entity_kind, entity_id, title, body)
        SELECT 'transcript_segment', new.id, COALESCE(new.speaker, ''), new.text
         WHERE (SELECT deleted_at FROM meetings WHERE id = new.meeting_id) IS NULL;
    END;

    DROP TRIGGER segments_au;
    CREATE TRIGGER segments_au AFTER UPDATE ON transcript_segments BEGIN
        DELETE FROM search_index
         WHERE entity_kind = 'transcript_segment' AND entity_id = old.id;
        INSERT INTO search_index(entity_kind, entity_id, title, body)
        SELECT 'transcript_segment', new.id, COALESCE(new.speaker, ''), new.text
         WHERE (SELECT deleted_at FROM meetings WHERE id = new.meeting_id) IS NULL;
    END;

    -- Trashing or restoring a meeting rewrites its segments' index entries in one statement.
    CREATE TRIGGER meetings_au AFTER UPDATE OF deleted_at ON meetings BEGIN
        DELETE FROM search_index
         WHERE entity_kind = 'transcript_segment'
           AND entity_id IN (SELECT id FROM transcript_segments WHERE meeting_id = new.id);
        INSERT INTO search_index(entity_kind, entity_id, title, body)
        SELECT 'transcript_segment', id, COALESCE(speaker, ''), text
          FROM transcript_segments
         WHERE meeting_id = new.id AND new.deleted_at IS NULL;
    END;
    "#,
    // v11 — summary templates, and the template recorded on what it produced.
    //
    // One prompt produced every summary, so a sales call and an architecture review got the same
    // treatment and a user who wanted "decisions and owners only" had no way to ask.
    //
    // A table rather than a settings blob, unlike the routing rules: these are referenced by
    // foreign key from `summaries`, queried individually, and edited one at a time. The opposite
    // call to the routing rule set, for the opposite reasons.
    //
    // `template_id` is nullable because every summary that already exists was produced before
    // templates did, and deleting a template must not erase the record of what produced a
    // summary — which is why the reference does not cascade.
    //
    // The three built-ins are seeded rows rather than hardcoded constants, so a user can copy one
    // and edit it. `is_builtin` exists so the repository can refuse to delete the last way of
    // summarising anything.
    r#"
    CREATE TABLE summary_templates (
        id          TEXT PRIMARY KEY NOT NULL,
        name        TEXT NOT NULL UNIQUE,
        prompt      TEXT NOT NULL,
        is_builtin  INTEGER NOT NULL DEFAULT 0,
        created_at  TEXT NOT NULL,
        updated_at  TEXT NOT NULL
    );

    ALTER TABLE summaries ADD COLUMN template_id TEXT REFERENCES summary_templates(id);

    INSERT INTO summary_templates (id, name, prompt, is_builtin, created_at, updated_at) VALUES
        ('00000000-0000-4000-8000-000000000001', 'General meeting',
         'Summarise this meeting. Lead with what was decided and who owes what. Keep it to what was actually said — omit a section rather than filling it.',
         1, '2026-08-19T00:00:00+00:00', '2026-08-19T00:00:00+00:00'),
        ('00000000-0000-4000-8000-000000000002', 'Sales call',
         'Summarise this sales call. Cover: what the customer asked for, objections raised, pricing discussed, and the agreed next step with its owner and date. Do not infer budget or intent that was not stated.',
         1, '2026-08-19T00:00:00+00:00', '2026-08-19T00:00:00+00:00'),
        ('00000000-0000-4000-8000-000000000003', 'Engineering review',
         'Summarise this engineering discussion. Cover: the decision reached, the alternatives rejected and why, any risk or unknown that was named, and follow-up work with owners. Prefer the reasoning over the conclusion.',
         1, '2026-08-19T00:00:00+00:00', '2026-08-19T00:00:00+00:00');
    "#,
    // v12 — a pointer to retained audio, for the meetings whose owner asked for it.
    //
    // Audio was never kept: capture streamed into transcription and the samples were gone. That
    // made two things impossible — hearing the moment a line was said, and re-transcribing with a
    // better model when the first pass invented something.
    //
    // A path and a size rather than a blob. An hour of audio is tens of megabytes; in the database
    // it would be copied by every backup, walked by every `VACUUM`, and read into memory by a
    // range request. On disk, deletion is a filesystem operation that can be verified.
    //
    // Nullable, and null for every meeting that already exists, because retention is off by default
    // and enabling it later must not make earlier meetings look broken.
    r#"
    ALTER TABLE meetings ADD COLUMN audio_path TEXT;
    ALTER TABLE meetings ADD COLUMN audio_bytes INTEGER;
    CREATE INDEX idx_meetings_audio ON meetings(audio_path) WHERE audio_path IS NOT NULL;
    "#,
    // v13 — jobs that run without anybody watching, and a durable account of what they did.
    //
    // `agent.rs` keeps its runs in memory, arguing that a trace matters only while it is happening
    // and that the note it wrote survives anyway. That argument assumes somebody was present. For a
    // run at 6am nobody was, and "it failed on Tuesday and I need to know why" is the normal case —
    // so the trace is persisted here.
    //
    // Bounded on write rather than swept: a job firing every fifteen minutes would otherwise grow
    // this table forever, which is a disk-space bug with a slow fuse.
    //
    // `timezone` sits beside `cron` because "every Friday at 5pm" means the user's Friday. Storing
    // it explicitly makes a DST transition or a relocation interpretable rather than mysterious.
    //
    // `job_allowed_tools` from the design is deliberately *not* here. It governs which external
    // tools a run may propose, which needs the MCP tables that do not exist yet — so it belongs in
    // that migration, next to the tables it relates to, rather than sitting empty here.
    r#"
    CREATE TABLE jobs (
        id           TEXT PRIMARY KEY NOT NULL,
        name         TEXT NOT NULL UNIQUE,
        prompt       TEXT NOT NULL,
        cron         TEXT NOT NULL,
        timezone     TEXT NOT NULL,
        enabled      INTEGER NOT NULL DEFAULT 1,
        catch_up     INTEGER NOT NULL DEFAULT 0,
        timeout_secs INTEGER NOT NULL DEFAULT 600,
        created_at   TEXT NOT NULL,
        updated_at   TEXT NOT NULL
    );

    CREATE TABLE job_runs (
        id           TEXT PRIMARY KEY NOT NULL,
        job_id       TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
        status       TEXT NOT NULL,
        trace        TEXT,
        note_id      TEXT REFERENCES notes(id) ON DELETE SET NULL,
        proposals    INTEGER NOT NULL DEFAULT 0,
        error        TEXT,
        started_at   TEXT NOT NULL,
        finished_at  TEXT
    );

    CREATE INDEX idx_job_runs_job ON job_runs(job_id, started_at DESC);
    "#,
    // v14 — things worth remembering about the person using this.
    //
    // Off by default and capped hard. Memory is injected into prompts that already carry retrieved
    // material and a transcript, so an unbounded list crowds out the actual content and makes every
    // answer slightly worse in a way nobody can attribute. Reaching a cap forces a choice, and
    // forcing the choice is the point.
    //
    // The CHECK makes the scope/project pairing a schema invariant rather than a convention: a
    // global memory carrying a project id, or a project memory without one, cannot exist.
    //
    // `source_meeting_id` is ON DELETE SET NULL. A memory outlives the meeting that produced it, but
    // while that meeting exists the provenance is what lets the UI answer "why does it think that".
    //
    // Extraction state is its own table rather than a column on `meetings`, for the reason v8
    // applies to embeddings: it is derived state about a background pass, and `meetings` should not
    // grow a column every time something processes it.
    r#"
    CREATE TABLE memories (
        id                TEXT PRIMARY KEY NOT NULL,
        scope             TEXT NOT NULL,
        project_id        TEXT REFERENCES projects(id) ON DELETE CASCADE,
        text              TEXT NOT NULL,
        origin            TEXT NOT NULL,
        source_meeting_id TEXT REFERENCES meetings(id) ON DELETE SET NULL,
        created_at        TEXT NOT NULL,
        updated_at        TEXT NOT NULL,
        CHECK ((scope = 'project') = (project_id IS NOT NULL))
    );

    CREATE INDEX idx_memories_scope ON memories(scope, project_id);

    CREATE TABLE memory_extraction_state (
        meeting_id   TEXT PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE,
        processed_at TEXT NOT NULL
    );
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
        const { assert!(SUPPORTED_VERSION >= 3, "expected at least 3 migrations") };
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
            "external_items",
            "connector_accounts",
            "connector_outbox",
            "people",
            "meeting_participants",
            "meeting_series",
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

    /// Each migration's `// vN` comment must match the array position it actually occupies.
    ///
    /// The version a migration produces comes from its index, never from its comment, and
    /// until this test existed nothing checked that the two agreed. They disagreed once
    /// already: an untagged entry made a hand count come up short and a migration shipped
    /// labelled v4 from slot 5 (fixed in 857e965). The tag lives in a `//` comment outside
    /// the `r#"..."#` literal, so this has to read the source rather than the array.
    #[test]
    fn migration_version_tags_match_their_array_positions() {
        let source = include_str!("migrations.rs");
        let body = source
            .split_once("const MIGRATIONS: &[&str] = &[")
            .expect("MIGRATIONS array should exist")
            .1;

        let tags: Vec<u32> = body
            .lines()
            // Stop at the end of the array literal, so the doc comment above and this
            // test's own prose cannot be mistaken for entries.
            .take_while(|line| !line.starts_with("];"))
            .filter_map(|line| {
                let rest = line.trim().strip_prefix("// v")?;
                let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                digits.parse().ok()
            })
            .collect();

        assert_eq!(
            tags.len(),
            MIGRATIONS.len(),
            "every migration needs a `// vN` tag: found {} tag(s) for {} migration(s). \
             An untagged entry is what caused the v4/v5 mislabelling.",
            tags.len(),
            MIGRATIONS.len()
        );

        for (index, tag) in tags.iter().enumerate() {
            let expected = index as u32 + 1;
            assert_eq!(
                *tag, expected,
                "entry {index} is tagged v{tag} but its array position makes it v{expected}"
            );
        }
    }

    /// Regenerating a summary must not delete the action items derived from it.
    ///
    /// Before v6 both tables cascaded from `summaries`, so re-summarising a meeting silently
    /// destroyed every action item along with the owner, due date and status a user had set
    /// by hand. This is the regression test for that.
    #[test]
    fn action_items_and_decisions_survive_their_summary() {
        let mut conn = fresh();
        migrate(&mut conn).unwrap();

        conn.execute_batch(
            "INSERT INTO meetings (id, title, source, started_at, created_at, updated_at)
             VALUES ('m1', 'Standup', 'live', '2026-01-01T09:00:00Z',
                     '2026-01-01T09:00:00Z', '2026-01-01T09:00:00Z');
             INSERT INTO summaries (id, meeting_id, text, model, created_at)
             VALUES ('s1', 'm1', 'first pass', 'mock', '2026-01-01T09:30:00Z');
             INSERT INTO action_items
                 (id, meeting_id, summary_id, text, owner, status, created_at, updated_at)
             VALUES ('a1', 'm1', 's1', 'ship the thing', 'priya', 'in_progress',
                     '2026-01-01T09:30:00Z', '2026-01-01T09:30:00Z');
             INSERT INTO decisions (id, meeting_id, summary_id, text)
             VALUES ('d1', 'm1', 's1', 'we ship on friday');",
        )
        .unwrap();

        // Re-summarising deletes the old summary row.
        conn.execute("DELETE FROM summaries WHERE id = 's1'", [])
            .unwrap();

        let (owner, status, summary_id): (String, String, Option<String>) = conn
            .query_row(
                "SELECT owner, status, summary_id FROM action_items WHERE id = 'a1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("action item must outlive the summary that proposed it");
        assert_eq!(owner, "priya", "hand-set owner must survive");
        assert_eq!(status, "in_progress", "hand-set status must survive");
        assert_eq!(
            summary_id, None,
            "provenance degrades to NULL, not deletion"
        );

        let decisions: u32 = conn
            .query_row("SELECT COUNT(*) FROM decisions WHERE id = 'd1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(decisions, 1, "decision must outlive its summary");
    }

    /// Deleting the *meeting* should still take its work items with it — the v6 rewrite
    /// moved ownership, it did not remove it.
    #[test]
    fn deleting_a_meeting_still_cascades_to_its_work_items() {
        let mut conn = fresh();
        migrate(&mut conn).unwrap();

        conn.execute_batch(
            "INSERT INTO meetings (id, title, source, started_at, created_at, updated_at)
             VALUES ('m1', 'Standup', 'live', '2026-01-01T09:00:00Z',
                     '2026-01-01T09:00:00Z', '2026-01-01T09:00:00Z');
             INSERT INTO action_items
                 (id, meeting_id, text, status, created_at, updated_at)
             VALUES ('a1', 'm1', 'ship the thing', 'todo',
                     '2026-01-01T09:30:00Z', '2026-01-01T09:30:00Z');
             INSERT INTO decisions (id, meeting_id, text)
             VALUES ('d1', 'm1', 'we ship on friday');",
        )
        .unwrap();

        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute("DELETE FROM meetings WHERE id = 'm1'", [])
            .unwrap();

        for table in ["action_items", "decisions"] {
            let count: u32 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "{table} should cascade from its meeting");
        }
    }

    #[test]
    fn rejects_database_from_a_newer_build() {
        let mut conn = fresh();
        conn.pragma_update(None, "user_version", SUPPORTED_VERSION + 5)
            .unwrap();

        let err = migrate(&mut conn).expect_err("should refuse a newer schema");
        assert!(
            matches!(err, StorageError::SchemaTooNew { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn partial_migration_does_not_advance_version() {
        // Pre-create a table that migration v1 also creates, so v1 fails part-way.
        let mut conn = fresh();
        conn.execute_batch("CREATE TABLE meetings (id TEXT);")
            .unwrap();

        let err = migrate(&mut conn).expect_err("should fail on conflicting table");
        assert!(
            matches!(err, StorageError::Migration { version: 1, .. }),
            "got {err:?}"
        );
        assert_eq!(
            current_version(&conn).unwrap(),
            0,
            "version must not advance when a migration fails"
        );
    }

    #[test]
    fn v5_creates_connector_tables() {
        let mut conn = fresh();
        migrate(&mut conn).unwrap();

        for table in ["external_items", "connector_accounts", "connector_outbox"] {
            let count: u32 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing table {table}");
        }
    }

    #[test]
    fn outbox_idempotency_key_is_unique() {
        let mut conn = fresh();
        migrate(&mut conn).unwrap();

        let insert = "INSERT INTO connector_outbox
            (id, connector_id, node_kind, node_id, operation, payload, idempotency_key,
             status, attempts, next_attempt_at, created_at)
            VALUES (?1, 'vault', 'meeting', 'n1', 'create', '{}', 'dupe',
                    'pending', 0, '2026-01-01 00:00:00+00:00', '2026-01-01 00:00:00+00:00')";

        conn.execute(insert, ["a"]).unwrap();
        assert!(
            conn.execute(insert, ["b"]).is_err(),
            "a second row with the same idempotency_key must be rejected"
        );
    }
}
