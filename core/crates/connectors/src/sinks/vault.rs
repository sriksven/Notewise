//! Mirror meetings into a folder of markdown files.
//!
//! The connector that needs no account, no OAuth app, and no network. Markdown because it is
//! the format that survives: a user who stops using Notewise can still read their meetings,
//! and it drops straight into Obsidian or any editor.
//!
//! Content comes from `notewise_storage::meeting_to_markdown` via the enqueued payload, so
//! this file is a destination, not a second renderer.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::connector::{Connector, SinkConnector};
use crate::error::{ConnectorError, Result};
use crate::types::{ExternalRef, Health, Outbound};

#[derive(Debug)]
pub struct VaultSink {
    root: PathBuf,
}

impl VaultSink {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Reduce a title to something safe to use as a file name.
///
/// Path separators and `..` are stripped rather than escaped: a meeting titled
/// `../../etc/passwd` must land inside the vault, not outside it. The node id is appended so
/// two meetings with the same title do not overwrite each other.
fn file_name(title: &str, node_id: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();

    let trimmed = cleaned.trim_matches(['-', ' ']).trim();
    let stem = if trimmed.is_empty() {
        "untitled"
    } else {
        trimmed
    };
    // 12 hex characters, not 8. The id is a v4 UUID, so 8 characters is 32 bits — and a
    // collision here is silent, because one meeting's file simply overwrites another's and
    // `push` still reports success. At 2^32, a recurring "Standup" reaches a 1% chance of
    // collision by its 9,300th instance, which a daily meeting hits inside thirty years.
    // 12 characters is 48 bits and puts the same figure past any plausible vault.
    let short_id: String = node_id.chars().take(12).collect();

    format!("{stem}-{short_id}.md")
}

#[async_trait]
impl Connector for VaultSink {
    fn id(&self) -> &str {
        "vault"
    }

    fn display_name(&self) -> &str {
        "Markdown vault"
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn health(&self) -> Result<Health> {
        if self.root.is_dir() {
            Ok(Health::Ok)
        } else {
            Ok(Health::Unavailable(format!(
                "{} is not a directory",
                self.root.display()
            )))
        }
    }
}

#[async_trait]
impl SinkConnector for VaultSink {
    async fn push(&self, outbound: &Outbound) -> Result<ExternalRef> {
        let title = outbound
            .payload
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("untitled");

        let markdown = outbound
            .payload
            .get("markdown")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ConnectorError::Permanent("payload has no 'markdown' field".into()))?;

        let name = file_name(title, &outbound.node_id.to_string());
        let path = self.root.join(&name);

        // A missing vault folder is a configuration mistake, not a blip: the user moved or
        // never chose the directory. Retrying cannot fix it, so it must not be Transient.
        // `tokio::fs`, not `std::fs`. A vault is very often inside an iCloud, Dropbox, or
        // Google Drive folder, where a sync client can stall a write for seconds while it
        // holds a lock. A blocking write of that length on a runtime worker starves every
        // other task on that thread; `tokio::fs` moves it to the blocking pool.
        tokio::fs::write(&path, markdown).await.map_err(|e| {
            ConnectorError::Permanent(format!("cannot write {}: {e}", path.display()))
        })?;

        Ok(ExternalRef {
            external_id: name,
            url: Some(format!("file://{}", path.display())),
            title: Some(title.to_string()),
            remote_version: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Operation;
    use notewise_storage::Id;

    fn outbound(title: &str, body: &str) -> Outbound {
        Outbound {
            node_kind: "meeting".into(),
            node_id: Id::new(),
            operation: Operation::Create,
            payload: serde_json::json!({ "title": title, "markdown": body }),
            existing: None,
        }
    }

    #[tokio::test]
    async fn push_writes_a_markdown_file() {
        let dir = tempfile::tempdir().unwrap();
        let sink = VaultSink::new(dir.path());

        let reference = sink
            .push(&outbound("Standup", "# Standup\n\nShipped."))
            .await
            .unwrap();

        let written = std::fs::read_to_string(dir.path().join(&reference.external_id)).unwrap();
        assert!(written.contains("Shipped."));
    }

    #[tokio::test]
    async fn the_same_node_overwrites_rather_than_accumulating() {
        let dir = tempfile::tempdir().unwrap();
        let sink = VaultSink::new(dir.path());
        let mut first = outbound("Standup", "v1");
        first.operation = Operation::Update;
        let second = Outbound {
            payload: serde_json::json!({"title": "Standup", "markdown": "v2"}),
            ..first.clone()
        };

        sink.push(&first).await.unwrap();
        let reference = sink.push(&second).await.unwrap();

        let written = std::fs::read_to_string(dir.path().join(&reference.external_id)).unwrap();
        assert_eq!(written, "v2");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn titles_with_path_separators_cannot_escape_the_vault() {
        let dir = tempfile::tempdir().unwrap();
        let sink = VaultSink::new(dir.path());

        let reference = sink
            .push(&outbound("../../etc/passwd", "nope"))
            .await
            .unwrap();

        assert!(
            !reference.external_id.contains(".."),
            "got {}",
            reference.external_id
        );
        assert!(dir
            .path()
            .join(&reference.external_id)
            .starts_with(dir.path()));
    }

    #[tokio::test]
    async fn a_missing_vault_directory_is_a_configuration_error_not_a_retry() {
        let sink = VaultSink::new("/nonexistent/notewise-vault-test");
        let err = sink.push(&outbound("Standup", "x")).await.unwrap_err();
        assert!(
            !err.is_retryable(),
            "retrying will not create the user's folder"
        );
    }

    #[tokio::test]
    async fn health_reports_a_missing_directory() {
        let sink = VaultSink::new("/nonexistent/notewise-vault-test");
        assert!(matches!(
            sink.health().await.unwrap(),
            Health::Unavailable(_)
        ));
    }

    #[test]
    fn the_vault_is_local() {
        let dir = tempfile::tempdir().unwrap();
        assert!(VaultSink::new(dir.path()).is_local());
    }
}
