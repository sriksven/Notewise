//! Where the workspace lives on disk.
//!
//! # Why this is not left to each surface
//!
//! It was, and they disagreed. The CLI composed a vendor-neutral path by hand
//! (`~/Library/Application Support/notewise`), while the desktop shell asked Tauri for its
//! app-data directory, which is derived from the bundle identifier
//! (`~/Library/Application Support/dev.notewise.app`). Both are defensible conventions. Having
//! both is not: a user's meetings, notes and tickets silently landed in one of two databases
//! depending on which surface created them, and neither surface could see the other's.
//!
//! For a product whose entire promise is that the SQLite file is yours, "which copy?" is not a
//! question that may exist. So the answer lives here, once, in the crate that owns the
//! database — and every surface asks rather than derives.
//!
//! # Why the vendor-neutral path wins
//!
//! Three reasons. It holds the older and larger store for anyone who used the CLI first; it is
//! the same shape on every platform (`notewise/`, not a reverse-DNS identifier that only macOS
//! and Windows conventions produce); and the identifier-derived path's one real advantage —
//! staying inside a sandbox container — is an advantage Notewise must not take. A sandboxed
//! container is unreachable from the CLI and from `mcp-server`, which would make the split
//! permanent instead of merely current.

use std::path::{Path, PathBuf};

use crate::error::{Result, StorageError};

/// Overrides everything below. One variable relocates the whole workspace, and every surface
/// honours it, which is what makes a portable install or a test fixture possible.
pub const DATA_DIR_ENV: &str = "NOTEWISE_DATA_DIR";

/// The database file name, inside whichever directory is chosen.
pub const DB_FILE: &str = "notewise.db";

/// The application directory, under the platform's data root.
const APP_DIR: &str = "notewise";

/// Downloaded transcription models, inside the workspace directory.
const MODEL_DIR: &str = "models";

/// Directories an earlier build wrote a workspace into.
///
/// Kept as data rather than deleted so an existing install is not stranded. `dev.notewise.app`
/// is the Tauri bundle identifier the desktop shell used to resolve its own path from.
const LEGACY_APP_DIRS: &[&str] = &["dev.notewise.app"];

/// SQLite's sidecar files.
///
/// The database runs in WAL mode, so recent commits can live in `-wal` rather than in the main
/// file. Moving `notewise.db` alone would silently discard them — this is the difference
/// between a migration and a data-loss bug.
const SIDECARS: &[&str] = &["-wal", "-shm"];

/// The directory holding the workspace.
pub fn data_dir() -> Result<PathBuf> {
    data_dir_with(|key| std::env::var(key).ok())
}

/// The canonical database path. What every surface opens unless the user names another.
pub fn database_path() -> Result<PathBuf> {
    Ok(data_dir()?.join(DB_FILE))
}

/// Resolution against an arbitrary environment.
///
/// Split out so the platform rules can be tested without mutating the real process
/// environment, which would race every other test in this crate.
fn data_dir_with(get: impl Fn(&str) -> Option<String>) -> Result<PathBuf> {
    if let Some(dir) = get(DATA_DIR_ENV).filter(|d| !d.trim().is_empty()) {
        return Ok(PathBuf::from(dir));
    }

    let home = || {
        get("HOME")
            .or_else(|| get("USERPROFILE"))
            .map(PathBuf::from)
            .ok_or(StorageError::NoDataDirectory)
    };

    let base = if cfg!(target_os = "macos") {
        home()?.join("Library/Application Support")
    } else if cfg!(target_os = "windows") {
        match get("APPDATA") {
            Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
            _ => home()?.join("AppData/Roaming"),
        }
    } else {
        // XDG: honour the override before falling back to the spec's default.
        match get("XDG_DATA_HOME") {
            Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
            _ => home()?.join(".local/share"),
        }
    };

    Ok(base.join(APP_DIR))
}

/// Databases an earlier build may have left behind.
///
/// Empty when `NOTEWISE_DATA_DIR` is set: that variable named an exact location, and going
/// looking for other workspaces the user did not ask about would be the opposite of what an
/// explicit override means.
pub fn legacy_database_paths() -> Result<Vec<PathBuf>> {
    legacy_database_paths_with(|key| std::env::var(key).ok())
}

