//! A watched folder, read as a connector.
//!
//! # Why a connector rather than a bespoke watcher
//!
//! A folder is outside the app, and "read things from outside" is what the connector seam is for.
//! Going around it would mean reimplementing cursor handling, idempotent upsert and per-source
//! failure isolation that `Importer` already has.
//!
//! # Why a scan rather than a filesystem watcher
//!
//! No `FSEvents`, no `inotify`, no `ReadDirectoryChangesW`. A cross-platform watcher is a dependency
//! and a source of platform-specific missed events, and the requirement is loose — a document edited
//! now needs to be searchable in minutes, not milliseconds. Polling also composes with `Importer`,
//! which is invoked rather than event-driven.
//!
//! # Why the traversal is bounded
//!
//! Somebody will point this at their home directory. Depth, file count, extension and per-file size
//! are all capped so that degrades into "imported the first few hundred text files" rather than
//! embedding a machine.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::connector::{Connector, SourceConnector};
use crate::error::{ConnectorError, Result};
use crate::types::{Cursor, Health, Inbound, PullBatch};

/// How deep to walk below the watched folder.
pub const MAX_DEPTH: usize = 8;

/// How many files one scan will take.
pub const MAX_FILES: usize = 500;

/// The largest file worth importing whole.
///
/// A megabyte of text is a very long document. Beyond it the thing is probably not prose, and storing
/// the body would put megabytes into the prompt budget of every grounded answer.
pub const MAX_BYTES: u64 = 1024 * 1024;

/// Extensions read as text.
///
/// An allowlist rather than a denylist: a folder contains binaries, archives and images, and reading
/// an unknown extension as UTF-8 produces either an error or garbage indexed as prose.
pub const EXTENSIONS: &[&str] = &["md", "markdown", "txt", "text", "rst", "org"];

/// One file the scan decided to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub byte_size: u64,
    pub modified_at: DateTime<Utc>,
}

/// Whether a path is a file this connector reads.
///
/// Pure, so the allowlist and the size cap are testable without a filesystem.
pub fn is_readable(path: &Path, byte_size: u64) -> bool {
    if byte_size > MAX_BYTES {
        return false;
    }
    // Hidden files and directories are skipped: `.git`, `.obsidian` and friends are full of text
    // that is machinery rather than writing.
    if path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
    {
        return false;
    }

    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| EXTENSIONS.contains(&e.as_str()))
}

/// Walk a folder, bounded.
///
/// Returns what it found, and stops at [`MAX_FILES`] rather than refusing — a folder with more files
/// than that should import some of them, not none.
pub fn scan(root: &Path) -> Vec<Found> {
    let mut found = Vec::new();
    let mut queue = vec![(root.to_path_buf(), 0usize)];

    while let Some((dir, depth)) = queue.pop() {
        if depth > MAX_DEPTH || found.len() >= MAX_FILES {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            // An unreadable directory is skipped rather than aborting the scan. A permission
            // problem in one subfolder should not lose the rest of a vault.
            continue;
        };

        for entry in entries.flatten() {
            if found.len() >= MAX_FILES {
                break;
            }
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };

            if meta.is_dir() {
                if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.'))
                {
                    continue;
                }
                queue.push((path, depth + 1));
            } else if meta.is_file() && is_readable(&path, meta.len()) {
                found.push(Found {
                    path,
                    byte_size: meta.len(),
                    modified_at: meta
                        .modified()
                        .map(DateTime::<Utc>::from)
                        .unwrap_or_else(|_| Utc::now()),
                });
            }
        }
    }

    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

/// A title for a document, from its filename.
///
/// The first heading would often be better, but a file whose first line is not a heading would then
/// get a title from arbitrary prose. The filename is what the user chose and what they will look for.
pub fn title_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.replace(['-', '_'], " "))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

#[derive(Debug)]
pub struct Documents {
    root: PathBuf,
}

impl Documents {
    /// The one place this connector's name is written.
    pub const ID: &'static str = "documents";

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[async_trait]
impl Connector for Documents {
    fn id(&self) -> &str {
        Self::ID
    }

