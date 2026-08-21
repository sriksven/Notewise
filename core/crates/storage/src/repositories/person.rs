use chrono::Utc;
use rusqlite::Row;

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::id::Id;
use crate::models::Person;

#[derive(Debug, Clone)]
pub struct NewPerson {
    pub display_name: String,
    pub email: Option<String>,
}

/// An enrolled voiceprint, paired with the model that produced it.
///
/// The model identity travels with the vector because cosine distance between embeddings
/// from different models is meaningless and the bytes do not say which model made them.
/// A matcher that ignores this field will produce confident wrong names on real people.
#[derive(Debug, Clone, PartialEq)]
pub struct VoicePrint {
    pub person_id: Id,
    pub model: String,
    pub vector: Vec<f32>,
}

#[derive(Debug)]
pub struct PersonRepository<'a> {
    db: &'a Database,
}

impl<'a> PersonRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn create(&self, new: NewPerson) -> Result<Person> {
        let now = Utc::now();
        let person = Person {
            id: Id::new(),
            display_name: new.display_name,
            email: new.email,
            has_voice_print: false,
            created_at: now,
            updated_at: now,
        };

        self.db.conn().execute(
            "INSERT INTO people (id, display_name, email, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                person.id,
                person.display_name,
                person.email,
                person.created_at,
                person.updated_at
            ],
        )?;

        Ok(person)
    }

    pub fn get(&self, id: Id) -> Result<Person> {
        self.db
            .conn()
            .query_row(SELECT_PERSON_BY_ID, rusqlite::params![id], map_person)
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => StorageError::not_found("Person", id),
                other => other.into(),
            })
    }

    /// Look a person up by display name, creating them if absent.
    ///
    /// Names are matched case-insensitively but stored as first given. This is how a speaker
    /// label or a transcript-extracted owner becomes an identity without asking the user to
    /// pre-register everyone they will ever meet.
    ///
    /// Names are not unique in reality, so this will merge two different people who share
    /// one. That is the right trade for attribution — a wrongly merged pair is visible and
    /// fixable, whereas a proliferation of near-duplicate rows is neither — but it means
    /// callers with a real identifier should use [`Self::find_by_email`] first.
    pub fn find_or_create_by_name(&self, display_name: &str) -> Result<Person> {
        let existing: Option<Person> = self
            .db
            .conn()
            .query_row(
                "SELECT id, display_name, email, voice_print IS NOT NULL, created_at, updated_at
                 FROM people WHERE display_name = ?1 COLLATE NOCASE",
                rusqlite::params![display_name],
                map_person,
            )
            .ok();

        match existing {
            Some(person) => Ok(person),
            None => self.create(NewPerson {
                display_name: display_name.to_string(),
                email: None,
            }),
        }
    }

    pub fn find_by_email(&self, email: &str) -> Result<Option<Person>> {
        Ok(self
            .db
            .conn()
            .query_row(
                "SELECT id, display_name, email, voice_print IS NOT NULL, created_at, updated_at
                 FROM people WHERE email = ?1 COLLATE NOCASE",
                rusqlite::params![email],
                map_person,
            )
            .ok())
    }

    pub fn list(&self) -> Result<Vec<Person>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, display_name, email, voice_print IS NOT NULL, created_at, updated_at
             FROM people ORDER BY display_name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], map_person)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn rename(&self, id: Id, display_name: &str) -> Result<Person> {
        let changed = self.db.conn().execute(
            "UPDATE people SET display_name = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, display_name, Utc::now()],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("Person", id));
        }
        self.get(id)
    }

    pub fn delete(&self, id: Id) -> Result<()> {
        let changed = self
            .db
            .conn()
            .execute("DELETE FROM people WHERE id = ?1", rusqlite::params![id])?;
        if changed == 0 {
            return Err(StorageError::not_found("Person", id));
        }
        Ok(())
    }

    /// Record a voiceprint for a person.
    ///
    /// **Not currently called by anything, and that is deliberate.** A speaker embedding is a
    /// biometric identifier for someone who is usually not the user of this machine and who
    /// never agreed to be enrolled. Wiring this up is gated on an explicit consent and
    /// encryption-at-rest decision, not on the storage existing. The capability lives here so
    /// that decision does not also cost a schema migration.
    pub fn set_voice_print(&self, id: Id, vector: &[f32], model: &str) -> Result<()> {
        if vector.is_empty() {
            return Err(StorageError::Invalid {
                what: "voice print",
                reason: "refusing to store an empty embedding".into(),
            });
        }

        let changed = self.db.conn().execute(
            "UPDATE people
                SET voice_print = ?2, voice_dims = ?3, voice_model = ?4, updated_at = ?5
              WHERE id = ?1",
            rusqlite::params![
                id,
                encode_vector(vector),
                vector.len() as i64,
                model,
                Utc::now()
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("Person", id));
        }
        Ok(())
    }

    pub fn clear_voice_print(&self, id: Id) -> Result<()> {
        let changed = self.db.conn().execute(
            "UPDATE people
                SET voice_print = NULL, voice_dims = NULL, voice_model = NULL, updated_at = ?2
              WHERE id = ?1",
            rusqlite::params![id, Utc::now()],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("Person", id));
        }
        Ok(())
    }

    /// Every enrolled voiceprint, for a matcher to compare against.
    ///
    /// Returns vectors rather than doing the matching: nearest-neighbour over embeddings is
    /// not a SQL query, and the metric (cosine over L2-normalised vectors) belongs to
    /// `diarization::cluster`. Implementing it here would either duplicate that metric, where
    /// it would drift, or make `storage` depend on `diarization` — which inverts the
    /// dependency rule. Callers filter by `model` before comparing.
    pub fn voice_prints(&self) -> Result<Vec<VoicePrint>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, voice_model, voice_print FROM people
             WHERE voice_print IS NOT NULL AND voice_model IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, Id>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|(person_id, model, blob)| {
                Ok(VoicePrint {
                    person_id,
                    model,
                    vector: decode_vector(&blob)?,
                })
            })
            .collect()
    }

    /// Link a person to a meeting they took part in.
    ///
    /// Idempotent: re-adding the same person is not an error, because the callers that
    /// discover participants — a transcript pass, a calendar import — legitimately see the
    /// same person more than once.
    pub fn add_participant(&self, meeting_id: Id, person_id: Id, role: Option<&str>) -> Result<()> {
        self.db.conn().execute(
            "INSERT INTO meeting_participants (meeting_id, person_id, role)
             VALUES (?1, ?2, ?3)
             ON CONFLICT (meeting_id, person_id) DO UPDATE SET role = COALESCE(?3, role)",
            rusqlite::params![meeting_id, person_id, role],
        )?;
        Ok(())
    }

    pub fn participants(&self, meeting_id: Id) -> Result<Vec<Person>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT p.id, p.display_name, p.email, p.voice_print IS NOT NULL,
                    p.created_at, p.updated_at
               FROM people p
               JOIN meeting_participants mp ON mp.person_id = p.id
              WHERE mp.meeting_id = ?1
              ORDER BY p.display_name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map(rusqlite::params![meeting_id], map_person)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Meeting ids a person took part in, most recent first.
    pub fn meeting_ids_for_person(&self, person_id: Id) -> Result<Vec<Id>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT m.id FROM meetings m
               JOIN meeting_participants mp ON mp.meeting_id = m.id
              WHERE mp.person_id = ?1
              ORDER BY m.started_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![person_id], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

