use std::fmt;
use std::str::FromStr;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identifier for every stored entity.
///
/// Stored as a hyphenated UUID string rather than a blob so that databases stay readable
/// with standard SQLite tooling — worth more during debugging than the 20 bytes saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(Uuid);

impl Id {
    /// Generate a fresh random identifier.
    pub fn new() -> Self {
        Id(Uuid::new_v4())
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }

    pub fn from_uuid(uuid: Uuid) -> Self {
        Id(uuid)
    }
}

impl Default for Id {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.hyphenated())
    }
}

impl FromStr for Id {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Id)
    }
}

impl ToSql for Id {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.0.hyphenated().to_string()))
    }
}

impl FromSql for Id {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let text = value.as_str()?;
        Uuid::parse_str(text)
            .map(Id)
            .map_err(|e| FromSqlError::Other(Box::new(e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        assert_ne!(Id::new(), Id::new());
    }

    #[test]
    fn round_trips_through_string() {
        let id = Id::new();
        let parsed: Id = id.to_string().parse().expect("should parse back");
        assert_eq!(id, parsed);
    }

    #[test]
    fn displays_as_hyphenated_uuid() {
        let id = Id::new();
        let s = id.to_string();
        assert_eq!(s.len(), 36, "hyphenated uuid is 36 chars, got {s}");
        assert_eq!(s.matches('-').count(), 4);
    }

    #[test]
    fn rejects_non_uuid_text() {
        assert!("not-a-uuid".parse::<Id>().is_err());
    }
}
