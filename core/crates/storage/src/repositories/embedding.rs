//! Stored vectors, and what they were made from.
//!
//! Everything here is derived data — see the v8 migration for why that shapes the schema. The
//! repository's job is to make three things cheap: writing a batch, reading every vector for
//! one model, and knowing what has gone stale.

use chrono::{DateTime, Utc};
use rusqlite::Row;

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::id::Id;

/// A chunk of text and its vector.
#[derive(Debug, Clone, PartialEq)]
pub struct Embedding {
    pub id: Id,
    /// `meeting`, `note`, or `ticket` — matching `NodeKind` naming.
    pub entity_kind: String,
    pub entity_id: Id,
    /// Position within the entity, so chunks come back in reading order.
    pub chunk_index: i64,
    pub text: String,
    pub vector: Vec<f32>,
    pub model: String,
    /// When the entity was last edited, as of embedding. Drives staleness.
    pub source_updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewEmbedding {
    pub entity_kind: String,
    pub entity_id: Id,
    pub chunk_index: i64,
    pub text: String,
    pub vector: Vec<f32>,
    pub model: String,
    pub source_updated_at: DateTime<Utc>,
}

/// What has been embedded for one entity, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedEntity {
    pub entity_kind: String,
    pub entity_id: Id,
    pub source_updated_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct EmbeddingRepository<'a> {
    db: &'a Database,
}

