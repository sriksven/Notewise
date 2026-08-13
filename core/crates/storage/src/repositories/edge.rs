//! Raw edge persistence.
//!
//! This is deliberately untyped: kinds are plain strings here, and the `graph` crate layers
//! typed [`NodeKind`]/[`EdgeKind`] enums plus traversal on top. Keeping SQL-level kinds
//! untyped means adding a new entity type needs no schema migration.
//!
//! [`NodeKind`]: https://docs.rs/notewise-graph
//! [`EdgeKind`]: https://docs.rs/notewise-graph

use chrono::{DateTime, Utc};
use rusqlite::Row;

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::id::Id;

/// A stored association between two entities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeRecord {
    pub id: Id,
    pub from_kind: String,
    pub from_id: Id,
    pub edge_kind: String,
    pub to_kind: String,
    pub to_id: Id,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEdge {
    pub from_kind: String,
    pub from_id: Id,
    pub edge_kind: String,
    pub to_kind: String,
    pub to_id: Id,
}

#[derive(Debug)]
pub struct EdgeRepository<'a> {
    db: &'a Database,
}

impl<'a> EdgeRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Create an edge, or return the existing one if identical.
    ///
    /// Idempotent because edge creation is frequently derived from re-processing the same
    /// meeting — re-summarizing should not multiply edges.
    pub fn create(&self, new: NewEdge) -> Result<EdgeRecord> {
        if let Some(existing) = self.find(&new)? {
            return Ok(existing);
        }

        let edge = EdgeRecord {
            id: Id::new(),
            from_kind: new.from_kind,
            from_id: new.from_id,
            edge_kind: new.edge_kind,
            to_kind: new.to_kind,
            to_id: new.to_id,
            created_at: Utc::now(),
        };

        self.db.conn().execute(
            "INSERT INTO edges
                (id, from_kind, from_id, edge_kind, to_kind, to_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                edge.id,
                edge.from_kind,
                edge.from_id,
                edge.edge_kind,
                edge.to_kind,
                edge.to_id,
                edge.created_at
            ],
        )?;

        Ok(edge)
    }

    fn find(&self, edge: &NewEdge) -> Result<Option<EdgeRecord>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, from_kind, from_id, edge_kind, to_kind, to_id, created_at
             FROM edges
             WHERE from_kind = ?1 AND from_id = ?2 AND edge_kind = ?3
               AND to_kind = ?4 AND to_id = ?5",
        )?;
        let mut rows = stmt.query_map(
            rusqlite::params![
                edge.from_kind,
                edge.from_id,
                edge.edge_kind,
                edge.to_kind,
                edge.to_id
            ],
            map_edge,
        )?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Edges pointing away from a node.
    pub fn outgoing(&self, kind: &str, id: Id) -> Result<Vec<EdgeRecord>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, from_kind, from_id, edge_kind, to_kind, to_id, created_at
             FROM edges WHERE from_kind = ?1 AND from_id = ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![kind, id], map_edge)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Edges pointing at a node.
    pub fn incoming(&self, kind: &str, id: Id) -> Result<Vec<EdgeRecord>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, from_kind, from_id, edge_kind, to_kind, to_id, created_at
             FROM edges WHERE to_kind = ?1 AND to_id = ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![kind, id], map_edge)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every edge touching a node, in either direction.
    ///
    /// Traversal is undirected on purpose: "everything related to this meeting" should find
    /// a note that references the meeting just as readily as a decision the meeting produced.
    pub fn touching(&self, kind: &str, id: Id) -> Result<Vec<EdgeRecord>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, from_kind, from_id, edge_kind, to_kind, to_id, created_at
             FROM edges
             WHERE (from_kind = ?1 AND from_id = ?2) OR (to_kind = ?1 AND to_id = ?2)",
        )?;
        let rows = stmt.query_map(rusqlite::params![kind, id], map_edge)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn delete(&self, id: Id) -> Result<()> {
        let changed = self
            .db
            .conn()
            .execute("DELETE FROM edges WHERE id = ?1", rusqlite::params![id])?;
        if changed == 0 {
            return Err(StorageError::not_found("Edge", id));
        }
        Ok(())
    }

    /// Remove every edge touching a node.
    ///
    /// Entity tables cascade on delete, but the edge table cannot — it has no foreign keys,
    /// precisely because it references heterogeneous kinds. Callers deleting an entity must
    /// call this, which is why `graph` wraps both in one operation.
    pub fn delete_touching(&self, kind: &str, id: Id) -> Result<usize> {
        let removed = self.db.conn().execute(
            "DELETE FROM edges
             WHERE (from_kind = ?1 AND from_id = ?2) OR (to_kind = ?1 AND to_id = ?2)",
            rusqlite::params![kind, id],
        )?;
        Ok(removed)
    }

    pub fn count(&self) -> Result<u64> {
        let count = self
            .db
            .conn()
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
        Ok(count)
    }
}

