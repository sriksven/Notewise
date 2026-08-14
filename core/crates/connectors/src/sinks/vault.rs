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
use sha2::{Digest, Sha256};

use crate::connector::{Connector, SinkConnector};
use crate::error::{ConnectorError, Result};
use crate::types::{ExternalRef, Health, Outbound};

/// Fingerprint of what is in a file, used to tell our own last write from someone else's edit.
///
/// A hash rather than a modification time: a sync client touching a file, or a restore from a
/// backup, moves the timestamp without changing a word. What matters is whether the bytes are
/// still the bytes we wrote.
fn fingerprint(content: &str) -> String {
    hex::encode(Sha256::digest(content.as_bytes()))
}

#[derive(Debug)]
pub struct VaultSink {
    root: PathBuf,
}

impl VaultSink {
    /// The one place this connector's name is written.
    ///
    /// It appears in the registry builder, in the API's list of ids it will accept, and in the
    /// credential store's key — and it is persisted in `connector_outbox` and `external_items`,
    /// so a rename is a breaking change to data already on disk. As three separate string
    /// literals that drift silently: the API accepts `POST /v1/connectors/vault`, writes the
    /// account row, and the registry never registers it. Named once, a rename is a compile
    /// error at every site instead.
    pub const ID: &'static str = "vault";

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
        Self::ID
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

        // Never overwrite something the user wrote.
        //
        // The whole promise of this connector is that the files are theirs — a vault lives in
        // Obsidian, and people annotate meeting notes. Blind writes make that promise false in
        // the one case where it matters, and silently: the edit is gone with no error and no
        // copy. So a file that no longer matches what we last put there is treated as theirs.
        //
        // Only claimed when there is a previous version to compare against. A first push, or
        // one whose record predates this check, cannot tell an edit from a file it wrote
        // itself, and refusing on that basis would strand every meeting behind a conflict that
        // never happened.
        if let Some(previous) = outbound
            .existing
            .as_ref()
            .and_then(|e| e.remote_version.as_deref())
        {
            match tokio::fs::read_to_string(&path).await {
                Ok(current) if fingerprint(&current) != previous => {
                    return Err(ConnectorError::Permanent(format!(
                        "{} has been edited since Notewise last wrote it — not overwriting. \
                         Move or delete the file to let it be rewritten.",
                        path.display()
                    )));
                }
                // Unchanged since our last write, or gone. Both are ours to write.
                Ok(_) | Err(_) => {}
            }
        }

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
            // What we just wrote, so the next push can tell it from an edit. Previously `None`,
            // which left the comparison above with nothing to compare against.
            remote_version: Some(fingerprint(markdown)),
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

    /// The literal is pinned here on purpose, and this is the only place it should be written
    /// besides the constant itself. It is persisted in `connector_outbox` and `external_items`,
    /// so changing it orphans every row already on disk — a deliberate migration, never a
    /// rename that compiled.
    #[test]
    fn the_id_is_a_stored_value_and_cannot_drift() {
        assert_eq!(VaultSink::ID, "vault");
        assert_eq!(VaultSink::new("/tmp").id(), VaultSink::ID);
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

    /// The promise of this connector is that the files are yours. A vault lives in Obsidian and
    /// people annotate meeting notes; overwriting that loses work silently, with no error and
    /// no copy.
    #[tokio::test]
    async fn an_edited_file_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let sink = VaultSink::new(dir.path());

        let first = outbound("Standup", "v1");
        let reference = sink.push(&first).await.unwrap();
        let path = dir.path().join(&reference.external_id);

        // The user opens it in their editor and adds a line.
        std::fs::write(&path, "v1\n\nMy own note about this meeting.").unwrap();

        let second = Outbound {
            payload: serde_json::json!({"title": "Standup", "markdown": "v2"}),
            existing: Some(reference),
            ..first.clone()
        };

        let refused = sink.push(&second).await;
        assert!(
            matches!(refused, Err(ConnectorError::Permanent(_))),
            "an edited file must not be overwritten, got {refused:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "v1\n\nMy own note about this meeting.",
            "the user's edit must survive"
        );
    }

    /// The other half: a file we wrote and nobody touched is still ours to update. Refusing
    /// here would strand every meeting behind a conflict that never happened.
    #[tokio::test]
    async fn an_untouched_file_is_still_updated() {
        let dir = tempfile::tempdir().unwrap();
        let sink = VaultSink::new(dir.path());

        let first = outbound("Standup", "v1");
        let reference = sink.push(&first).await.unwrap();
        let path = dir.path().join(&reference.external_id);

        let second = Outbound {
            payload: serde_json::json!({"title": "Standup", "markdown": "v2"}),
            existing: Some(reference),
            ..first.clone()
        };

        sink.push(&second).await.expect("an untouched file updates");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v2");
    }

    /// A push with no prior version cannot tell an edit from its own earlier write, so it must
    /// not guess. Anything else would refuse on first contact with a pre-existing vault.
    #[tokio::test]
    async fn a_first_push_does_not_claim_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let sink = VaultSink::new(dir.path());

        let first = outbound("Standup", "v1");
        let reference = sink.push(&first).await.unwrap();
        std::fs::write(dir.path().join(&reference.external_id), "edited").unwrap();

        // `existing` carries no version — the record predates this check.
        let second = Outbound {
            payload: serde_json::json!({"title": "Standup", "markdown": "v2"}),
            existing: Some(ExternalRef {
                remote_version: None,
                ..reference.clone()
            }),
            ..first.clone()
        };

        sink.push(&second).await.expect("no version means no claim");
    }

    #[tokio::test]
    async fn a_write_records_the_version_it_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let sink = VaultSink::new(dir.path());

        let reference = sink.push(&outbound("Standup", "body")).await.unwrap();
        assert_eq!(
            reference.remote_version.as_deref(),
            Some(fingerprint("body").as_str()),
            "without this the conflict check has nothing to compare against"
        );
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