impl<'a> EmbeddingRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Replace every chunk for one entity, in a transaction.
    ///
    /// Replace rather than insert: re-embedding an edited note that got shorter must not leave
    /// the chunks that no longer exist behind, still matching queries and citing text the note
    /// does not contain. Deleting first is what guarantees that, and the transaction is what
    /// stops a failure halfway through leaving the entity with no vectors at all.
    pub fn replace_for_entity(
        &self,
        entity_kind: &str,
        entity_id: Id,
        model: &str,
        chunks: Vec<NewEmbedding>,
    ) -> Result<usize> {
        let conn = self.db.conn();

        // The same explicit-batch idiom the segment writer uses: `Database` hands out a shared
        // `&Connection`, and `Connection::transaction` needs `&mut`.
        conn.execute_batch("BEGIN")?;
        let write = || -> Result<usize> {
            conn.execute(
                "DELETE FROM embeddings
                 WHERE entity_kind = ?1 AND entity_id = ?2 AND model = ?3",
                rusqlite::params![entity_kind, entity_id, model],
            )?;

            let mut stmt = conn.prepare(
                "INSERT INTO embeddings
                    (id, entity_kind, entity_id, chunk_index, text, vector, dims, model,
                     source_updated_at, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;

            let now = Utc::now();
            let mut written = 0;
            for chunk in &chunks {
                stmt.execute(rusqlite::params![
                    Id::new(),
                    chunk.entity_kind,
                    chunk.entity_id,
                    chunk.chunk_index,
                    chunk.text,
                    to_blob(&chunk.vector),
                    chunk.vector.len() as i64,
                    chunk.model,
                    chunk.source_updated_at,
                    now,
                ])?;
                written += 1;
            }
            Ok(written)
        };

        match write() {
            Ok(written) => {
                conn.execute_batch("COMMIT")?;
                Ok(written)
            }
            Err(e) => {
                conn.execute_batch("ROLLBACK")?;
                Err(e)
            }
        }
    }

    /// Every vector produced by one model.
    ///
    /// Scoped to a model because comparing across models is meaningless. Loaded whole rather
    /// than filtered in SQL: similarity is not something SQLite can rank, so the scan happens
    /// in Rust either way, and one query beats one per candidate.
    pub fn all_for_model(&self, model: &str) -> Result<Vec<Embedding>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, entity_kind, entity_id, chunk_index, text, vector, dims, model,
                    source_updated_at
             FROM embeddings WHERE model = ?1
             ORDER BY entity_kind, entity_id, chunk_index",
        )?;
        let rows = stmt.query_map(rusqlite::params![model], map_embedding)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// What this model has already indexed, so a caller can skip it.
    pub fn indexed_entities(&self, model: &str) -> Result<Vec<IndexedEntity>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT entity_kind, entity_id, MAX(source_updated_at)
             FROM embeddings WHERE model = ?1
             GROUP BY entity_kind, entity_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![model], |row| {
            Ok(IndexedEntity {
                entity_kind: row.get(0)?,
                entity_id: row.get(1)?,
                source_updated_at: row.get(2)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Drop one entity's vectors, for every model.
    ///
    /// Called when the entity itself is destroyed. Nothing cascades here — the table
    /// references heterogeneous kinds and so has no foreign keys.
    pub fn delete_for_entity(&self, entity_kind: &str, entity_id: Id) -> Result<usize> {
        Ok(self.db.conn().execute(
            "DELETE FROM embeddings WHERE entity_kind = ?1 AND entity_id = ?2",
            rusqlite::params![entity_kind, entity_id],
        )?)
    }

    /// Throw the index away. The next run rebuilds it.
    pub fn clear(&self) -> Result<usize> {
        Ok(self.db.conn().execute("DELETE FROM embeddings", [])?)
    }

    /// Vectors from models other than this one, which can never be compared against it.
    pub fn count_from_other_models(&self, model: &str) -> Result<u64> {
        Ok(self.db.conn().query_row(
            "SELECT COUNT(*) FROM embeddings WHERE model != ?1",
            rusqlite::params![model],
            |row| row.get(0),
        )?)
    }

    pub fn count(&self, model: &str) -> Result<u64> {
        Ok(self.db.conn().query_row(
            "SELECT COUNT(*) FROM embeddings WHERE model = ?1",
            rusqlite::params![model],
            |row| row.get(0),
        )?)
    }
}

fn to_blob(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// Read a vector back, checking the length the row claims.
///
/// A blob whose byte count disagrees with `dims` is corrupt, and silently returning a shorter
/// vector would make every comparison against it score zero — a wrong answer that looks like a
/// weak match rather than a fault.
fn from_blob(bytes: &[u8], dims: i64) -> std::result::Result<Vec<f32>, String> {
    if !bytes.len().is_multiple_of(4) {
        return Err(format!(
            "{} bytes is not a whole number of f32s",
            bytes.len()
        ));
    }

    let vector: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();

    if vector.len() as i64 != dims {
        return Err(format!(
            "row claims {dims} dimensions but holds {}",
            vector.len()
        ));
    }

    Ok(vector)
}

fn map_embedding(row: &Row<'_>) -> rusqlite::Result<Embedding> {
    let bytes: Vec<u8> = row.get(5)?;
    let dims: i64 = row.get(6)?;

    let vector = from_blob(&bytes, dims).map_err(|reason| {
        rusqlite::Error::FromSqlConversionFailure(
            5,
            rusqlite::types::Type::Blob,
            Box::new(StorageError::Corrupt {
                column: "embeddings.vector",
                reason,
            }),
        )
    })?;

    Ok(Embedding {
        id: row.get(0)?,
        entity_kind: row.get(1)?,
        entity_id: row.get(2)?,
        chunk_index: row.get(3)?,
        text: row.get(4)?,
        vector,
        model: row.get(7)?,
        source_updated_at: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn chunk(entity: Id, index: i64, vector: Vec<f32>, model: &str) -> NewEmbedding {
        NewEmbedding {
            entity_kind: "note".into(),
            entity_id: entity,
            chunk_index: index,
            text: format!("chunk {index}"),
            vector,
            model: model.into(),
            source_updated_at: Utc::now(),
        }
    }

    #[test]
    fn a_vector_survives_the_round_trip_exactly() {
        let db = db();
        let repo = EmbeddingRepository::new(&db);
        let note = Id::new();

        // Values chosen to catch a lossy conversion: a negative, a zero, an exact binary
        // fraction, and two that are not representable exactly in binary.
        let vector = vec![-1.5, 0.1, 0.7, 0.0, 42.0];
        repo.replace_for_entity("note", note, "m", vec![chunk(note, 0, vector.clone(), "m")])
            .unwrap();

        let stored = repo.all_for_model("m").unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].vector, vector);
    }

    /// The bug this prevents: an edited note that got shorter leaves its old chunks behind,
    /// still matching queries and citing text the note no longer contains.
    #[test]
    fn re_embedding_replaces_rather_than_accumulates() {
        let db = db();
        let repo = EmbeddingRepository::new(&db);
        let note = Id::new();

        repo.replace_for_entity(
            "note",
            note,
            "m",
            vec![
                chunk(note, 0, vec![1.0], "m"),
                chunk(note, 1, vec![2.0], "m"),
                chunk(note, 2, vec![3.0], "m"),
            ],
        )
        .unwrap();
        assert_eq!(repo.count("m").unwrap(), 3);

        repo.replace_for_entity("note", note, "m", vec![chunk(note, 0, vec![9.0], "m")])
            .unwrap();

        let stored = repo.all_for_model("m").unwrap();
        assert_eq!(stored.len(), 1, "the two stale chunks should be gone");
        assert_eq!(stored[0].vector, vec![9.0]);
    }

    /// Re-embedding with one model must not destroy another model's vectors — they are
    /// separate indexes that happen to share a table.
    #[test]
    fn replacing_one_model_leaves_another_alone() {
        let db = db();
        let repo = EmbeddingRepository::new(&db);
        let note = Id::new();

        repo.replace_for_entity("note", note, "a", vec![chunk(note, 0, vec![1.0], "a")])
            .unwrap();
        repo.replace_for_entity("note", note, "b", vec![chunk(note, 0, vec![2.0], "b")])
            .unwrap();

        repo.replace_for_entity("note", note, "a", vec![chunk(note, 0, vec![3.0], "a")])
            .unwrap();

        assert_eq!(repo.count("a").unwrap(), 1);
        assert_eq!(repo.count("b").unwrap(), 1);
        assert_eq!(repo.all_for_model("b").unwrap()[0].vector, vec![2.0]);
    }

    #[test]
    fn reads_are_scoped_to_one_model() {
        let db = db();
        let repo = EmbeddingRepository::new(&db);
        let note = Id::new();

        repo.replace_for_entity("note", note, "a", vec![chunk(note, 0, vec![1.0], "a")])
            .unwrap();
        repo.replace_for_entity("note", note, "b", vec![chunk(note, 0, vec![2.0], "b")])
            .unwrap();

        assert_eq!(repo.all_for_model("a").unwrap().len(), 1);
        assert_eq!(repo.count_from_other_models("a").unwrap(), 1);
    }

    #[test]
    fn chunks_come_back_in_reading_order() {
        let db = db();
        let repo = EmbeddingRepository::new(&db);
        let note = Id::new();

        // Inserted out of order on purpose.
        repo.replace_for_entity(
            "note",
            note,
            "m",
            vec![
                chunk(note, 2, vec![3.0], "m"),
                chunk(note, 0, vec![1.0], "m"),
                chunk(note, 1, vec![2.0], "m"),
            ],
        )
        .unwrap();

        let indices: Vec<_> = repo
            .all_for_model("m")
            .unwrap()
            .into_iter()
            .map(|e| e.chunk_index)
            .collect();
        assert_eq!(indices, vec![0, 1, 2]);
    }

    #[test]
    fn indexed_entities_reports_the_newest_source_timestamp() {
        let db = db();
        let repo = EmbeddingRepository::new(&db);
        let note = Id::new();
        let newer = Utc::now();
        let older = newer - chrono::Duration::hours(1);

        repo.replace_for_entity(
            "note",
            note,
            "m",
            vec![
                NewEmbedding {
                    source_updated_at: older,
                    ..chunk(note, 0, vec![1.0], "m")
                },
                NewEmbedding {
                    source_updated_at: newer,
                    ..chunk(note, 1, vec![2.0], "m")
                },
            ],
        )
        .unwrap();

        let indexed = repo.indexed_entities("m").unwrap();
        assert_eq!(indexed.len(), 1);
        assert_eq!(
            indexed[0].source_updated_at.timestamp(),
            newer.timestamp(),
            "staleness must compare against the newest chunk"
        );
    }

    #[test]
    fn deleting_an_entity_removes_every_model_s_vectors() {
        let db = db();
        let repo = EmbeddingRepository::new(&db);
        let note = Id::new();
        let other = Id::new();

        repo.replace_for_entity("note", note, "a", vec![chunk(note, 0, vec![1.0], "a")])
            .unwrap();
        repo.replace_for_entity("note", note, "b", vec![chunk(note, 0, vec![1.0], "b")])
            .unwrap();
        repo.replace_for_entity("note", other, "a", vec![chunk(other, 0, vec![1.0], "a")])
            .unwrap();

        assert_eq!(repo.delete_for_entity("note", note).unwrap(), 2);
        assert_eq!(repo.count("a").unwrap(), 1, "the other note survives");
        assert_eq!(repo.count("b").unwrap(), 0);
    }

    #[test]
    fn clearing_drops_everything() {
        let db = db();
        let repo = EmbeddingRepository::new(&db);
        let note = Id::new();
        repo.replace_for_entity("note", note, "m", vec![chunk(note, 0, vec![1.0], "m")])
            .unwrap();

        assert_eq!(repo.clear().unwrap(), 1);
        assert!(repo.all_for_model("m").unwrap().is_empty());
    }

    /// A blob that disagrees with its own `dims` is corrupt. Returning a truncated vector
    /// would score zero against everything — a wrong answer disguised as a weak match.
    #[test]
    fn a_corrupt_vector_is_reported_not_silently_truncated() {
        assert!(from_blob(&[0, 0, 0], 1).is_err(), "not a whole f32");
        assert!(from_blob(&[0, 0, 0, 0], 5).is_err(), "dims disagree");
        assert_eq!(from_blob(&[0, 0, 128, 63], 1).unwrap(), vec![1.0f32]);
    }

    #[test]
    fn a_corrupt_row_surfaces_as_an_error_from_the_query() {
        let db = db();
        let repo = EmbeddingRepository::new(&db);
        let note = Id::new();
        repo.replace_for_entity("note", note, "m", vec![chunk(note, 0, vec![1.0, 2.0], "m")])
            .unwrap();

        // Corrupt the stored width without touching the blob.
        db.conn()
            .execute("UPDATE embeddings SET dims = 99", [])
            .unwrap();

        assert!(
            repo.all_for_model("m").is_err(),
            "a corrupt row must not read back as a short vector"
        );
    }

    #[test]
    fn an_entity_with_no_chunks_still_clears_its_old_ones() {
        let db = db();
        let repo = EmbeddingRepository::new(&db);
        let note = Id::new();
        repo.replace_for_entity("note", note, "m", vec![chunk(note, 0, vec![1.0], "m")])
            .unwrap();

        // An entity emptied of text produces no chunks; its vectors must still go.
        assert_eq!(
            repo.replace_for_entity("note", note, "m", Vec::new())
                .unwrap(),
            0
        );
        assert_eq!(repo.count("m").unwrap(), 0);
    }
}
