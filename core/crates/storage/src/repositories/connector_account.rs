//! Per-connector account state.
//!
//! Deliberately holds **no tokens**. Credentials live in the OS keychain, addressed by
//! `connector_id`; a refresh token in this file would travel with any copy of the database,
//! including one attached to a bug report.

use chrono::{DateTime, Utc};

use crate::db::Database;
use crate::error::Result;
use crate::repositories::decode_enum;

/// Whether a connector can currently be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountStatus {
    Connected,
    /// Credentials were rejected. Retrying cannot fix this; the user must reconnect.
    NeedsReauth,
    /// Connected but paused by the user.
    Disabled,
}

impl AccountStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AccountStatus::Connected => "connected",
            AccountStatus::NeedsReauth => "needs_reauth",
            AccountStatus::Disabled => "disabled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "connected" => Some(AccountStatus::Connected),
            "needs_reauth" => Some(AccountStatus::NeedsReauth),
            "disabled" => Some(AccountStatus::Disabled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorAccount {
    pub connector_id: String,
    pub account_label: Option<String>,
    pub scopes: Vec<String>,
    pub status: AccountStatus,
    pub connected_at: DateTime<Utc>,
    pub cursor: Option<String>,
}

#[derive(Debug)]
pub struct ConnectorAccountRepository<'a> {
    db: &'a Database,
}

impl<'a> ConnectorAccountRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Record a connected account, replacing any previous one for this connector.
    pub fn connect(
        &self,
        connector_id: &str,
        account_label: Option<&str>,
        scopes: &[String],
    ) -> Result<()> {
        self.db.conn().execute(
            "INSERT INTO connector_accounts
                (connector_id, account_label, scopes, status, connected_at, cursor)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)
             ON CONFLICT(connector_id) DO UPDATE SET
                account_label = excluded.account_label,
                scopes        = excluded.scopes,
                status        = excluded.status,
                connected_at  = excluded.connected_at",
            rusqlite::params![
                connector_id,
                account_label,
                scopes.join(" "),
                AccountStatus::Connected.as_str(),
                Utc::now()
            ],
        )?;
        Ok(())
    }

    pub fn get(&self, connector_id: &str) -> Result<Option<ConnectorAccount>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT connector_id, account_label, scopes, status, connected_at, cursor
             FROM connector_accounts WHERE connector_id = ?1",
        )?;

        let mut rows = stmt.query(rusqlite::params![connector_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };

        let raw_status: String = row.get(3)?;
        let raw_scopes: String = row.get(2)?;

        Ok(Some(ConnectorAccount {
            connector_id: row.get(0)?,
            account_label: row.get(1)?,
            scopes: raw_scopes.split_whitespace().map(str::to_string).collect(),
            status: decode_enum("status", &raw_status, AccountStatus::parse)?,
            connected_at: row.get(4)?,
            cursor: row.get(5)?,
        }))
    }

    pub fn list(&self) -> Result<Vec<ConnectorAccount>> {
        let conn = self.db.conn();
        let mut stmt =
            conn.prepare("SELECT connector_id FROM connector_accounts ORDER BY connector_id")?;
        let ids: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<_, _>>()?;
        drop(stmt);

        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(account) = self.get(&id)? {
                out.push(account);
            }
        }
        Ok(out)
    }

    pub fn set_status(&self, connector_id: &str, status: AccountStatus) -> Result<()> {
        self.db.conn().execute(
            "UPDATE connector_accounts SET status = ?2 WHERE connector_id = ?1",
            rusqlite::params![connector_id, status.as_str()],
        )?;
        Ok(())
    }

    pub fn set_cursor(&self, connector_id: &str, cursor: Option<&str>) -> Result<()> {
        self.db.conn().execute(
            "UPDATE connector_accounts SET cursor = ?2 WHERE connector_id = ?1",
            rusqlite::params![connector_id, cursor],
        )?;
        Ok(())
    }

    /// Remove the account. Removing an absent account succeeds.
    pub fn disconnect(&self, connector_id: &str) -> Result<()> {
        self.db.conn().execute(
            "DELETE FROM connector_accounts WHERE connector_id = ?1",
            rusqlite::params![connector_id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory database")
    }

    #[test]
    fn connect_then_read_back() {
        let db = db();
        let repo = ConnectorAccountRepository::new(&db);

        repo.connect("vault", Some("~/Notes"), &["write".into()])
            .unwrap();

        let account = repo.get("vault").unwrap().expect("just connected");
        assert_eq!(account.account_label.as_deref(), Some("~/Notes"));
        assert_eq!(account.status, AccountStatus::Connected);
        assert!(account.cursor.is_none());
    }

    #[test]
    fn needs_reauth_survives_a_read() {
        let db = db();
        let repo = ConnectorAccountRepository::new(&db);

        repo.connect("google_calendar", Some("a@b.com"), &["ro".into()])
            .unwrap();
        repo.set_status("google_calendar", AccountStatus::NeedsReauth)
            .unwrap();

        let account = repo.get("google_calendar").unwrap().expect("connected");
        assert_eq!(account.status, AccountStatus::NeedsReauth);
    }

    #[test]
    fn cursor_advances_independently_of_status() {
        let db = db();
        let repo = ConnectorAccountRepository::new(&db);

        repo.connect("google_calendar", None, &[]).unwrap();
        repo.set_cursor("google_calendar", Some("page-2")).unwrap();

        let account = repo.get("google_calendar").unwrap().expect("connected");
        assert_eq!(account.cursor.as_deref(), Some("page-2"));
        assert_eq!(account.status, AccountStatus::Connected);
    }

    #[test]
    fn disconnect_removes_the_account() {
        let db = db();
        let repo = ConnectorAccountRepository::new(&db);

        repo.connect("vault", None, &[]).unwrap();
        repo.disconnect("vault").unwrap();

        assert!(repo.get("vault").unwrap().is_none());
    }

    #[test]
    fn unknown_status_in_the_database_is_an_error_not_a_panic() {
        let db = db();
        db.conn()
            .execute(
                "INSERT INTO connector_accounts
                    (connector_id, account_label, scopes, status, connected_at)
                 VALUES ('weird', NULL, '', 'ascended', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();

        let repo = ConnectorAccountRepository::new(&db);
        assert!(repo.get("weird").is_err());
    }
}