fn legacy_database_paths_with(get: impl Fn(&str) -> Option<String>) -> Result<Vec<PathBuf>> {
    if get(DATA_DIR_ENV).filter(|d| !d.trim().is_empty()).is_some() {
        return Ok(Vec::new());
    }

    // The legacy directories sit beside the canonical one, under the same platform root.
    let Some(root) = data_dir_with(&get)?.parent().map(Path::to_path_buf) else {
        return Ok(Vec::new());
    };

    Ok(LEGACY_APP_DIRS
        .iter()
        .map(|dir| root.join(dir).join(DB_FILE))
        .collect())
}

/// What [`adopt_legacy_workspace`] did, so a surface can say so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Adoption {
    /// Nothing to adopt. Either no legacy store exists, or the canonical one is the only one.
    Canonical,
    /// A lone legacy store was moved into the canonical location. The common case for anyone
    /// who only ever used the desktop app: their workspace follows them, silently and once.
    Adopted { from: PathBuf },
    /// More than one workspace exists and none of them was touched.
    ///
    /// Two databases with real content in them cannot be reconciled by a rename, and merging
    /// them means remapping every id and edge — a feature, not a migration. Refusing to guess
    /// keeps both intact and leaves the choice with the user.
    Split { legacy: Vec<PathBuf> },
}

impl Adoption {
    /// A message worth putting in front of the user, if there is one.
    ///
    /// Only [`Adoption::Split`] produces one. Adoption is deliberately silent: it is the
    /// outcome that loses nothing and surprises nobody.
    pub fn warning(&self) -> Option<String> {
        match self {
            Adoption::Split { legacy } => Some(format!(
                "another Notewise workspace exists at {} and is not being read. \
                 Earlier builds split the store between the app and the CLI. \
                 Nothing there has been modified; open it with `--db <path>` to look inside.",
                legacy
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            _ => None,
        }
    }
}

/// Bring a workspace left by an earlier build into the canonical location.
///
/// Safe to call on every launch: it does nothing at all once the canonical store exists and no
/// legacy one does, which is the steady state.
pub fn adopt_legacy_workspace(canonical: &Path) -> Result<Adoption> {
    adopt_from(canonical, &legacy_database_paths()?)
}

/// The directory holding downloaded transcription models, beside a workspace.
///
/// Named here rather than composed by each surface, for the reason the whole module exists:
/// three places deriving the same path is how they came to disagree.
pub fn model_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(MODEL_DIR)
}

/// Bring transcription models from an earlier install alongside the adopted workspace.
///
/// Deliberately unlike the database, which refuses to merge. A model file is a large immutable
/// blob named for the model it holds, so two files with the same name *are* the same model —
/// there is no ambiguity to resolve and nothing a user could lose by the wrong choice. Leaving
/// them behind, on the other hand, means silently re-downloading gigabytes because the
/// directory beside the workspace moved.
///
/// Colliding names are skipped rather than overwritten, nothing is ever deleted, and a file
/// that will not move is reported rather than fatal — a model can always be fetched again,
/// which is not a reason to refuse to start.
pub fn adopt_legacy_models(canonical_data_dir: &Path) -> Result<Vec<PathBuf>> {
    let legacy = legacy_model_dirs_with(|key| std::env::var(key).ok())?;
    Ok(adopt_models_from(&model_dir(canonical_data_dir), &legacy))
}

/// The model directories beside each legacy workspace.
fn legacy_model_dirs_with(get: impl Fn(&str) -> Option<String>) -> Result<Vec<PathBuf>> {
    Ok(legacy_database_paths_with(get)?
        .iter()
        .filter_map(|db| db.parent().map(model_dir))
        .collect())
}

fn adopt_models_from(canonical: &Path, legacy: &[PathBuf]) -> Vec<PathBuf> {
    let mut adopted = Vec::new();

    for dir in legacy {
        if dir == canonical || !dir.is_dir() {
            continue;
        }

        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };

        // Sorted, so a partially-completed run behaves the same way the next time.
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect();
        files.sort();

        for file in files {
            let Some(name) = file.file_name() else {
                continue;
            };
            let target = canonical.join(name);
            if target.exists() {
                // The same model is already here. Two files with this name hold the same
                // weights, so there is nothing to choose between them.
                continue;
            }
            if std::fs::create_dir_all(canonical).is_err() {
                break;
            }
            match move_file(&file, &target) {
                Ok(()) => adopted.push(target),
                Err(_) => continue,
            }
        }
    }

