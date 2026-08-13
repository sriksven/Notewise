//! Breadth-first traversal over the object graph.

use std::collections::{HashSet, VecDeque};

use notewise_storage::Id;
use serde::{Deserialize, Serialize};

use crate::kinds::EdgeKind;
use crate::node::NodeRef;
use crate::{Graph, GraphError, Result};

/// One direct link from a queried node to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub edge_id: Id,
    /// The node at the other end.
    pub node: NodeRef,
    pub kind: EdgeKind,
    /// Whether the stored edge points away from the node that was queried.
    pub outbound: bool,
}

/// A node found during traversal, with how it was reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelatedNode {
    pub node: NodeRef,
    /// Hops from the starting node. Direct connections are `1`.
    pub distance: u32,
    /// The edge kind on the final hop.
    pub via: EdgeKind,
    /// The node this was reached from.
    pub from: NodeRef,
}

/// Walk outward from `start`, nearest first, up to `depth` hops.
///
/// Each node is reported once, by its shortest path — breadth-first order guarantees the
/// first time a node is dequeued is via a shortest route, so cycles terminate and a node
/// reachable by several paths does not appear repeatedly.
pub(crate) fn breadth_first(
    graph: &Graph<'_>,
    start: NodeRef,
    depth: u32,
) -> Result<Vec<RelatedNode>> {
    if depth > Graph::MAX_DEPTH {
        return Err(GraphError::DepthTooLarge(depth));
    }
    if depth == 0 {
        return Ok(Vec::new());
    }

    let mut visited: HashSet<NodeRef> = HashSet::new();
    visited.insert(start);

    let mut queue: VecDeque<(NodeRef, u32)> = VecDeque::new();
    queue.push_back((start, 0));

    let mut found = Vec::new();

    while let Some((current, distance)) = queue.pop_front() {
        if distance >= depth {
            continue;
        }

        for connection in graph.connections(current)? {
            if !visited.insert(connection.node) {
                continue;
            }

            found.push(RelatedNode {
                node: connection.node,
                distance: distance + 1,
                via: connection.kind,
                from: current,
            });
            queue.push_back((connection.node, distance + 1));
        }
    }

    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::NodeKind;
    use notewise_storage::Database;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn n(kind: NodeKind) -> NodeRef {
        NodeRef::new(kind, Id::new())
    }

    /// meeting → summary → action_item → ticket, plus a note referencing the meeting.
    struct Chain {
        meeting: NodeRef,
        summary: NodeRef,
        action_item: NodeRef,
        ticket: NodeRef,
        note: NodeRef,
    }

    fn chain(graph: &Graph<'_>) -> Chain {
        let c = Chain {
            meeting: n(NodeKind::Meeting),
            summary: n(NodeKind::Summary),
            action_item: n(NodeKind::ActionItem),
            ticket: n(NodeKind::Ticket),
            note: n(NodeKind::Note),
        };
        graph
            .connect(c.summary, EdgeKind::DerivedFrom, c.meeting)
            .unwrap();
        graph
            .connect(c.action_item, EdgeKind::DerivedFrom, c.summary)
            .unwrap();
        graph
            .connect(c.action_item, EdgeKind::BecameTicket, c.ticket)
            .unwrap();
        graph
            .connect(c.note, EdgeKind::References, c.meeting)
            .unwrap();
        c
    }

    #[test]
    fn depth_one_finds_only_direct_connections() {
        let db = db();
        let graph = Graph::new(&db);
        let c = chain(&graph);

        let related = graph.related(c.meeting, 1).unwrap();
        let nodes: HashSet<_> = related.iter().map(|r| r.node).collect();

        assert_eq!(nodes, HashSet::from([c.summary, c.note]));
        assert!(related.iter().all(|r| r.distance == 1));
    }

    #[test]
    fn traversal_reaches_across_multiple_hops() {
        let db = db();
        let graph = Graph::new(&db);
        let c = chain(&graph);

        let related = graph.related(c.meeting, 3).unwrap();
        let nodes: HashSet<_> = related.iter().map(|r| r.node).collect();

        assert_eq!(
            nodes,
            HashSet::from([c.summary, c.note, c.action_item, c.ticket]),
            "a ticket three hops away should be reachable from the meeting"
        );
    }

    #[test]
    fn results_are_ordered_nearest_first() {
        let db = db();
        let graph = Graph::new(&db);
        let c = chain(&graph);

        let distances: Vec<_> = graph
            .related(c.meeting, 3)
            .unwrap()
            .iter()
            .map(|r| r.distance)
            .collect();

        let mut sorted = distances.clone();
        sorted.sort_unstable();
        assert_eq!(
            distances, sorted,
            "breadth-first order should be nearest first"
        );
    }

    #[test]
    fn distance_reflects_the_shortest_path() {
        let db = db();
        let graph = Graph::new(&db);
        let c = chain(&graph);

        let related = graph.related(c.meeting, 4).unwrap();
        let distance = |node: NodeRef| related.iter().find(|r| r.node == node).unwrap().distance;

        assert_eq!(distance(c.summary), 1);
        assert_eq!(distance(c.note), 1);
        assert_eq!(distance(c.action_item), 2);
        assert_eq!(distance(c.ticket), 3);
    }

    #[test]
    fn the_starting_node_is_never_included() {
        let db = db();
        let graph = Graph::new(&db);
        let c = chain(&graph);

        let related = graph.related(c.meeting, 5).unwrap();
        assert!(related.iter().all(|r| r.node != c.meeting));
    }

    #[test]
    fn depth_zero_returns_nothing() {
        let db = db();
        let graph = Graph::new(&db);
        let c = chain(&graph);
        assert!(graph.related(c.meeting, 0).unwrap().is_empty());
    }

    #[test]
    fn cycles_terminate() {
        let db = db();
        let graph = Graph::new(&db);
        let (a, b, c) = (n(NodeKind::Note), n(NodeKind::Note), n(NodeKind::Note));

        graph.connect(a, EdgeKind::References, b).unwrap();
        graph.connect(b, EdgeKind::References, c).unwrap();
        graph.connect(c, EdgeKind::References, a).unwrap();

        let related = graph.related(a, Graph::MAX_DEPTH).unwrap();
        assert_eq!(
            related.len(),
            2,
            "each node reported once despite the cycle"
        );
    }

    #[test]
    fn a_node_reachable_by_two_paths_is_reported_once() {
        let db = db();
        let graph = Graph::new(&db);
        let (start, left, right, target) = (
            n(NodeKind::Meeting),
            n(NodeKind::Note),
            n(NodeKind::Note),
            n(NodeKind::Ticket),
        );

        graph.connect(start, EdgeKind::References, left).unwrap();
        graph.connect(start, EdgeKind::References, right).unwrap();
        graph.connect(left, EdgeKind::References, target).unwrap();
        graph.connect(right, EdgeKind::References, target).unwrap();

        let related = graph.related(start, 3).unwrap();
        let hits = related.iter().filter(|r| r.node == target).count();
        assert_eq!(hits, 1, "diamond paths must not duplicate the target");
    }

    #[test]
    fn traversal_ignores_edge_direction() {
        let db = db();
        let graph = Graph::new(&db);
        let c = chain(&graph);

        // The summary points *at* the meeting, yet is reachable *from* it.
        assert!(graph
            .related(c.meeting, 1)
            .unwrap()
            .iter()
            .any(|r| r.node == c.summary));
        // And the meeting is reachable from the summary.
        assert!(graph
            .related(c.summary, 1)
            .unwrap()
            .iter()
            .any(|r| r.node == c.meeting));
    }

    #[test]
    fn via_records_the_edge_kind_of_the_final_hop() {
        let db = db();
        let graph = Graph::new(&db);
        let c = chain(&graph);

        let related = graph.related(c.meeting, 3).unwrap();
        let ticket_hop = related.iter().find(|r| r.node == c.ticket).unwrap();

        assert_eq!(ticket_hop.via, EdgeKind::BecameTicket);
        assert_eq!(ticket_hop.from, c.action_item);
    }

    #[test]
    fn depth_beyond_the_maximum_is_rejected() {
        let db = db();
        let graph = Graph::new(&db);
        let err = graph
            .related(n(NodeKind::Meeting), Graph::MAX_DEPTH + 1)
            .expect_err("should refuse an unbounded traversal");
        assert!(matches!(err, GraphError::DepthTooLarge(_)), "got {err:?}");
    }

    #[test]
    fn an_isolated_node_has_no_relations() {
        let db = db();
        let graph = Graph::new(&db);
        chain(&graph);
        assert!(graph.related(n(NodeKind::Meeting), 3).unwrap().is_empty());
    }

    #[test]
    fn depth_limits_how_far_traversal_reaches() {
        let db = db();
        let graph = Graph::new(&db);
        let c = chain(&graph);

        let two_hops: HashSet<_> = graph
            .related(c.meeting, 2)
            .unwrap()
            .iter()
            .map(|r| r.node)
            .collect();

        assert!(two_hops.contains(&c.action_item));
        assert!(
            !two_hops.contains(&c.ticket),
            "the ticket is three hops away and must be excluded at depth 2"
        );
    }
}
