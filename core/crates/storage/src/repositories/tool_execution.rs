//! External MCP servers, which of their tools are allowed, and every call proposed.
//!
//! # Why these are persisted when agent runs are not
//!
//! `api-server`'s agent keeps its runs in memory and argues, correctly, that a trace is only
//! interesting while it is happening and that what matters — the note it wrote — survives anyway.
//!
//! Nothing about that transfers here. The effect of a tool call is *outside* Notewise: a ticket was
//! filed, a message was posted, a card may have been charged. Losing the record on restart leaves
//! the user unable to answer "did that already run?", and guessing wrong costs a duplicate side
//! effect in somebody else's system, which nothing in this app can undo.
//!
//! So nothing here is pruned, unlike `job_runs`. A bounded history would eventually delete the
//! answer to the only question these rows exist to answer.
//!
//! # Default-deny, composed
//!
//! A tool is reachable only when its server is enabled *and* a row enables the tool. Two switches,
//! both off by default, so a server added and forgotten grants nothing and a server disabled in a
//! hurry takes all of its tools with it.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row};

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::id::Id;

use super::decode_enum;

/// How a server is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpTransport {
    /// A child process, spoken to over its stdin and stdout.
    Stdio,
    /// A remote server over streamable HTTP.
    Http,
}

impl McpTransport {
    pub fn as_str(&self) -> &'static str {
        match self {
            McpTransport::Stdio => "stdio",
            McpTransport::Http => "http",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "stdio" => McpTransport::Stdio,
            "http" => McpTransport::Http,
            _ => return None,
        })
    }
}

/// A configured external server.
///
/// Credentials are absent on purpose. Environment variables for a stdio server and headers for an
/// HTTP one hold secrets, so they live in the keychain and this row holds only the server's
/// identity — the same split routing rules make for provider keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    pub id: Id,
    pub name: String,
    pub transport: McpTransport,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub enabled: bool,
    pub auto_start: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewMcpServer {
    pub name: String,
    pub transport: McpTransport,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub auto_start: bool,
}

/// Where a proposed call has got to.
///
/// # Why `Unknown` exists when the design doc lists five statuses
///
/// A call that timed out may have taken effect. The doc says so — "`Timeout` says 'unknown', not
/// 'failed', in the UI" — and also lists `failed` as the status to record. Both cannot hold unless
/// the interface derives "unknown" by reading the error text, and deciding what to tell a user about
/// a possibly-duplicated side effect by string-matching an error message is precisely the fragility
/// these rows exist to prevent.
///
/// So a timeout gets its own status. The distinction is the one the user acts on: a failure can be
/// retried by hand, and an unknown outcome means checking the other system first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionStatus {
    /// A model proposed it and it passed schema validation. Nothing has been sent.
    Proposed,
    /// A human approved it. Still nothing has been sent.
    Confirmed,
    Succeeded,
    Failed,
    /// It timed out. Whether it took effect is not known.
    Unknown,
    /// A human declined. Nothing was sent and nothing ever will be.
    Rejected,
}

impl ExecutionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecutionStatus::Proposed => "proposed",
            ExecutionStatus::Confirmed => "confirmed",
            ExecutionStatus::Succeeded => "succeeded",
            ExecutionStatus::Failed => "failed",
            ExecutionStatus::Unknown => "unknown",
            ExecutionStatus::Rejected => "rejected",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "proposed" => ExecutionStatus::Proposed,
            "confirmed" => ExecutionStatus::Confirmed,
            "succeeded" => ExecutionStatus::Succeeded,
            "failed" => ExecutionStatus::Failed,
            "unknown" => ExecutionStatus::Unknown,
            "rejected" => ExecutionStatus::Rejected,
            _ => return None,
        })
    }

    /// Whether this is the end of the line.
    pub fn is_final(&self) -> bool {
        !matches!(self, ExecutionStatus::Proposed | ExecutionStatus::Confirmed)
    }
}

/// One proposed, confirmed, or completed external call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecution {
    pub id: Id,
    /// The action item this came from, if any. `ON DELETE SET NULL`: deleting the task must not
    /// erase the record that something was done in another system on its behalf.
    pub action_item_id: Option<Id>,
    pub server_id: Id,
    pub tool_name: String,
    /// The arguments, exactly as they will be or were sent.
    ///
    /// Kept as the original text rather than a re-serialized value, because what a user confirmed
    /// and what was sent have to be the same bytes for the confirmation to mean anything.
    pub arguments: String,
    pub status: ExecutionStatus,
    /// The server's answer, or the error. JSON for a success, a message otherwise.
    pub result: Option<String>,
    pub proposed_at: DateTime<Utc>,
    pub executed_at: Option<DateTime<Utc>>,
}

