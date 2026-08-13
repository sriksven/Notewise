use chrono::Utc;
use rusqlite::Row;

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::id::Id;
use crate::models::{Project, Workspace};

/// Fields required to create a workspace. Timestamps and id are assigned by the repository.
#[derive(Debug, Clone)]
pub struct NewWorkspace {
    pub name: String,
}

/// Fields required to create a project.
#[derive(Debug, Clone)]
pub struct NewProject {
    pub workspace_id: Id,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug)]
pub struct WorkspaceRepository<'a> {
    db: &'a Database,
}

impl<'a> WorkspaceRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn create(&self, new: NewWorkspace) -> Result<Workspace> {
        let now = Utc::now();
        let workspace = Workspace {
            id: Id::new(),
            name: new.name,
            created_at: now,
            updated_at: now,
        };

        self.db.conn().execute(
            "INSERT INTO workspaces (id, name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                workspace.id,
                workspace.name,
                workspace.created_at,
                workspace.updated_at
            ],
        )?;

        Ok(workspace)
    }

    pub fn get(&self, id: Id) -> Result<Workspace> {
        self.db
            .conn()
            .query_row(
                "SELECT id, name, created_at, updated_at FROM workspaces WHERE id = ?1",
                rusqlite::params![id],
                map_workspace,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => StorageError::not_found("Workspace", id),
                other => other.into(),
            })
    }

    pub fn list(&self) -> Result<Vec<Workspace>> {
        let conn = self.db.conn();
        let mut stmt =
            conn.prepare("SELECT id, name, created_at, updated_at FROM workspaces ORDER BY name")?;
        let rows = stmt.query_map([], map_workspace)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn rename(&self, id: Id, name: &str) -> Result<Workspace> {
        let changed = self.db.conn().execute(
            "UPDATE workspaces SET name = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, name, Utc::now()],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("Workspace", id));
        }
        self.get(id)
    }

    /// Deletes the workspace and, by cascade, its projects.
    pub fn delete(&self, id: Id) -> Result<()> {
        let changed = self
            .db
            .conn()
            .execute("DELETE FROM workspaces WHERE id = ?1", rusqlite::params![id])?;
        if changed == 0 {
            return Err(StorageError::not_found("Workspace", id));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct ProjectRepository<'a> {
    db: &'a Database,
}

impl<'a> ProjectRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn create(&self, new: NewProject) -> Result<Project> {
        let now = Utc::now();
        let project = Project {
            id: Id::new(),
            workspace_id: new.workspace_id,
            name: new.name,
            description: new.description,
            created_at: now,
            updated_at: now,
        };

        self.db.conn().execute(
            "INSERT INTO projects (id, workspace_id, name, description, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                project.id,
                project.workspace_id,
                project.name,
                project.description,
                project.created_at,
                project.updated_at
            ],
        )?;

        Ok(project)
    }

    pub fn get(&self, id: Id) -> Result<Project> {
        self.db
            .conn()
            .query_row(
                "SELECT id, workspace_id, name, description, created_at, updated_at
                 FROM projects WHERE id = ?1",
                rusqlite::params![id],
                map_project,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => StorageError::not_found("Project", id),
                other => other.into(),
            })
    }

    pub fn list_in_workspace(&self, workspace_id: Id) -> Result<Vec<Project>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, workspace_id, name, description, created_at, updated_at
             FROM projects WHERE workspace_id = ?1 ORDER BY name",
        )?;
        let rows = stmt.query_map(rusqlite::params![workspace_id], map_project)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn delete(&self, id: Id) -> Result<()> {
        let changed = self
            .db
            .conn()
            .execute("DELETE FROM projects WHERE id = ?1", rusqlite::params![id])?;
        if changed == 0 {
            return Err(StorageError::not_found("Project", id));
        }
        Ok(())
    }
}

fn map_workspace(row: &Row<'_>) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: row.get(0)?,
        name: row.get(1)?,
        created_at: row.get(2)?,
        updated_at: row.get(3)?,
    })
}

fn map_project(row: &Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn workspace(db: &Database) -> Workspace {
        WorkspaceRepository::new(db)
            .create(NewWorkspace {
                name: "Acme".into(),
            })
            .expect("create workspace")
    }

    #[test]
    fn creates_and_reads_back_a_workspace() {
        let db = db();
        let repo = WorkspaceRepository::new(&db);
        let created = repo.create(NewWorkspace { name: "Acme".into() }).unwrap();
        assert_eq!(repo.get(created.id).unwrap(), created);
    }

    #[test]
    fn get_missing_workspace_reports_not_found() {
        let db = db();
        let err = WorkspaceRepository::new(&db)
            .get(Id::new())
            .expect_err("should be missing");
        assert!(matches!(err, StorageError::NotFound { kind: "Workspace", .. }));
    }

    #[test]
    fn lists_workspaces_alphabetically() {
        let db = db();
        let repo = WorkspaceRepository::new(&db);
        repo.create(NewWorkspace { name: "Zulu".into() }).unwrap();
        repo.create(NewWorkspace { name: "Alpha".into() }).unwrap();

        let names: Vec<_> = repo.list().unwrap().into_iter().map(|w| w.name).collect();
        assert_eq!(names, vec!["Alpha", "Zulu"]);
    }

    #[test]
    fn rename_updates_name_and_timestamp() {
        let db = db();
        let repo = WorkspaceRepository::new(&db);
        let created = repo.create(NewWorkspace { name: "Old".into() }).unwrap();

        let renamed = repo.rename(created.id, "New").unwrap();
        assert_eq!(renamed.name, "New");
        assert!(renamed.updated_at >= created.updated_at);
    }

    #[test]
    fn rename_missing_workspace_reports_not_found() {
        let db = db();
        let err = WorkspaceRepository::new(&db)
            .rename(Id::new(), "whatever")
            .expect_err("should be missing");
        assert!(matches!(err, StorageError::NotFound { .. }));
    }

    #[test]
    fn creates_project_within_workspace() {
        let db = db();
        let ws = workspace(&db);
        let repo = ProjectRepository::new(&db);

        let project = repo
            .create(NewProject {
                workspace_id: ws.id,
                name: "Launch".into(),
                description: Some("Q3 launch".into()),
            })
            .unwrap();

        assert_eq!(repo.get(project.id).unwrap(), project);
        assert_eq!(repo.list_in_workspace(ws.id).unwrap().len(), 1);
    }

    #[test]
    fn deleting_workspace_cascades_to_projects() {
        let db = db();
        let ws = workspace(&db);
        let projects = ProjectRepository::new(&db);
        let project = projects
            .create(NewProject {
                workspace_id: ws.id,
                name: "Launch".into(),
                description: None,
            })
            .unwrap();

        WorkspaceRepository::new(&db).delete(ws.id).unwrap();

        let err = projects.get(project.id).expect_err("should have cascaded");
        assert!(matches!(err, StorageError::NotFound { kind: "Project", .. }));
    }

    #[test]
    fn project_rejects_unknown_workspace() {
        let db = db();
        let err = ProjectRepository::new(&db)
            .create(NewProject {
                workspace_id: Id::new(),
                name: "Orphan".into(),
                description: None,
            })
            .expect_err("foreign key should reject this");
        assert!(matches!(err, StorageError::Sqlite(_)));
    }
}
