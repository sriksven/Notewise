use chrono::Utc;

use crate::db::Database;
use crate::error::Result;

/// Application-level key/value configuration.
///
/// Deliberately untyped: values are strings and the caller owns the meaning. A typed column
/// per setting would mean a migration every time the app grows a preference, for no benefit —
/// nothing joins against these.
#[derive(Debug)]
pub struct SettingsRepository<'a> {
    db: &'a Database,
}

impl<'a> SettingsRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// The value for `key`, or `None` when it has never been set.
    pub fn get(&self, key: &str) -> Result<Option<String>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare("SELECT value FROM app_settings WHERE key = ?1")?;
        let mut rows = stmt.query(rusqlite::params![key])?;

        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Insert or overwrite `key`.
    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.db.conn().execute(
            "INSERT INTO app_settings (key, value, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                                            updated_at = excluded.updated_at",
            rusqlite::params![key, value, Utc::now()],
        )?;
        Ok(())
    }

    /// Remove `key`. Removing an absent key succeeds.
    pub fn delete(&self, key: &str) -> Result<()> {
        self.db.conn().execute(
            "DELETE FROM app_settings WHERE key = ?1",
            rusqlite::params![key],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Database;
    use crate::repositories::SettingsRepository;

    #[test]
    fn absent_key_reads_as_none() {
        let db = Database::open_in_memory().unwrap();
        let settings = SettingsRepository::new(&db);
        assert_eq!(settings.get("onboarding_completed_at").unwrap(), None);
    }

    #[test]
    fn a_value_survives_a_round_trip() {
        let db = Database::open_in_memory().unwrap();
        let settings = SettingsRepository::new(&db);

        settings
            .set("onboarding_completed_at", "2026-08-13T10:00:00Z")
            .unwrap();

        assert_eq!(
            settings.get("onboarding_completed_at").unwrap().as_deref(),
            Some("2026-08-13T10:00:00Z")
        );
    }

    /// `set` is an upsert. Onboarding writes the same key on every completion attempt, and a
    /// UNIQUE violation there would turn a harmless retry into an error.
    #[test]
    fn setting_an_existing_key_overwrites_it() {
        let db = Database::open_in_memory().unwrap();
        let settings = SettingsRepository::new(&db);

        settings.set("k", "first").unwrap();
        settings.set("k", "second").unwrap();

        assert_eq!(settings.get("k").unwrap().as_deref(), Some("second"));
    }

    #[test]
    fn a_deleted_key_reads_as_none_again() {
        let db = Database::open_in_memory().unwrap();
        let settings = SettingsRepository::new(&db);

        settings.set("k", "v").unwrap();
        settings.delete("k").unwrap();

        assert_eq!(settings.get("k").unwrap(), None);
    }
}