impl ToolExecution {
    /// The arguments as a value, for sending.
    pub fn arguments_value(&self) -> Result<serde_json::Value> {
        serde_json::from_str(&self.arguments).map_err(|e| StorageError::Corrupt {
            column: "tool_executions.arguments",
            reason: e.to_string(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct NewToolExecution {
    pub action_item_id: Option<Id>,
    pub server_id: Id,
    pub tool_name: String,
    pub arguments: String,
}

/// How a call ended.
#[derive(Debug, Clone)]
pub enum Outcome {
    Succeeded {
        result: String,
    },
    Failed {
        error: String,
    },
    /// Timed out. Not a failure — see [`ExecutionStatus::Unknown`].
    Unknown {
        detail: String,
    },
}

impl Outcome {
    fn status(&self) -> ExecutionStatus {
        match self {
            Outcome::Succeeded { .. } => ExecutionStatus::Succeeded,
            Outcome::Failed { .. } => ExecutionStatus::Failed,
            Outcome::Unknown { .. } => ExecutionStatus::Unknown,
        }
    }

    fn detail(&self) -> &str {
        match self {
            Outcome::Succeeded { result } => result,
            Outcome::Failed { error } => error,
            Outcome::Unknown { detail } => detail,
        }
    }
}

// ---------------------------------------------------------------- servers

#[derive(Debug)]
pub struct McpServerRepository<'a> {
    db: &'a Database,
}

impl<'a> McpServerRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Add a server. Disabled, because connecting a client must not grant capability as a side
    /// effect of having typed a command in.
    pub fn create(&self, new: NewMcpServer) -> Result<McpServer> {
        let server = McpServer {
            id: Id::new(),
            name: new.name,
            transport: new.transport,
            command: new.command,
            args: new.args,
            url: new.url,
            enabled: false,
            auto_start: new.auto_start,
            created_at: Utc::now(),
        };

        let args = serde_json::to_string(&server.args)?;

        self.db
            .conn()
            .execute(
                "INSERT INTO mcp_servers
                    (id, name, transport, command, args, url, enabled, auto_start, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8)",
                rusqlite::params![
                    server.id,
                    server.name,
                    server.transport.as_str(),
                    server.command,
                    args,
                    server.url,
                    server.auto_start,
                    server.created_at,
                ],
            )
            .map_err(|e| match e {
                // The name is unique because it is what a model proposes a call against and what
                // the allowlist is keyed by. Two servers called "linear" would make an enabled
                // tool ambiguous.
                rusqlite::Error::SqliteFailure(f, _)
                    if f.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    StorageError::Refused(format!(
                        "a server named '{}' already exists",
                        server.name
                    ))
                }
                other => other.into(),
            })?;

        Ok(server)
    }

    pub fn list(&self) -> Result<Vec<McpServer>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, transport, command, args, url, enabled, auto_start, created_at
               FROM mcp_servers ORDER BY name",
        )?;
        let rows = stmt.query_map([], map_server)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }

    pub fn get(&self, id: Id) -> Result<McpServer> {
        let conn = self.db.conn();
        conn.query_row(
            "SELECT id, name, transport, command, args, url, enabled, auto_start, created_at
               FROM mcp_servers WHERE id = ?1",
            rusqlite::params![id],
            map_server,
        )
        .optional()?
        .ok_or_else(|| StorageError::not_found("McpServer", id))?
    }

    /// Find by the name a model would use.
    pub fn by_name(&self, name: &str) -> Result<Option<McpServer>> {
        let conn = self.db.conn();
        conn.query_row(
            "SELECT id, name, transport, command, args, url, enabled, auto_start, created_at
               FROM mcp_servers WHERE name = ?1",
            rusqlite::params![name],
            map_server,
        )
        .optional()?
        .transpose()
    }

