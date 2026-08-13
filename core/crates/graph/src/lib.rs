//! The Notewise object graph.
//!
//! Meetings, notes, decisions, tickets, and emails are connected by typed edges. This is
//! what makes "everything related to this meeting" a single traversal instead of a
//! hand-written join per relationship.
//!
//! # Ownership vs association
//!
//! `notewise-storage` models **ownership** with foreign keys — a transcript segment belongs
//! to exactly one meeting. This crate models **association**: many-to-many, heterogeneous,
//! and traversable to arbitrary depth. Encoding association as foreign keys would mean a
//! schema migration every time the product grows a new kind of link.
//!
//! # Example
//!
//! ```
//! use notewise_graph::{EdgeKind, Graph, NodeKind, NodeRef};
//! use notewise_storage::{Database, Id};
//!
//! let db = Database::open_in_memory()?;
//! let graph = Graph::new(&db);
//!
//! let meeting = NodeRef::new(NodeKind::Meeting, Id::new());
//! let note = NodeRef::new(NodeKind::Note, Id::new());
//!
//! graph.connect(note, EdgeKind::References, meeting)?;
//!
//! let related = graph.related(meeting, 1)?;
//! assert_eq!(related.len(), 1);
//! assert_eq!(related[0].node, note);
//! # Ok::<(), notewise_graph::GraphError>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod kinds;
mod node;
mod traverse;

pub use kinds::{EdgeKind, NodeKind};
pub use node::NodeRef;
pub use traverse::{Connection, RelatedNode};