const SELECT_PERSON_BY_ID: &str =
    "SELECT id, display_name, email, voice_print IS NOT NULL, created_at, updated_at
     FROM people WHERE id = ?1";

fn map_person(row: &Row<'_>) -> rusqlite::Result<Person> {
    Ok(Person {
        id: row.get(0)?,
        display_name: row.get(1)?,
        email: row.get(2)?,
        has_voice_print: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn encode_vector(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Little-endian f32s. Fixed by this function rather than by the host, so a database copied
/// between machines of different endianness still reads correctly.
fn decode_vector(blob: &[u8]) -> Result<Vec<f32>> {
    if !blob.len().is_multiple_of(4) {
        return Err(StorageError::Corrupt {
            column: "people.voice_print",
            reason: format!("{} bytes is not a whole number of f32s", blob.len()),
        });
    }
    Ok(blob
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::MeetingSource;
    use crate::repositories::{MeetingRepository, NewMeeting};

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn person(db: &Database, name: &str) -> Person {
        PersonRepository::new(db)
            .create(NewPerson {
                display_name: name.into(),
                email: None,
            })
            .unwrap()
    }

    #[test]
    fn a_new_person_has_no_voice_print() {
        let db = db();
        assert!(!person(&db, "Priya").has_voice_print);
    }

    #[test]
    fn find_or_create_by_name_is_case_insensitive_and_does_not_duplicate() {
        let db = db();
        let repo = PersonRepository::new(&db);

        let first = repo.find_or_create_by_name("Priya Raman").unwrap();
        let again = repo.find_or_create_by_name("priya raman").unwrap();

        assert_eq!(first.id, again.id, "must not create a second row");
        assert_eq!(
            again.display_name, "Priya Raman",
            "the name as first given is kept"
        );
        assert_eq!(repo.list().unwrap().len(), 1);
    }

    #[test]
    fn a_voice_print_round_trips_with_its_model() {
        let db = db();
        let repo = PersonRepository::new(&db);
        let p = person(&db, "Priya");

        repo.set_voice_print(p.id, &[0.5, -0.25, 0.125], "cam++/v1")
            .unwrap();

        let prints = repo.voice_prints().unwrap();
        assert_eq!(prints.len(), 1);
        assert_eq!(prints[0].person_id, p.id);
        assert_eq!(prints[0].model, "cam++/v1");
        assert_eq!(prints[0].vector, vec![0.5, -0.25, 0.125]);
        assert!(repo.get(p.id).unwrap().has_voice_print);
    }

    #[test]
    fn an_unenrolled_person_is_absent_from_voice_prints() {
        let db = db();
        let repo = PersonRepository::new(&db);
        person(&db, "Never enrolled");
        assert!(repo.voice_prints().unwrap().is_empty());
    }

    #[test]
    fn clearing_a_voice_print_removes_it_entirely() {
        let db = db();
        let repo = PersonRepository::new(&db);
        let p = person(&db, "Priya");
        repo.set_voice_print(p.id, &[1.0, 2.0], "cam++/v1").unwrap();

        repo.clear_voice_print(p.id).unwrap();

        assert!(repo.voice_prints().unwrap().is_empty());
        assert!(!repo.get(p.id).unwrap().has_voice_print);
    }

    #[test]
    fn an_empty_embedding_is_refused() {
        let db = db();
        let repo = PersonRepository::new(&db);
        let p = person(&db, "Priya");

        let err = repo
            .set_voice_print(p.id, &[], "cam++/v1")
            .expect_err("an empty vector matches everything and means nothing");
        assert!(matches!(err, StorageError::Invalid { .. }), "got {err:?}");
    }

    #[test]
    fn a_truncated_voice_print_is_reported_not_panicked() {
        let db = db();
        let p = person(&db, "Priya");
        // Three bytes cannot be whole f32s. Simulates a corrupted or truncated write.
        db.conn()
            .execute(
                "UPDATE people SET voice_print = ?2, voice_model = 'cam++/v1' WHERE id = ?1",
                rusqlite::params![p.id, vec![1u8, 2, 3]],
            )
            .unwrap();

        let err = PersonRepository::new(&db)
            .voice_prints()
            .expect_err("should report corruption");
        assert!(matches!(err, StorageError::Corrupt { .. }), "got {err:?}");
    }

    #[test]
    fn adding_the_same_participant_twice_is_not_an_error() {
        let db = db();
        let repo = PersonRepository::new(&db);
        let p = person(&db, "Priya");
        let meeting = MeetingRepository::new(&db)
            .create(NewMeeting {
                project_id: None,
                title: "Standup".into(),
                source: MeetingSource::Import,
                started_at: Utc::now(),
            })
            .unwrap();

        repo.add_participant(meeting.id, p.id, Some("host"))
            .unwrap();
        repo.add_participant(meeting.id, p.id, None).unwrap();

        let participants = repo.participants(meeting.id).unwrap();
        assert_eq!(participants.len(), 1);
        assert_eq!(participants[0].id, p.id);
    }

    #[test]
    fn re_adding_a_participant_without_a_role_keeps_the_known_one() {
        let db = db();
        let repo = PersonRepository::new(&db);
        let p = person(&db, "Priya");
        let meeting = MeetingRepository::new(&db)
            .create(NewMeeting {
                project_id: None,
                title: "Standup".into(),
                source: MeetingSource::Import,
                started_at: Utc::now(),
            })
            .unwrap();

        repo.add_participant(meeting.id, p.id, Some("host"))
            .unwrap();
        repo.add_participant(meeting.id, p.id, None).unwrap();

        let role: Option<String> = db
            .conn()
            .query_row(
                "SELECT role FROM meeting_participants WHERE meeting_id = ?1 AND person_id = ?2",
                rusqlite::params![meeting.id, p.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            role.as_deref(),
            Some("host"),
            "a later sighting without a role must not erase a known one"
        );
    }

    #[test]
    fn deleting_a_person_removes_their_participation() {
        let db = db();
        let repo = PersonRepository::new(&db);
        let p = person(&db, "Priya");
        let meeting = MeetingRepository::new(&db)
            .create(NewMeeting {
                project_id: None,
                title: "Standup".into(),
                source: MeetingSource::Import,
                started_at: Utc::now(),
            })
            .unwrap();
        repo.add_participant(meeting.id, p.id, None).unwrap();

        repo.delete(p.id).unwrap();

        assert!(repo.participants(meeting.id).unwrap().is_empty());
    }
}