    pub fn set_enabled(&self, id: Id, enabled: bool) -> Result<McpServer> {
        let changed = self.db.conn().execute(
            "UPDATE mcp_servers SET enabled = ?2 WHERE id = ?1",
            rusqlite::params![id, enabled],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("McpServer", id));
        }
        self.get(id)
    }

    pub fn set_auto_start(&self, id: Id, auto_start: bool) -> Result<McpServer> {
        let changed = self.db.conn().execute(
            "UPDATE mcp_servers SET auto_start = ?2 WHERE id = ?1",
            rusqlite::params![id, auto_start],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("McpServer", id));
        }
        self.get(id)
    }

    /// Remove a server, its enabled tools, and its execution history.
    ///
    /// The history goes because `tool_executions.server_id` is `ON DELETE CASCADE` — a record
    /// naming a server that no longer exists could not be read back usefully. A user who wants to
    /// keep the history disables the server instead, which is why disabling exists.
    pub fn delete(&self, id: Id) -> Result<()> {
        let changed = self.db.conn().execute(
            "DELETE FROM mcp_servers WHERE id = ?1",
            rusqlite::params![id],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("McpServer", id));
        }
        Ok(())
    }

    /// Allow one tool on one server.
    pub fn enable_tool(&self, server_id: Id, tool: &str) -> Result<()> {
        if tool.trim().is_empty() {
            return Err(StorageError::Invalid {
                what: "tool name",
                reason: "a tool name cannot be blank".into(),
            });
        }
        self.db.conn().execute(
            "INSERT OR IGNORE INTO mcp_enabled_tools (server_id, tool_name) VALUES (?1, ?2)",
            rusqlite::params![server_id, tool],
        )?;
        Ok(())
    }

    pub fn disable_tool(&self, server_id: Id, tool: &str) -> Result<()> {
        self.db.conn().execute(
            "DELETE FROM mcp_enabled_tools WHERE server_id = ?1 AND tool_name = ?2",
            rusqlite::params![server_id, tool],
        )?;
        Ok(())
    }

    /// Which of a server's tools are enabled, whether or not the server itself is.
    ///
    /// For rendering the settings screen. Not for deciding whether a call may run — use
    /// [`Self::allowed_pairs`], which also requires the server to be enabled.
    pub fn enabled_tools(&self, server_id: Id) -> Result<Vec<String>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT tool_name FROM mcp_enabled_tools WHERE server_id = ?1 ORDER BY tool_name",
        )?;
        let rows = stmt.query_map(rusqlite::params![server_id], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every (server name, tool) pair that may actually be called.
    ///
    /// Keyed by name rather than id because that is what a model proposes and what the client
    /// checks. The join requires `enabled = 1`, so disabling a server withdraws all of its tools
    /// without touching their rows — the user's per-tool choices survive being turned off.
    pub fn allowed_pairs(&self) -> Result<Vec<(String, String)>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT s.name, t.tool_name
               FROM mcp_enabled_tools t
               JOIN mcp_servers s ON s.id = t.server_id
              WHERE s.enabled = 1
              ORDER BY s.name, t.tool_name",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The subset of the allowlist a scheduled job may propose from.
    ///
    /// Intersected with the global allowlist rather than read on its own: a job must not be able to
    /// widen what the user enabled. A job with no rows may propose nothing, which is the safe
    /// default for something that runs while nobody is watching.
    pub fn job_allowed_pairs(&self, job_id: Id) -> Result<Vec<(String, String)>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT s.name, j.tool_name
               FROM job_allowed_tools j
               JOIN mcp_servers s ON s.id = j.server_id
               JOIN mcp_enabled_tools t
                    ON t.server_id = j.server_id AND t.tool_name = j.tool_name
              WHERE j.job_id = ?1 AND s.enabled = 1
              ORDER BY s.name, j.tool_name",
        )?;
        let rows = stmt.query_map(rusqlite::params![job_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Replace a job's tool subset.
    pub fn set_job_allowed_tools(&self, job_id: Id, pairs: &[(Id, String)]) -> Result<()> {
        let conn = self.db.conn();
        // `unchecked_transaction` because the repository holds `&Database`, as every repository
        // here does. Replacing a job's subset is two statements and must not half-apply.
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM job_allowed_tools WHERE job_id = ?1",
            rusqlite::params![job_id],
        )?;
        for (server_id, tool) in pairs {
            tx.execute(
                "INSERT OR IGNORE INTO job_allowed_tools (job_id, server_id, tool_name)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![job_id, server_id, tool],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

// ---------------------------------------------------------------- executions

#[derive(Debug)]
pub struct ToolExecutionRepository<'a> {
    db: &'a Database,
}

impl<'a> ToolExecutionRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Record a proposal. Nothing has been sent and nothing will be until it is confirmed.
    pub fn propose(&self, new: NewToolExecution) -> Result<ToolExecution> {
        // Rejected here rather than at the wire: a row whose arguments are not JSON could never be
        // sent, and storing one would mean a proposal that can only ever fail sitting in the
        // confirmation queue looking valid.
        if serde_json::from_str::<serde_json::Value>(&new.arguments).is_err() {
            return Err(StorageError::Invalid {
                what: "tool arguments",
                reason: "the arguments must be JSON".into(),
            });
        }

        let execution = ToolExecution {
            id: Id::new(),
            action_item_id: new.action_item_id,
            server_id: new.server_id,
            tool_name: new.tool_name,
            arguments: new.arguments,
            status: ExecutionStatus::Proposed,
            result: None,
            proposed_at: Utc::now(),
            executed_at: None,
        };

        self.db.conn().execute(
            "INSERT INTO tool_executions
                (id, action_item_id, server_id, tool_name, arguments, status, proposed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                execution.id,
                execution.action_item_id,
                execution.server_id,
                execution.tool_name,
                execution.arguments,
                execution.status.as_str(),
                execution.proposed_at,
            ],
        )?;

        Ok(execution)
    }

    pub fn get(&self, id: Id) -> Result<ToolExecution> {
        let conn = self.db.conn();
        conn.query_row(
            "SELECT id, action_item_id, server_id, tool_name, arguments, status, result,
                    proposed_at, executed_at
               FROM tool_executions WHERE id = ?1",
            rusqlite::params![id],
            map_execution,
        )
        .optional()?
        .ok_or_else(|| StorageError::not_found("ToolExecution", id))?
    }

    /// Most recent first.
    pub fn list(&self, limit: usize) -> Result<Vec<ToolExecution>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, action_item_id, server_id, tool_name, arguments, status, result,
                    proposed_at, executed_at
               FROM tool_executions ORDER BY proposed_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit as i64], map_execution)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    /// Everything waiting on a human.
    pub fn awaiting_confirmation(&self) -> Result<Vec<ToolExecution>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, action_item_id, server_id, tool_name, arguments, status, result,
                    proposed_at, executed_at
               FROM tool_executions WHERE status = 'proposed' ORDER BY proposed_at DESC",
        )?;
        let rows = stmt.query_map([], map_execution)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    pub fn for_action_item(&self, action_item_id: Id) -> Result<Vec<ToolExecution>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, action_item_id, server_id, tool_name, arguments, status, result,
                    proposed_at, executed_at
               FROM tool_executions WHERE action_item_id = ?1 ORDER BY proposed_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![action_item_id], map_execution)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    /// A human approved it.
    ///
    /// Only from `proposed`, and the transition is a row update guarded by the current status —
    /// so two windows confirming the same proposal cannot both succeed, and a confirmation cannot
    /// be replayed against a call that already ran.
    pub fn confirm(&self, id: Id) -> Result<ToolExecution> {
        self.transition(id, ExecutionStatus::Proposed, ExecutionStatus::Confirmed)
    }

    /// A human declined. Nothing is sent, ever.
    pub fn reject(&self, id: Id) -> Result<ToolExecution> {
        self.transition(id, ExecutionStatus::Proposed, ExecutionStatus::Rejected)
    }

    /// Record how a confirmed call ended.
    ///
    /// Requires `confirmed`. There is deliberately no path from `proposed` to `succeeded`: if this
    /// could be reached without a confirmation, every other guarantee in this feature would be
    /// decoration.
    pub fn finish(&self, id: Id, outcome: Outcome) -> Result<ToolExecution> {
        let status = outcome.status();
        let changed = self.db.conn().execute(
            "UPDATE tool_executions
                SET status = ?3, result = ?4, executed_at = ?5
              WHERE id = ?1 AND status = ?2",
            rusqlite::params![
                id,
                ExecutionStatus::Confirmed.as_str(),
                status.as_str(),
                outcome.detail(),
                Utc::now(),
            ],
        )?;

        if changed == 0 {
            return Err(self.why_not(id, ExecutionStatus::Confirmed));
        }
        self.get(id)
    }

    fn transition(
        &self,
        id: Id,
        from: ExecutionStatus,
        to: ExecutionStatus,
    ) -> Result<ToolExecution> {
        let changed = self.db.conn().execute(
            "UPDATE tool_executions SET status = ?3 WHERE id = ?1 AND status = ?2",
            rusqlite::params![id, from.as_str(), to.as_str()],
        )?;

        if changed == 0 {
            return Err(self.why_not(id, from));
        }
        self.get(id)
    }

    /// Explain a transition that did not happen.
    ///
    /// The message names the status the row is actually in, because "cannot confirm" on its own
    /// sends the reader looking for a bug rather than at the other window where they already
    /// confirmed it.
    fn why_not(&self, id: Id, expected: ExecutionStatus) -> StorageError {
        match self.get(id) {
            Ok(current) => StorageError::Refused(format!(
                "this call is {}, not {} — nothing was sent",
                current.status.as_str(),
                expected.as_str()
            )),
            Err(e) => e,
        }
    }
}

