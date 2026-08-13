use std::fmt;

use notewise_storage::Id;
use serde::{Deserialize, Serialize};

use crate::kinds::NodeKind;

/// A reference to an entity in the graph.
///
/// Identity is the `(kind, id)` pair, not the id alone. Ids are uuids so a collision across
/// kinds is vanishingly unlikely, but treating kind as part of identity keeps traversal
/// honest and makes edge rows self-describing without a join.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeRef {
    pub kind: NodeKind,
    pub id: Id,
}

impl NodeRef {
    pub fn new(kind: NodeKind, id: Id) -> Self {
        Self { kind, id }
    }
}

impl fmt::Display for NodeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind, self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_includes_kind() {
        let id = Id::new();
        assert_ne!(
            NodeRef::new(NodeKind::Note, id),
            NodeRef::new(NodeKind::Ticket, id)
        );
    }

    #[test]
    fn equal_when_kind_and_id_match() {
        let id = Id::new();
        assert_eq!(
            NodeRef::new(NodeKind::Note, id),
            NodeRef::new(NodeKind::Note, id)
        );
    }

    #[test]
    fn displays_as_kind_colon_id() {
        let id = Id::new();
        let node = NodeRef::new(NodeKind::Meeting, id);
        assert_eq!(node.to_string(), format!("meeting:{id}"));
    }

    #[test]
    fn usable_as_a_hash_key() {
        let mut set = std::collections::HashSet::new();
        let id = Id::new();
        assert!(set.insert(NodeRef::new(NodeKind::Note, id)));
        assert!(!set.insert(NodeRef::new(NodeKind::Note, id)));
        assert!(set.insert(NodeRef::new(NodeKind::Ticket, id)));
    }
}