    adopted
}

/// The policy, against an explicit candidate list, so it can be tested without a home
/// directory.
fn adopt_from(canonical: &Path, legacy: &[PathBuf]) -> Result<Adoption> {
    let mut existing: Vec<PathBuf> = legacy
        .iter()
        .filter(|path| path.as_path() != canonical && path.is_file())
        .cloned()
        .collect();
    // Deterministic: which store gets adopted must not depend on directory iteration order.
    existing.sort();

    match existing.len() {
        0 => Ok(Adoption::Canonical),
        // Both exist. Neither is touched — see `Adoption::Split`.
        _ if canonical.is_file() => Ok(Adoption::Split { legacy: existing }),
        1 => {
            let from = existing.remove(0);
            if let Some(parent) = canonical.parent() {
                std::fs::create_dir_all(parent).map_err(|source| StorageError::DataDir {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            move_workspace(&from, canonical)?;
            Ok(Adoption::Adopted { from })
        }
        // Several legacy stores and no canonical one. Picking one would be a guess about which
        // of the user's workspaces is the real one, so it stays a report.
        _ => Ok(Adoption::Split { legacy: existing }),
    }
}

/// Move a database and its WAL sidecars.
///
/// Falls back to copy-then-remove because the canonical directory and a legacy one are not
/// guaranteed to be on the same filesystem, and `rename` fails across mount points.
fn move_workspace(from: &Path, to: &Path) -> Result<()> {
    move_file(from, to)?;

    for suffix in SIDECARS {
        let sidecar_from = sidecar(from, suffix);
        if sidecar_from.is_file() {
            move_file(&sidecar_from, &sidecar(to, suffix))?;
        }
    }

    Ok(())
}

/// `notewise.db` + `-wal` is `notewise.db-wal`, which is a suffix on the file name rather than
/// a change of extension — `set_extension` would produce `notewise-wal` and lose the data.
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

fn move_file(from: &Path, to: &Path) -> Result<()> {
    let failed = |source| StorageError::WorkspaceMove {
        from: from.to_path_buf(),
        to: to.to_path_buf(),
        source,
    };

    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::fs::copy(from, to).map_err(failed)?;
            std::fs::remove_file(from).map_err(failed)?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An environment with nothing in it but a home directory.
    fn env(home: &Path) -> impl Fn(&str) -> Option<String> + '_ {
        move |key| match key {
            "HOME" | "USERPROFILE" => Some(home.display().to_string()),
            _ => None,
        }
    }

    fn touch(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn the_workspace_directory_is_vendor_neutral() {
        // Not a reverse-DNS bundle identifier: the CLI and `mcp-server` have no bundle, and a
        // path only one surface can derive is how the store came to be split.
        let dir = data_dir_with(env(Path::new("/home/x"))).unwrap();
        assert!(dir.ends_with("notewise"), "{}", dir.display());
    }

    #[test]
    fn the_data_dir_variable_wins() {
        let dir = data_dir_with(|key| (key == DATA_DIR_ENV).then(|| "/somewhere".to_string()));
        assert_eq!(dir.unwrap(), PathBuf::from("/somewhere"));
    }

    #[test]
    fn an_empty_data_dir_variable_is_ignored() {
        // An exported-but-blank variable is a shell accident, not a request to put the
        // workspace in the filesystem root.
        let dir = data_dir_with(|key| match key {
            DATA_DIR_ENV => Some("   ".to_string()),
            "HOME" | "USERPROFILE" => Some("/home/x".to_string()),
            _ => None,
        })
        .unwrap();
        assert!(dir.ends_with("notewise"), "{}", dir.display());
    }

    #[test]
    fn without_a_home_directory_it_says_so_rather_than_guessing() {
        assert!(matches!(
            data_dir_with(|_| None),
            Err(StorageError::NoDataDirectory)
        ));
    }

    #[test]
    fn the_legacy_directory_sits_beside_the_canonical_one() {
        let legacy = legacy_database_paths_with(env(Path::new("/home/x"))).unwrap();
        assert!(
            legacy
                .iter()
                .any(|p| p.ends_with("dev.notewise.app/notewise.db")),
            "{legacy:?}"
        );
        let canonical = data_dir_with(env(Path::new("/home/x"))).unwrap();
        assert_eq!(legacy[0].parent().unwrap().parent(), canonical.parent());
    }

    #[test]
    fn an_explicit_data_dir_suppresses_the_legacy_search() {
        // The user named a location. Adopting something from elsewhere into it would override
        // the one instruction they gave.
        let legacy =
            legacy_database_paths_with(|key| (key == DATA_DIR_ENV).then(|| "/w".to_string()));
        assert!(legacy.unwrap().is_empty());
    }

    #[test]
    fn nothing_to_adopt_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join("notewise/notewise.db");
        let legacy = dir.path().join("legacy/notewise.db");

        assert_eq!(
            adopt_from(&canonical, &[legacy]).unwrap(),
            Adoption::Canonical
        );
        assert!(!canonical.exists());
    }

    #[test]
    fn a_lone_legacy_store_is_adopted() {
        // Anyone who only ever opened the desktop app: their workspace moves once, quietly,
        // and every surface sees it afterwards.
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join("notewise/notewise.db");
        let legacy = dir.path().join("dev.notewise.app/notewise.db");
        touch(&legacy, "the only workspace");

        let outcome = adopt_from(&canonical, std::slice::from_ref(&legacy)).unwrap();

        assert_eq!(
            outcome,
            Adoption::Adopted {
                from: legacy.clone()
            }
        );
        assert_eq!(
            std::fs::read_to_string(&canonical).unwrap(),
            "the only workspace"
        );
        assert!(
            !legacy.exists(),
            "the legacy store should have moved, not been copied"
        );
        assert_eq!(outcome.warning(), None, "adoption is silent");
    }

    #[test]
    fn adoption_takes_the_wal_sidecar_with_it() {
        // WAL mode means recent commits can live entirely in `-wal`. Moving the database
        // without it is how a migration turns into data loss.
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join("notewise/notewise.db");
        let legacy = dir.path().join("dev.notewise.app/notewise.db");
        touch(&legacy, "db");
        touch(&legacy.with_file_name("notewise.db-wal"), "recent commits");
        touch(&legacy.with_file_name("notewise.db-shm"), "shared memory");

        adopt_from(&canonical, std::slice::from_ref(&legacy)).unwrap();

        assert_eq!(
            std::fs::read_to_string(canonical.with_file_name("notewise.db-wal")).unwrap(),
            "recent commits"
        );
        assert!(canonical.with_file_name("notewise.db-shm").is_file());
        assert!(!legacy.with_file_name("notewise.db-wal").exists());
    }

    #[test]
    fn two_populated_stores_are_both_left_alone() {
        // The case that must never lose data: merging is a feature, not a migration.
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join("notewise/notewise.db");
        let legacy = dir.path().join("dev.notewise.app/notewise.db");
        touch(&canonical, "nineteen meetings");
        touch(&legacy, "three meetings");

        let outcome = adopt_from(&canonical, std::slice::from_ref(&legacy)).unwrap();

        assert_eq!(
            outcome,
            Adoption::Split {
                legacy: vec![legacy.clone()]
            }
        );
        assert_eq!(
            std::fs::read_to_string(&canonical).unwrap(),
            "nineteen meetings"
        );
        assert_eq!(std::fs::read_to_string(&legacy).unwrap(), "three meetings");

        let warning = outcome.warning().expect("a split store must be reported");
        assert!(warning.contains("dev.notewise.app"), "{warning}");
    }

    #[test]
    fn several_legacy_stores_are_reported_rather_than_ranked() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join("notewise/notewise.db");
        let one = dir.path().join("a/notewise.db");
        let two = dir.path().join("b/notewise.db");
        touch(&one, "one");
        touch(&two, "two");

        let outcome = adopt_from(&canonical, &[one.clone(), two.clone()]).unwrap();

        assert_eq!(
            outcome,
            Adoption::Split {
                legacy: vec![one.clone(), two.clone()]
            }
        );
        assert!(!canonical.exists(), "nothing should have been adopted");
        assert!(one.is_file() && two.is_file());
    }

    #[test]
    fn legacy_models_are_looked_for_beside_the_legacy_workspace() {
        // The composition that actually runs on a user's machine: the models directory sits
        // inside the old app-data directory, not beside the new one.
        let dirs = legacy_model_dirs_with(env(Path::new("/home/x"))).unwrap();
        assert!(
            dirs.iter().any(|d| d.ends_with("dev.notewise.app/models")),
            "{dirs:?}"
        );
        let canonical = model_dir(&data_dir_with(env(Path::new("/home/x"))).unwrap());
        assert!(
            canonical.ends_with("notewise/models"),
            "{}",
            canonical.display()
        );
        assert!(!dirs.contains(&canonical));
    }

    #[test]
    fn models_from_an_earlier_install_come_along() {
        // Otherwise moving the workspace directory silently orphans gigabytes and the app
        // re-downloads a model that is already on the disk.
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join("notewise/models");
        let legacy = dir.path().join("dev.notewise.app/models");
        touch(&legacy.join("ggml-base.en.bin"), "weights");

        let adopted = adopt_models_from(&canonical, std::slice::from_ref(&legacy));

        assert_eq!(adopted, vec![canonical.join("ggml-base.en.bin")]);
        assert_eq!(
            std::fs::read_to_string(canonical.join("ggml-base.en.bin")).unwrap(),
            "weights"
        );
        assert!(!legacy.join("ggml-base.en.bin").exists());
    }

    #[test]
    fn a_model_already_present_is_never_overwritten() {
        // Same name means same weights, so there is nothing to choose between them — and
        // clobbering the one in use to prove it would be the worst of both.
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join("notewise/models");
        let legacy = dir.path().join("dev.notewise.app/models");
        touch(&canonical.join("ggml-tiny.en.bin"), "the one in use");
        touch(&legacy.join("ggml-tiny.en.bin"), "the older copy");
        touch(&legacy.join("ggml-large-v3.bin"), "only over here");

        let adopted = adopt_models_from(&canonical, std::slice::from_ref(&legacy));

        assert_eq!(adopted, vec![canonical.join("ggml-large-v3.bin")]);
        assert_eq!(
            std::fs::read_to_string(canonical.join("ggml-tiny.en.bin")).unwrap(),
            "the one in use"
        );
        assert_eq!(
            std::fs::read_to_string(legacy.join("ggml-tiny.en.bin")).unwrap(),
            "the older copy",
            "a skipped model stays where it is rather than being deleted"
        );
    }

    #[test]
    fn adopting_models_with_nothing_to_adopt_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join("notewise/models");
        let legacy = dir.path().join("dev.notewise.app/models");

        assert!(adopt_models_from(&canonical, std::slice::from_ref(&legacy)).is_empty());
        assert!(!canonical.exists(), "no empty directory left behind");
    }

    #[test]
    fn a_legacy_path_equal_to_the_canonical_one_is_not_adopted_onto_itself() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join("notewise/notewise.db");
        touch(&canonical, "the workspace");

        assert_eq!(
            adopt_from(&canonical, std::slice::from_ref(&canonical)).unwrap(),
            Adoption::Canonical
        );
        assert_eq!(
            std::fs::read_to_string(&canonical).unwrap(),
            "the workspace"
        );
    }
}