fn map_server(row: &Row<'_>) -> rusqlite::Result<Result<McpServer>> {
    let raw_transport: String = row.get(2)?;
    let raw_args: String = row.get(4)?;

    let build = || -> Result<McpServer> {
        Ok(McpServer {
            id: row.get(0)?,
            name: row.get(1)?,
            transport: decode_enum("mcp_servers.transport", &raw_transport, McpTransport::parse)?,
            command: row.get(3)?,
            args: serde_json::from_str(&raw_args).map_err(|e| StorageError::Corrupt {
                column: "mcp_servers.args",
                reason: e.to_string(),
            })?,
            url: row.get(5)?,
            enabled: row.get(6)?,
            auto_start: row.get(7)?,
            created_at: row.get(8)?,
        })
    };

    Ok(build())
}

fn map_execution(row: &Row<'_>) -> rusqlite::Result<Result<ToolExecution>> {
    let raw_status: String = row.get(5)?;

    let build = || -> Result<ToolExecution> {
        Ok(ToolExecution {
            id: row.get(0)?,
            action_item_id: row.get(1)?,
            server_id: row.get(2)?,
            tool_name: row.get(3)?,
            arguments: row.get(4)?,
            status: decode_enum(
                "tool_executions.status",
                &raw_status,
                ExecutionStatus::parse,
            )?,
            result: row.get(6)?,
            proposed_at: row.get(7)?,
            executed_at: row.get(8)?,
        })
    };

    Ok(build())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().expect("an in-memory workspace")
    }

    fn server(db: &Database, name: &str) -> McpServer {
        McpServerRepository::new(db)
            .create(NewMcpServer {
                name: name.into(),
                transport: McpTransport::Stdio,
                command: Some("linear-mcp".into()),
                args: vec!["--stdio".into()],
                url: None,
                auto_start: true,
            })
            .expect("creates")
    }

    fn proposal(db: &Database, server_id: Id) -> ToolExecution {
        ToolExecutionRepository::new(db)
            .propose(NewToolExecution {
                action_item_id: None,
                server_id,
                tool_name: "create_issue".into(),
                arguments: r#"{"title":"Fix the importer"}"#.into(),
            })
            .expect("proposes")
    }

    /// Connecting a client must not grant capability as a side effect.
    #[test]
    fn a_new_server_is_disabled_and_reaches_nothing() {
        let db = db();
        let server = server(&db, "linear");

        assert!(!server.enabled);
        assert!(McpServerRepository::new(&db)
            .allowed_pairs()
            .expect("reads")
            .is_empty());
    }

    #[test]
    fn a_server_round_trips_with_its_arguments() {
        let db = db();
        let created = server(&db, "linear");
        let read = McpServerRepository::new(&db)
            .get(created.id)
            .expect("reads");

        assert_eq!(read, created);
        assert_eq!(read.args, vec!["--stdio".to_string()]);
        assert_eq!(read.transport, McpTransport::Stdio);
    }

    /// Two servers called "linear" would make an enabled tool ambiguous.
    #[test]
    fn two_servers_cannot_share_a_name() {
        let db = db();
        server(&db, "linear");

        let err = McpServerRepository::new(&db)
            .create(NewMcpServer {
                name: "linear".into(),
                transport: McpTransport::Http,
                command: None,
                url: Some("https://example.com/mcp".into()),
                args: vec![],
                auto_start: true,
            })
            .expect_err("must refuse");

        assert!(err.to_string().contains("linear"), "{err}");
    }

    /// Both switches, and both are needed.
    #[test]
    fn a_tool_is_reachable_only_when_its_server_is_enabled_too() {
        let db = db();
        let repo = McpServerRepository::new(&db);
        let server = server(&db, "linear");

        repo.enable_tool(server.id, "create_issue")
            .expect("enables");
        assert!(
            repo.allowed_pairs().expect("reads").is_empty(),
            "an enabled tool on a disabled server reaches nothing"
        );

        repo.set_enabled(server.id, true).expect("enables");
        assert_eq!(
            repo.allowed_pairs().expect("reads"),
            vec![("linear".to_string(), "create_issue".to_string())]
        );
    }

    /// Turning a server off must not lose the user's per-tool choices.
    #[test]
    fn disabling_a_server_withdraws_its_tools_without_forgetting_them() {
        let db = db();
        let repo = McpServerRepository::new(&db);
        let server = server(&db, "linear");
        repo.enable_tool(server.id, "create_issue")
            .expect("enables");
        repo.set_enabled(server.id, true).expect("enables");

        repo.set_enabled(server.id, false).expect("disables");
        assert!(repo.allowed_pairs().expect("reads").is_empty());
        assert_eq!(
            repo.enabled_tools(server.id).expect("reads"),
            vec!["create_issue".to_string()],
            "the choice survives being turned off"
        );
    }

    #[test]
    fn enabling_one_tool_does_not_enable_another() {
        let db = db();
        let repo = McpServerRepository::new(&db);
        let server = server(&db, "linear");
        repo.enable_tool(server.id, "create_issue")
            .expect("enables");
        repo.set_enabled(server.id, true).expect("enables");

        let pairs = repo.allowed_pairs().expect("reads");
        assert_eq!(pairs.len(), 1);
        assert!(!pairs.iter().any(|(_, tool)| tool == "delete_issue"));
    }

    #[test]
    fn enabling_the_same_tool_twice_is_not_an_error() {
        let db = db();
        let repo = McpServerRepository::new(&db);
        let server = server(&db, "linear");

        repo.enable_tool(server.id, "create_issue")
            .expect("enables");
        repo.enable_tool(server.id, "create_issue").expect("again");
        assert_eq!(repo.enabled_tools(server.id).expect("reads").len(), 1);
    }

    #[test]
    fn disabling_a_tool_removes_it_from_the_allowlist() {
        let db = db();
        let repo = McpServerRepository::new(&db);
        let server = server(&db, "linear");
        repo.enable_tool(server.id, "create_issue")
            .expect("enables");
        repo.set_enabled(server.id, true).expect("enables");

        repo.disable_tool(server.id, "create_issue")
            .expect("removes");
        assert!(repo.allowed_pairs().expect("reads").is_empty());
    }

    #[test]
    fn a_blank_tool_name_is_refused() {
        let db = db();
        let server = server(&db, "linear");
        assert!(McpServerRepository::new(&db)
            .enable_tool(server.id, "  ")
            .is_err());
    }

    #[test]
    fn a_server_can_be_found_by_the_name_a_model_would_use() {
        let db = db();
        let created = server(&db, "linear");
        let repo = McpServerRepository::new(&db);

        assert_eq!(
            repo.by_name("linear").expect("reads").map(|s| s.id),
            Some(created.id)
        );
        assert!(repo.by_name("jira").expect("reads").is_none());
    }

    #[test]
    fn deleting_a_server_takes_its_enabled_tools_with_it() {
        let db = db();
        let repo = McpServerRepository::new(&db);
        let server = server(&db, "linear");
        repo.enable_tool(server.id, "create_issue")
            .expect("enables");

        repo.delete(server.id).expect("deletes");
        assert!(repo.enabled_tools(server.id).expect("reads").is_empty());
        assert!(repo.get(server.id).is_err());
    }

    // ------------------------------------------------------------ the state machine

    #[test]
    fn a_proposal_starts_with_nothing_sent() {
        let db = db();
        let server = server(&db, "linear");
        let execution = proposal(&db, server.id);

        assert_eq!(execution.status, ExecutionStatus::Proposed);
        assert!(execution.executed_at.is_none());
        assert!(execution.result.is_none());
        assert!(!execution.status.is_final());
    }

    #[test]
    fn the_arguments_come_back_exactly_as_they_went_in() {
        let db = db();
        let server = server(&db, "linear");
        let execution = proposal(&db, server.id);

        let read = ToolExecutionRepository::new(&db)
            .get(execution.id)
            .expect("reads");
        assert_eq!(read.arguments, r#"{"title":"Fix the importer"}"#);
        assert_eq!(
            read.arguments_value().expect("parses")["title"],
            "Fix the importer"
        );
    }

    /// A proposal that could only ever fail must not sit in the queue looking valid.
    #[test]
    fn arguments_that_are_not_json_are_refused_at_the_door() {
        let db = db();
        let server = server(&db, "linear");

        let err = ToolExecutionRepository::new(&db)
            .propose(NewToolExecution {
                action_item_id: None,
                server_id: server.id,
                tool_name: "create_issue".into(),
                arguments: "not json".into(),
            })
            .expect_err("must refuse");
        assert!(err.to_string().contains("JSON"), "{err}");
    }

    #[test]
    fn the_happy_path_is_proposed_then_confirmed_then_succeeded() {
        let db = db();
        let server = server(&db, "linear");
        let repo = ToolExecutionRepository::new(&db);
        let execution = proposal(&db, server.id);

        let confirmed = repo.confirm(execution.id).expect("confirms");
        assert_eq!(confirmed.status, ExecutionStatus::Confirmed);
        assert!(confirmed.executed_at.is_none(), "confirming is not sending");

        let done = repo
            .finish(
                execution.id,
                Outcome::Succeeded {
                    result: r#"{"id":"ENG-1"}"#.into(),
                },
            )
            .expect("finishes");
        assert_eq!(done.status, ExecutionStatus::Succeeded);
        assert!(done.executed_at.is_some());
        assert_eq!(done.result.as_deref(), Some(r#"{"id":"ENG-1"}"#));
    }

    /// If this were reachable, every other guarantee here would be decoration.
    #[test]
    fn there_is_no_path_from_proposed_to_succeeded() {
        let db = db();
        let server = server(&db, "linear");
        let repo = ToolExecutionRepository::new(&db);
        let execution = proposal(&db, server.id);

        let err = repo
            .finish(
                execution.id,
                Outcome::Succeeded {
                    result: "{}".into(),
                },
            )
            .expect_err("must refuse");

        assert!(err.to_string().contains("proposed"), "{err}");
        assert_eq!(
            repo.get(execution.id).expect("reads").status,
            ExecutionStatus::Proposed,
            "and the row is untouched"
        );
    }

    #[test]
    fn a_rejection_sends_nothing_and_is_the_end() {
        let db = db();
        let server = server(&db, "linear");
        let repo = ToolExecutionRepository::new(&db);
        let execution = proposal(&db, server.id);

        let rejected = repo.reject(execution.id).expect("rejects");
        assert_eq!(rejected.status, ExecutionStatus::Rejected);
        assert!(rejected.executed_at.is_none());
        assert!(rejected.status.is_final());

        assert!(
            repo.confirm(execution.id).is_err(),
            "a rejected call cannot be confirmed afterwards"
        );
    }

    /// Two windows confirming the same proposal must not both succeed.
    #[test]
    fn a_confirmation_cannot_be_replayed() {
        let db = db();
        let server = server(&db, "linear");
        let repo = ToolExecutionRepository::new(&db);
        let execution = proposal(&db, server.id);

        repo.confirm(execution.id).expect("confirms");
        let err = repo.confirm(execution.id).expect_err("must refuse");
        assert!(err.to_string().contains("confirmed"), "{err}");
    }

    /// No automatic retry, and no manual one either: a failed call may have taken effect.
    #[test]
    fn a_finished_call_cannot_be_finished_again() {
        let db = db();
        let server = server(&db, "linear");
        let repo = ToolExecutionRepository::new(&db);
        let execution = proposal(&db, server.id);

        repo.confirm(execution.id).expect("confirms");
        repo.finish(
            execution.id,
            Outcome::Failed {
                error: "the server said no".into(),
            },
        )
        .expect("finishes");

        assert!(repo
            .finish(
                execution.id,
                Outcome::Succeeded {
                    result: "{}".into()
                }
            )
            .is_err());
    }

    /// The distinction the user acts on.
    #[test]
    fn a_timeout_is_recorded_as_unknown_rather_than_failed() {
        let db = db();
        let server = server(&db, "linear");
        let repo = ToolExecutionRepository::new(&db);
        let execution = proposal(&db, server.id);
        repo.confirm(execution.id).expect("confirms");

        let done = repo
            .finish(
                execution.id,
                Outcome::Unknown {
                    detail: "no answer in 60s".into(),
                },
            )
            .expect("finishes");

        assert_eq!(done.status, ExecutionStatus::Unknown);
        assert_ne!(
            done.status,
            ExecutionStatus::Failed,
            "telling a user it failed when it may have run is how a ticket gets filed twice"
        );
        assert!(done.status.is_final());
    }

    #[test]
    fn the_queue_holds_only_what_is_waiting_on_a_human() {
        let db = db();
        let server = server(&db, "linear");
        let repo = ToolExecutionRepository::new(&db);

        let waiting = proposal(&db, server.id);
        let answered = proposal(&db, server.id);
        repo.reject(answered.id).expect("rejects");

        let queue = repo.awaiting_confirmation().expect("reads");
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].id, waiting.id);
    }

    /// Deleting the task must not erase the record that something was done on its behalf.
    #[test]
    fn deleting_an_action_item_keeps_the_execution_record() {
        let db = db();
        let server = server(&db, "linear");

        // A summary and an action item to hang the execution off.
        let meeting = crate::MeetingRepository::new(&db)
            .create(crate::NewMeeting {
                title: "Planning".into(),
                started_at: Utc::now(),
                source: crate::MeetingSource::Import,
                project_id: None,
            })
            .expect("creates a meeting");

        let summaries = crate::SummaryRepository::new(&db);
        let summary = summaries
            .create(crate::NewSummary {
                meeting_id: meeting.id,
                text: "we agreed".into(),
                model: "test".into(),
                template_id: None,
            })
            .expect("creates a summary");
        let item = summaries
            .add_action_item(crate::NewActionItem {
                meeting_id: meeting.id,
                summary_id: Some(summary.id),
                text: "File a ticket".into(),
                owner: None,
                owner_person_id: None,
                due_at: None,
            })
            .expect("creates an action item");

        let repo = ToolExecutionRepository::new(&db);
        let execution = repo
            .propose(NewToolExecution {
                action_item_id: Some(item.id),
                server_id: server.id,
                tool_name: "create_issue".into(),
                arguments: "{}".into(),
            })
            .expect("proposes");

        assert_eq!(
            repo.for_action_item(item.id).expect("reads").len(),
            1,
            "and it is findable from the task"
        );

        summaries.delete_action_item(item.id).expect("deletes");

        let kept = repo.get(execution.id).expect("the record survives");
        assert!(
            kept.action_item_id.is_none(),
            "the link is cleared, not the row"
        );
    }

    #[test]
    fn history_is_most_recent_first() {
        let db = db();
        let server = server(&db, "linear");
        let repo = ToolExecutionRepository::new(&db);

        let first = proposal(&db, server.id);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = proposal(&db, server.id);

        let listed = repo.list(10).expect("reads");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, second.id);
        assert_eq!(listed[1].id, first.id);
    }

    // ------------------------------------------------------------ job subsets

    /// A job must not be able to widen what the user enabled.
    #[test]
    fn a_job_cannot_be_scoped_to_a_tool_the_user_has_not_enabled() {
        let db = db();
        let repo = McpServerRepository::new(&db);
        let server = server(&db, "linear");
        repo.set_enabled(server.id, true).expect("enables");

        let job = crate::JobRepository::new(&db)
            .create(crate::NewJob {
                name: "morning".into(),
                prompt: "look for work".into(),
                cron: "0 8 * * *".into(),
                timezone: "UTC".into(),
                catch_up: false,
                timeout_secs: 300,
            })
            .expect("creates a job");

        repo.set_job_allowed_tools(job.id, &[(server.id, "create_issue".into())])
            .expect("scopes the job");

        assert!(
            repo.job_allowed_pairs(job.id).expect("reads").is_empty(),
            "the tool is not globally enabled, so the job may not propose it"
        );

        repo.enable_tool(server.id, "create_issue")
            .expect("enables");
        assert_eq!(
            repo.job_allowed_pairs(job.id).expect("reads"),
            vec![("linear".to_string(), "create_issue".to_string())]
        );
    }

    /// The safe default for something that runs while nobody is watching.
    #[test]
    fn a_job_with_no_subset_may_propose_nothing() {
        let db = db();
        let repo = McpServerRepository::new(&db);
        let server = server(&db, "linear");
        repo.set_enabled(server.id, true).expect("enables");
        repo.enable_tool(server.id, "create_issue")
            .expect("enables");

        let job = crate::JobRepository::new(&db)
            .create(crate::NewJob {
                name: "morning".into(),
                prompt: "look for work".into(),
                cron: "0 8 * * *".into(),
                timezone: "UTC".into(),
                catch_up: false,
                timeout_secs: 300,
            })
            .expect("creates a job");

        assert!(repo.job_allowed_pairs(job.id).expect("reads").is_empty());
    }
}