use notewise_storage::{Database, EdgeRepository, Id, NewEdge, StorageError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GraphError {
    #[error(transparent)]
    Storage(#[from] StorageError),

    #[error("stored edge has an unrecognized node kind '{0}'")]
    UnknownNodeKind(String),

    #[error("stored edge has an unrecognized edge kind '{0}'")]
    UnknownEdgeKind(String),

    #[error("traversal depth {0} exceeds the maximum of {max}", max = Graph::MAX_DEPTH)]
    DepthTooLarge(u32),
}

pub type Result<T> = std::result::Result<T, GraphError>;

/// The object graph over a [`Database`].
#[derive(Debug)]
pub struct Graph<'a> {
    db: &'a Database,
}

impl<'a> Graph<'a> {
    /// Traversal depth ceiling.
    ///
    /// Each level fans out across every edge of every node found so far, so cost grows
    /// quickly. Depths beyond this are almost always a caller bug rather than an intent.
    pub const MAX_DEPTH: u32 = 6;

    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    fn edges(&self) -> EdgeRepository<'a> {
        EdgeRepository::new(self.db)
    }

    /// Link two nodes. Idempotent — connecting the same pair twice is a no-op.
    pub fn connect(&self, from: NodeRef, kind: EdgeKind, to: NodeRef) -> Result<()> {
        self.edges().create(NewEdge {
            from_kind: from.kind.as_str().to_string(),
            from_id: from.id,
            edge_kind: kind.as_str().to_string(),
            to_kind: to.kind.as_str().to_string(),
            to_id: to.id,
        })?;
        Ok(())
    }

    /// Remove a specific link. Returns whether an edge was actually removed.
    pub fn disconnect(&self, from: NodeRef, kind: EdgeKind, to: NodeRef) -> Result<bool> {
        let matching = self
            .edges()
            .outgoing(from.kind.as_str(), from.id)?
            .into_iter()
            .find(|e| {
                e.edge_kind == kind.as_str() && e.to_kind == to.kind.as_str() && e.to_id == to.id
            });

        match matching {
            Some(edge) => {
                self.edges().delete(edge.id)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Every direct connection to a node, in both directions.
    pub fn connections(&self, node: NodeRef) -> Result<Vec<Connection>> {
        let records = self.edges().touching(node.kind.as_str(), node.id)?;
        let mut out = Vec::with_capacity(records.len());

        for record in records {
            let edge_kind = EdgeKind::parse(&record.edge_kind)
                .ok_or_else(|| GraphError::UnknownEdgeKind(record.edge_kind.clone()))?;

            // The far end is whichever side is not the node we started from.
            let from = NodeRef::new(parse_kind(&record.from_kind)?, record.from_id);
            let to = NodeRef::new(parse_kind(&record.to_kind)?, record.to_id);
            let (other, outbound) = if from == node {
                (to, true)
            } else {
                (from, false)
            };

            out.push(Connection {
                edge_id: record.id,
                node: other,
                kind: edge_kind,
                outbound,
            });
        }

        Ok(out)
    }

    /// Everything reachable from a node within `depth` hops, nearest first.
    ///
    /// Traversal is undirected: a note that references a meeting is just as related to that
    /// meeting as a summary the meeting produced. Direction is preserved on each
    /// [`Connection`] for display, but does not limit reachability.
    ///
    /// The starting node is not included in the results.
    pub fn related(&self, start: NodeRef, depth: u32) -> Result<Vec<RelatedNode>> {
        traverse::breadth_first(self, start, depth)
    }

    /// Related nodes filtered to a single kind — "which tickets came out of this meeting?"
    pub fn related_of_kind(
        &self,
        start: NodeRef,
        kind: NodeKind,
        depth: u32,
    ) -> Result<Vec<RelatedNode>> {
        Ok(self
            .related(start, depth)?
            .into_iter()
            .filter(|r| r.node.kind == kind)
            .collect())
    }

    /// Remove a node's edges.
    ///
    /// The edge table has no foreign keys — it references heterogeneous kinds, so SQLite
    /// cannot cascade for it. Deleting an entity without calling this leaves edges pointing
    /// at something that no longer exists.
    pub fn detach(&self, node: NodeRef) -> Result<usize> {
        Ok(self.edges().delete_touching(node.kind.as_str(), node.id)?)
    }

    /// Total number of edges. Mostly useful for diagnostics and tests.
    pub fn edge_count(&self) -> Result<u64> {
        Ok(self.edges().count()?)
    }
}

fn parse_kind(raw: &str) -> Result<NodeKind> {
    NodeKind::parse(raw).ok_or_else(|| GraphError::UnknownNodeKind(raw.to_string()))
}

/// Convenience constructor for a node reference.
pub fn node(kind: NodeKind, id: Id) -> NodeRef {
    NodeRef::new(kind, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn meeting() -> NodeRef {
        NodeRef::new(NodeKind::Meeting, Id::new())
    }

    fn note() -> NodeRef {
        NodeRef::new(NodeKind::Note, Id::new())
    }

    #[test]
    fn connect_then_read_back_a_connection() {
        let db = db();
        let graph = Graph::new(&db);
        let (m, n) = (meeting(), note());

        graph.connect(n, EdgeKind::References, m).unwrap();

        let connections = graph.connections(m).unwrap();
        assert_eq!(connections.len(), 1);
        assert_eq!(connections[0].node, n);
        assert_eq!(connections[0].kind, EdgeKind::References);
        assert!(
            !connections[0].outbound,
            "from the meeting's perspective this edge points inward"
        );
    }

    #[test]
    fn direction_is_reported_relative_to_the_queried_node() {
        let db = db();
        let graph = Graph::new(&db);
        let (m, n) = (meeting(), note());
        graph.connect(n, EdgeKind::References, m).unwrap();

        assert!(graph.connections(n).unwrap()[0].outbound);
        assert!(!graph.connections(m).unwrap()[0].outbound);
    }

    #[test]
    fn connecting_twice_is_idempotent() {
        let db = db();
        let graph = Graph::new(&db);
        let (m, n) = (meeting(), note());

        graph.connect(n, EdgeKind::References, m).unwrap();
        graph.connect(n, EdgeKind::References, m).unwrap();

        assert_eq!(graph.edge_count().unwrap(), 1);
    }

    #[test]
    fn disconnect_removes_only_the_matching_edge() {
        let db = db();
        let graph = Graph::new(&db);
        let (m, n) = (meeting(), note());

        graph.connect(n, EdgeKind::References, m).unwrap();
        graph.connect(n, EdgeKind::Mentions, m).unwrap();

        assert!(graph.disconnect(n, EdgeKind::References, m).unwrap());
        assert_eq!(graph.edge_count().unwrap(), 1);
        assert_eq!(graph.connections(m).unwrap()[0].kind, EdgeKind::Mentions);
    }

    #[test]
    fn disconnecting_a_missing_edge_reports_false() {
        let db = db();
        let graph = Graph::new(&db);
        assert!(!graph
            .disconnect(note(), EdgeKind::References, meeting())
            .unwrap());
    }

    #[test]
    fn detach_removes_every_edge_touching_a_node() {
        let db = db();
        let graph = Graph::new(&db);
        let m = meeting();

        graph.connect(note(), EdgeKind::References, m).unwrap();
        graph.connect(note(), EdgeKind::References, m).unwrap();
        graph
            .connect(
                m,
                EdgeKind::Contains,
                NodeRef::new(NodeKind::Summary, Id::new()),
            )
            .unwrap();
        let unrelated_a = note();
        let unrelated_b = meeting();
        graph
            .connect(unrelated_a, EdgeKind::References, unrelated_b)
            .unwrap();

        assert_eq!(graph.detach(m).unwrap(), 3);
        assert_eq!(graph.edge_count().unwrap(), 1);
    }

    #[test]
    fn the_same_uuid_under_two_kinds_is_two_different_nodes() {
        let db = db();
        let graph = Graph::new(&db);
        let shared = Id::new();
        let as_note = NodeRef::new(NodeKind::Note, shared);
        let as_ticket = NodeRef::new(NodeKind::Ticket, shared);

        graph
            .connect(as_note, EdgeKind::References, meeting())
            .unwrap();

        assert_eq!(graph.connections(as_note).unwrap().len(), 1);
        assert_eq!(
            graph.connections(as_ticket).unwrap().len(),
            0,
            "kind is part of node identity"
        );
    }

    #[test]
    fn related_of_kind_filters_the_traversal() {
        let db = db();
        let graph = Graph::new(&db);
        let m = meeting();
        let summary = NodeRef::new(NodeKind::Summary, Id::new());
        let ticket = NodeRef::new(NodeKind::Ticket, Id::new());

        graph.connect(summary, EdgeKind::DerivedFrom, m).unwrap();
        graph.connect(note(), EdgeKind::References, m).unwrap();
        graph.connect(ticket, EdgeKind::References, m).unwrap();

        let tickets = graph.related_of_kind(m, NodeKind::Ticket, 1).unwrap();
        assert_eq!(tickets.len(), 1);
        assert_eq!(tickets[0].node, ticket);
    }

    #[test]
    fn unknown_stored_edge_kind_is_reported_not_panicked() {
        let db = db();
        let graph = Graph::new(&db);
        let m = meeting();
        graph.connect(note(), EdgeKind::References, m).unwrap();

        // Simulate a database written by a newer build that knows more edge kinds.
        EdgeRepository::new(&db)
            .create(NewEdge {
                from_kind: "note".into(),
                from_id: Id::new(),
                edge_kind: "teleports_to".into(),
                to_kind: "meeting".into(),
                to_id: m.id,
            })
            .unwrap();

        let err = graph
            .connections(m)
            .expect_err("should reject unknown kind");
        assert!(matches!(err, GraphError::UnknownEdgeKind(_)), "got {err:?}");
    }
}
