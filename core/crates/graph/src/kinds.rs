use std::fmt;

use serde::{Deserialize, Serialize};

/// The kind of entity a graph node points at.
///
/// Stored as a string in the edge table, so adding a variant needs no schema migration.
/// The string forms are part of the on-disk format — changing one is a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Workspace,
    Project,
    Meeting,
    TranscriptSegment,
    Summary,
    Decision,
    ActionItem,
    Note,
    Ticket,
    EmailDraft,
    Notification,
    /// An artifact that lives in another system — a Linear issue, a calendar event, a file
    /// in a user's vault. Notewise records it but does not own it.
    ExternalItem,
}

impl NodeKind {
    pub const ALL: &'static [NodeKind] = &[
        NodeKind::Workspace,
        NodeKind::Project,
        NodeKind::Meeting,
        NodeKind::TranscriptSegment,
        NodeKind::Summary,
        NodeKind::Decision,
        NodeKind::ActionItem,
        NodeKind::Note,
        NodeKind::Ticket,
        NodeKind::EmailDraft,
        NodeKind::Notification,
        NodeKind::ExternalItem,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::Workspace => "workspace",
            NodeKind::Project => "project",
            NodeKind::Meeting => "meeting",
            NodeKind::TranscriptSegment => "transcript_segment",
            NodeKind::Summary => "summary",
            NodeKind::Decision => "decision",
            NodeKind::ActionItem => "action_item",
            NodeKind::Note => "note",
            NodeKind::Ticket => "ticket",
            NodeKind::EmailDraft => "email_draft",
            NodeKind::Notification => "notification",
            NodeKind::ExternalItem => "external_item",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        NodeKind::ALL.iter().copied().find(|k| k.as_str() == s)
    }
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How two nodes relate.
///
/// Edges are stored directionally but traversed in both directions by default — see
/// [`Graph::related`](crate::Graph::related). Direction carries meaning for display
/// ("this note *references* that meeting") rather than for reachability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Output produced by processing the target, e.g. summary → meeting.
    DerivedFrom,
    /// A soft, user-authored link, e.g. note → meeting.
    References,
    /// Containment, e.g. project → meeting.
    Contains,
    /// An action item promoted into a ticket.
    BecameTicket,
    /// A person or entity named inside the source.
    Mentions,
    /// This node replaces an earlier one, e.g. a regenerated summary.
    Supersedes,
    /// A draft generated from source material.
    GeneratedFrom,
    /// A notification's trigger.
    NotifiesAbout,
    /// This node is mirrored in an external system, e.g. action item → Linear issue.
    SyncedTo,
}

impl EdgeKind {
    pub const ALL: &'static [EdgeKind] = &[
        EdgeKind::DerivedFrom,
        EdgeKind::References,
        EdgeKind::Contains,
        EdgeKind::BecameTicket,
        EdgeKind::Mentions,
        EdgeKind::Supersedes,
        EdgeKind::GeneratedFrom,
        EdgeKind::NotifiesAbout,
        EdgeKind::SyncedTo,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeKind::DerivedFrom => "derived_from",
            EdgeKind::References => "references",
            EdgeKind::Contains => "contains",
            EdgeKind::BecameTicket => "became_ticket",
            EdgeKind::Mentions => "mentions",
            EdgeKind::Supersedes => "supersedes",
            EdgeKind::GeneratedFrom => "generated_from",
            EdgeKind::NotifiesAbout => "notifies_about",
            EdgeKind::SyncedTo => "synced_to",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        EdgeKind::ALL.iter().copied().find(|k| k.as_str() == s)
    }
}

impl fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_node_kind_round_trips() {
        for kind in NodeKind::ALL {
            assert_eq!(NodeKind::parse(kind.as_str()), Some(*kind));
        }
        assert_eq!(NodeKind::parse("wormhole"), None);
    }

    #[test]
    fn every_edge_kind_round_trips() {
        for kind in EdgeKind::ALL {
            assert_eq!(EdgeKind::parse(kind.as_str()), Some(*kind));
        }
        assert_eq!(EdgeKind::parse("entangles"), None);
    }

    #[test]
    fn node_kind_strings_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for kind in NodeKind::ALL {
            assert!(seen.insert(kind.as_str()), "duplicate string for {kind:?}");
        }
    }

    #[test]
    fn edge_kind_strings_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for kind in EdgeKind::ALL {
            assert!(seen.insert(kind.as_str()), "duplicate string for {kind:?}");
        }
    }

    #[test]
    fn all_lists_are_exhaustive() {
        // A missing entry in ALL silently breaks `parse`, so guard the count.
        assert_eq!(NodeKind::ALL.len(), 12);
        assert_eq!(EdgeKind::ALL.len(), 9);
    }

    #[test]
    fn external_items_are_reachable_kinds() {
        assert_eq!(
            NodeKind::parse("external_item"),
            Some(NodeKind::ExternalItem)
        );
        assert_eq!(EdgeKind::parse("synced_to"), Some(EdgeKind::SyncedTo));
    }
}