fn map_edge(row: &Row<'_>) -> rusqlite::Result<EdgeRecord> {
    Ok(EdgeRecord {
        id: row.get(0)?,
        from_kind: row.get(1)?,
        from_id: row.get(2)?,
        edge_kind: row.get(3)?,
        to_kind: row.get(4)?,
        to_id: row.get(5)?,
        created_at: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn edge(from: Id, to: Id) -> NewEdge {
        NewEdge {
            from_kind: "note".into(),
            from_id: from,
            edge_kind: "references".into(),
            to_kind: "meeting".into(),
            to_id: to,
        }
    }

    #[test]
    fn creates_an_edge() {
        let db = db();
        let repo = EdgeRepository::new(&db);
        let (a, b) = (Id::new(), Id::new());

        let created = repo.create(edge(a, b)).unwrap();
        assert_eq!(created.from_id, a);
        assert_eq!(created.to_id, b);
        assert_eq!(repo.count().unwrap(), 1);
    }

    #[test]
    fn creating_the_same_edge_twice_is_idempotent() {
        let db = db();
        let repo = EdgeRepository::new(&db);
        let (a, b) = (Id::new(), Id::new());

        let first = repo.create(edge(a, b)).unwrap();
        let second = repo.create(edge(a, b)).unwrap();

        assert_eq!(first.id, second.id, "should return the existing edge");
        assert_eq!(
            repo.count().unwrap(),
            1,
            "re-processing must not multiply edges"
        );
    }

    #[test]
    fn direction_is_preserved() {
        let db = db();
        let repo = EdgeRepository::new(&db);
        let (note, meeting) = (Id::new(), Id::new());
        repo.create(edge(note, meeting)).unwrap();

        assert_eq!(repo.outgoing("note", note).unwrap().len(), 1);
        assert_eq!(repo.incoming("note", note).unwrap().len(), 0);
        assert_eq!(repo.incoming("meeting", meeting).unwrap().len(), 1);
        assert_eq!(repo.outgoing("meeting", meeting).unwrap().len(), 0);
    }

    #[test]
    fn touching_finds_edges_in_both_directions() {
        let db = db();
        let repo = EdgeRepository::new(&db);
        let subject = Id::new();

        repo.create(edge(subject, Id::new())).unwrap();
        repo.create(NewEdge {
            from_kind: "meeting".into(),
            from_id: Id::new(),
            edge_kind: "produced".into(),
            to_kind: "note".into(),
            to_id: subject,
        })
        .unwrap();

        assert_eq!(repo.touching("note", subject).unwrap().len(), 2);
    }

    #[test]
    fn kind_is_part_of_identity() {
        // The same uuid used as two different kinds must not collide.
        let db = db();
        let repo = EdgeRepository::new(&db);
        let shared = Id::new();

        repo.create(NewEdge {
            from_kind: "note".into(),
            from_id: shared,
            edge_kind: "references".into(),
            to_kind: "meeting".into(),
            to_id: Id::new(),
        })
        .unwrap();

        assert_eq!(repo.outgoing("note", shared).unwrap().len(), 1);
        assert_eq!(repo.outgoing("ticket", shared).unwrap().len(), 0);
    }

    #[test]
    fn delete_touching_removes_every_connected_edge() {
        let db = db();
        let repo = EdgeRepository::new(&db);
        let subject = Id::new();

        repo.create(edge(subject, Id::new())).unwrap();
        repo.create(edge(subject, Id::new())).unwrap();
        repo.create(NewEdge {
            from_kind: "meeting".into(),
            from_id: Id::new(),
            edge_kind: "produced".into(),
            to_kind: "note".into(),
            to_id: subject,
        })
        .unwrap();
        let unrelated = repo.create(edge(Id::new(), Id::new())).unwrap();

        let removed = repo.delete_touching("note", subject).unwrap();
        assert_eq!(removed, 3);
        assert_eq!(repo.count().unwrap(), 1);
        assert_eq!(repo.outgoing("note", unrelated.from_id).unwrap().len(), 1);
    }

    #[test]
    fn deleting_a_missing_edge_reports_not_found() {
        let db = db();
        let err = EdgeRepository::new(&db)
            .delete(Id::new())
            .expect_err("should be missing");
        assert!(matches!(err, StorageError::NotFound { kind: "Edge", .. }));
    }

    #[test]
    fn distinct_edge_kinds_between_the_same_pair_coexist() {
        let db = db();
        let repo = EdgeRepository::new(&db);
        let (a, b) = (Id::new(), Id::new());

        repo.create(edge(a, b)).unwrap();
        repo.create(NewEdge {
            from_kind: "note".into(),
            from_id: a,
            edge_kind: "supersedes".into(),
            to_kind: "meeting".into(),
            to_id: b,
        })
        .unwrap();

        assert_eq!(repo.count().unwrap(), 2);
    }
}