    fn display_name(&self) -> &str {
        "Watched folder"
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn health(&self) -> Result<Health> {
        if self.root.is_dir() {
            Ok(Health::Ok)
        } else {
            Ok(Health::Unavailable(format!(
                "{} is not a folder",
                self.root.display()
            )))
        }
    }
}

#[async_trait]
impl SourceConnector for Documents {
    async fn pull(&self, since: Cursor) -> Result<PullBatch> {
        if !self.root.is_dir() {
            // `NotConfigured` rather than transient: a path that is not a folder is a setting to
            // correct, not something to keep retrying.
            return Err(ConnectorError::NotConfigured(format!(
                "{} is not a folder",
                self.root.display()
            )));
        }

        // The high-water modification time from the last scan. Files older than it are unchanged, so
        // re-reading their contents would be work for no new information — the same rolling-window
        // shape the Google bridge uses, and for the same reason: no change feed exists.
        let since_time = since
            .as_deref()
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
            .map(|t| t.with_timezone(&Utc));

        let root = self.root.clone();
        // Off the async runtime: a scan of a large folder is blocking filesystem work, and doing it
        // on a runtime thread would stall every other request.
        let found = tokio::task::spawn_blocking(move || scan(&root))
            .await
            .map_err(|e| ConnectorError::Transient(format!("the scan did not finish: {e}")))?;

        let mut newest = since_time;
        let mut items = Vec::new();

        for file in found {
            if let Some(cutoff) = since_time {
                if file.modified_at <= cutoff {
                    continue;
                }
            }
            newest =
                Some(newest.map_or(file.modified_at, |n: DateTime<Utc>| n.max(file.modified_at)));

            match read_document(&file) {
                Ok(item) => items.push(item),
                // One unreadable file is skipped. A folder is somebody's real directory and will
                // contain something with a text extension that is not text.
                Err(e) => tracing::warn!(
                    path = %file.path.display(),
                    error = %e,
                    "skipping a file that could not be read"
                ),
            }
        }

        Ok(PullBatch {
            items,
            next_cursor: Cursor(newest.map(|t| t.to_rfc3339())),
        })
    }
}

fn read_document(file: &Found) -> Result<Inbound> {
    let body = std::fs::read_to_string(&file.path)
        .map_err(|e| ConnectorError::Permanent(format!("not readable as text: {e}")))?;

    Ok(Inbound {
        // The path is the identity: the same file re-read is the same document, and a moved file is
        // a new one whose old row becomes missing. Content-hashing would make a moved file the same
        // document and a reverted edit a different one, which is the wrong way round.
        external_id: file.path.display().to_string(),
        url: None,
        title: Some(title_of(&file.path)),
        remote_version: None,
        occurred_at: Some(file.modified_at),
        payload: serde_json::json!({
            "path": file.path.display().to_string(),
            "title": title_of(&file.path),
            "body": body,
            "byte_size": file.byte_size,
            "modified_at": file.modified_at.to_rfc3339(),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("notes.md"), "the cost structure").expect("write");
        fs::write(dir.path().join("readme.txt"), "plain").expect("write");
        fs::write(dir.path().join("photo.png"), [0u8, 1, 2]).expect("write");
        fs::create_dir(dir.path().join("sub")).expect("mkdir");
        fs::write(dir.path().join("sub/deeper.md"), "nested").expect("write");
        fs::create_dir(dir.path().join(".git")).expect("mkdir");
        fs::write(dir.path().join(".git/config"), "machinery").expect("write");
        fs::write(dir.path().join(".hidden.md"), "hidden").expect("write");
        dir
    }

    #[test]
    fn the_id_is_a_stored_value_and_cannot_drift() {
        assert_eq!(Documents::ID, "documents");
    }

    #[test]
    fn only_text_extensions_are_read() {
        assert!(is_readable(Path::new("a/notes.md"), 10));
        assert!(is_readable(Path::new("a/notes.MD"), 10));
        assert!(is_readable(Path::new("a/readme.txt"), 10));
        assert!(!is_readable(Path::new("a/photo.png"), 10));
        assert!(!is_readable(Path::new("a/archive.zip"), 10));
        assert!(!is_readable(Path::new("a/no-extension"), 10));
    }

    #[test]
    fn something_enormous_is_not_read_whole() {
        assert!(!is_readable(Path::new("a/huge.md"), MAX_BYTES + 1));
        assert!(is_readable(Path::new("a/big.md"), MAX_BYTES));
    }

    /// `.git` and `.obsidian` are full of text that is machinery rather than writing.
    #[test]
    fn hidden_files_and_folders_are_skipped() {
        assert!(!is_readable(Path::new("a/.hidden.md"), 10));

        let dir = tree();
        let found: Vec<String> = scan(dir.path())
            .into_iter()
            .map(|f| f.path.display().to_string())
            .collect();

        assert!(!found.iter().any(|p| p.contains(".git")), "{found:?}");
        assert!(!found.iter().any(|p| p.contains(".hidden")), "{found:?}");
    }

    #[test]
    fn a_scan_finds_text_at_depth_and_ignores_the_rest() {
        let dir = tree();
        let names: Vec<String> = scan(dir.path())
            .into_iter()
            .filter_map(|f| f.path.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();

        assert!(names.contains(&"notes.md".to_string()), "{names:?}");
        assert!(names.contains(&"readme.txt".to_string()), "{names:?}");
        assert!(names.contains(&"deeper.md".to_string()), "{names:?}");
        assert!(!names.contains(&"photo.png".to_string()), "{names:?}");
    }

    #[test]
    fn a_folder_that_is_not_there_is_reported_not_panicked() {
        assert!(scan(Path::new("/nowhere/that/exists")).is_empty());
    }

    #[test]
    fn a_title_comes_from_the_filename() {
        assert_eq!(
            title_of(Path::new("/v/architecture-notes.md")),
            "architecture notes"
        );
        assert_eq!(title_of(Path::new("/v/some_file.txt")), "some file");
        assert_eq!(title_of(Path::new("/v/plain.md")), "plain");
    }

    #[tokio::test]
    async fn a_pull_returns_the_folders_documents() {
        let dir = tree();
        let batch = Documents::new(dir.path())
            .pull(Cursor::start())
            .await
            .expect("pull");

        assert_eq!(batch.items.len(), 3, "two at the root, one nested");
        assert!(
            batch.next_cursor.0.is_some(),
            "the high-water mark is recorded"
        );

        let notes = batch
            .items
            .iter()
            .find(|i| i.title.as_deref() == Some("notes"))
            .expect("notes.md");
        assert_eq!(notes.payload["body"], "the cost structure");
    }

    /// Re-reading unchanged files is work for no new information.
    #[tokio::test]
    async fn a_second_pull_skips_what_has_not_changed() {
        let dir = tree();
        let connector = Documents::new(dir.path());

        let first = connector.pull(Cursor::start()).await.expect("first");
        let second = connector
            .pull(first.next_cursor.clone())
            .await
            .expect("second");

        assert_eq!(first.items.len(), 3);
        assert!(
            second.items.is_empty(),
            "nothing changed, so nothing should be re-read: {:?}",
            second
                .items
                .iter()
                .map(|i| &i.external_id)
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn a_missing_folder_is_an_error_rather_than_an_empty_success() {
        let err = Documents::new("/nowhere/that/exists")
            .pull(Cursor::start())
            .await
            .expect_err("must fail");

        // Reporting success with nothing found would look identical to an empty folder, and the
        // user would never learn their path is wrong.
        assert!(matches!(err, ConnectorError::NotConfigured(_)), "{err:?}");
    }

    #[tokio::test]
    async fn health_says_when_the_folder_is_gone() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            Documents::new(dir.path()).health().await.expect("health"),
            Health::Ok
        );

        match Documents::new("/nowhere").health().await.expect("health") {
            Health::Unavailable(reason) => assert!(reason.contains("not a folder"), "{reason}"),
            other => panic!("expected a complaint, got {other:?}"),
        }
    }

    /// A moved file is a new document whose old row becomes missing — not the same document.
    #[tokio::test]
    async fn the_path_is_the_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("a.md"), "same content").expect("write");

        let batch = Documents::new(dir.path())
            .pull(Cursor::start())
            .await
            .expect("pull");

        assert!(
            batch.items[0].external_id.ends_with("a.md"),
            "{}",
            batch.items[0].external_id
        );
    }

    #[test]
    fn a_scan_stops_at_the_file_cap_rather_than_refusing() {
        let dir = tempfile::tempdir().expect("tempdir");
        for n in 0..(MAX_FILES + 25) {
            fs::write(dir.path().join(format!("f{n}.md")), "x").expect("write");
        }

        let found = scan(dir.path());
        assert_eq!(
            found.len(),
            MAX_FILES,
            "a folder with too many files should import some, not none"
        );
    }
}
