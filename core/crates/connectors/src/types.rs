//! Types crossing the connector boundary.

use chrono::{DateTime, Utc};
use notewise_storage::Id;
use serde::{Deserialize, Serialize};

/// What a push is asking the remote system to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Create,
    Update,
    Delete,
}

impl Operation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Operation::Create => "create",
            Operation::Update => "update",
            Operation::Delete => "delete",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "create" => Some(Operation::Create),
            "update" => Some(Operation::Update),
            "delete" => Some(Operation::Delete),
            _ => None,
        }
    }
}

/// A unit of work handed to a [`SinkConnector`](crate::SinkConnector).
#[derive(Debug, Clone, PartialEq)]
pub struct Outbound {
    pub node_kind: String,
    pub node_id: Id,
    pub operation: Operation,
    pub payload: serde_json::Value,
    /// Set when this node has been pushed before, so the sink updates rather than creates.
    pub existing: Option<ExternalRef>,
}

/// Where a pushed artifact ended up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRef {
    pub external_id: String,
    pub url: Option<String>,
    pub title: Option<String>,
    pub remote_version: Option<String>,
}

impl ExternalRef {
    pub fn new(external_id: impl Into<String>) -> Self {
        Self {
            external_id: external_id.into(),
            url: None,
            title: None,
            remote_version: None,
        }
    }
}

/// One artifact read from a remote system.
#[derive(Debug, Clone, PartialEq)]
pub struct Inbound {
    pub external_id: String,
    pub url: Option<String>,
    pub title: Option<String>,
    pub remote_version: Option<String>,
    pub occurred_at: Option<DateTime<Utc>>,
    pub payload: serde_json::Value,
}

/// An opaque per-connector position in a remote change feed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cursor(pub Option<String>);

impl Cursor {
    pub fn start() -> Self {
        Cursor(None)
    }

    pub fn as_deref(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

/// One page of inbound items plus the position to resume from.
#[derive(Debug, Clone, PartialEq)]
pub struct PullBatch {
    pub items: Vec<Inbound>,
    pub next_cursor: Cursor,
}

/// Whether a connector can be used right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    Ok,
    /// Configured but the credential was rejected.
    NeedsAuth,
    /// Reachable check failed for a reason the user may be able to fix.
    Unavailable(String),
}
