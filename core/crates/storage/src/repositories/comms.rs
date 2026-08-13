use chrono::{DateTime, Utc};
use rusqlite::Row;

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::id::Id;
use crate::models::{DraftStatus, EmailDraft, Notification, NotificationChannel};

use super::decode_enum;

#[derive(Debug, Clone)]
pub struct NewEmailDraft {
    pub meeting_id: Option<Id>,
    pub subject: String,
    pub body: String,
    pub recipients: Vec<String>,
    /// Label for the tone/strategy variant, e.g. "concise recap".
    pub variant: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewNotification {
    pub source_kind: String,
    pub source_id: Id,
    pub recipient: String,
    pub channel: NotificationChannel,
    pub body: String,
}

/// Email drafts.
///
/// Drafts are created in [`DraftStatus::Draft`] and can only reach [`DraftStatus::Sent`]
/// by way of [`DraftStatus::Approved`]. There is no method here that creates a sent draft
/// or approves one implicitly — a wrong auto-send is the highest-consequence failure in
/// this product, so the state machine is enforced rather than documented. See SECURITY.md.
#[derive(Debug)]
pub struct EmailDraftRepository<'a> {
    db: &'a Database,
}

impl<'a> EmailDraftRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn create(&self, new: NewEmailDraft) -> Result<EmailDraft> {
        let now = Utc::now();
        let draft = EmailDraft {
            id: Id::new(),
            meeting_id: new.meeting_id,
            subject: new.subject,
            body: new.body,
            recipients: new.recipients,
            status: DraftStatus::Draft,
            variant: new.variant,
            created_at: now,
            updated_at: now,
        };

        self.db.conn().execute(
            "INSERT INTO email_drafts
                (id, meeting_id, subject, body, recipients, status, variant, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                draft.id,
                draft.meeting_id,
                draft.subject,
                draft.body,
                serde_json::to_string(&draft.recipients)?,
                draft.status.as_str(),
                draft.variant,
                draft.created_at,
                draft.updated_at
            ],
        )?;

        Ok(draft)
    }

    pub fn get(&self, id: Id) -> Result<EmailDraft> {
        self.db
            .conn()
            .query_row(SELECT_DRAFT, rusqlite::params![id], map_draft)
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => StorageError::not_found("EmailDraft", id),
                other => other.into(),
            })
            .and_then(|r| r)
    }

    pub fn list_for_meeting(&self, meeting_id: Id) -> Result<Vec<EmailDraft>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, meeting_id, subject, body, recipients, status, variant,
                    created_at, updated_at
             FROM email_drafts WHERE meeting_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![meeting_id], map_draft)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    /// Record explicit user approval. This is the only path to a sendable draft.
    pub fn approve(&self, id: Id) -> Result<EmailDraft> {
        self.transition(id, DraftStatus::Draft, DraftStatus::Approved)
    }

    /// Mark an approved draft as sent.
    ///
    /// Requires the draft to already be [`DraftStatus::Approved`]; calling this on an
    /// unapproved draft fails rather than sending. Nothing in the codebase may bypass this.
    pub fn mark_sent(&self, id: Id) -> Result<EmailDraft> {
        self.transition(id, DraftStatus::Approved, DraftStatus::Sent)
    }

    pub fn discard(&self, id: Id) -> Result<EmailDraft> {
        let changed = self.db.conn().execute(
            "UPDATE email_drafts SET status = ?2, updated_at = ?3
             WHERE id = ?1 AND status != 'sent'",
            rusqlite::params![id, DraftStatus::Discarded.as_str(), Utc::now()],
        )?;
        if changed == 0 {
            // Either missing, or already sent — a sent email cannot be un-sent.
            let current = self.get(id)?;
            return Err(StorageError::Corrupt {
                column: "email_drafts.status",
                reason: format!(
                    "cannot discard a draft in state '{}'",
                    current.status.as_str()
                ),
            });
        }
        self.get(id)
    }

    fn transition(&self, id: Id, from: DraftStatus, to: DraftStatus) -> Result<EmailDraft> {
        let changed = self.db.conn().execute(
            "UPDATE email_drafts SET status = ?3, updated_at = ?4
             WHERE id = ?1 AND status = ?2",
            rusqlite::params![id, from.as_str(), to.as_str(), Utc::now()],
        )?;

        if changed == 0 {
            let current = self.get(id)?;
            return Err(StorageError::Corrupt {
                column: "email_drafts.status",
                reason: format!(
                    "cannot move draft from '{}' to '{}': it is currently '{}'",
                    from.as_str(),
                    to.as_str(),
                    current.status.as_str()
                ),
            });
        }
        self.get(id)
    }
}

#[derive(Debug)]
pub struct NotificationRepository<'a> {
    db: &'a Database,
}

impl<'a> NotificationRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn create(&self, new: NewNotification) -> Result<Notification> {
        let notification = Notification {
            id: Id::new(),
            source_kind: new.source_kind,
            source_id: new.source_id,
            recipient: new.recipient,
            channel: new.channel,
            body: new.body,
            read_at: None,
            delivered_at: None,
            created_at: Utc::now(),
        };

        self.db.conn().execute(
            "INSERT INTO notifications
                (id, source_kind, source_id, recipient, channel, body,
                 read_at, delivered_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, ?7)",
            rusqlite::params![
                notification.id,
                notification.source_kind,
                notification.source_id,
                notification.recipient,
                notification.channel.as_str(),
                notification.body,
                notification.created_at
            ],
        )?;

        Ok(notification)
    }

    pub fn unread_for(&self, recipient: &str) -> Result<Vec<Notification>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, source_kind, source_id, recipient, channel, body,
                    read_at, delivered_at, created_at
             FROM notifications
             WHERE recipient = ?1 AND read_at IS NULL
             ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![recipient], map_notification)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    /// Undelivered notifications on a channel — what a delivery worker picks up.
    pub fn pending_on(&self, channel: NotificationChannel) -> Result<Vec<Notification>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, source_kind, source_id, recipient, channel, body,
                    read_at, delivered_at, created_at
             FROM notifications
             WHERE channel = ?1 AND delivered_at IS NULL
             ORDER BY created_at",
        )?;
        let rows = stmt.query_map(rusqlite::params![channel.as_str()], map_notification)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    pub fn mark_delivered(&self, id: Id, at: DateTime<Utc>) -> Result<()> {
        let changed = self.db.conn().execute(
            "UPDATE notifications SET delivered_at = ?2 WHERE id = ?1",
            rusqlite::params![id, at],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("Notification", id));
        }
        Ok(())
    }

    pub fn mark_read(&self, id: Id, at: DateTime<Utc>) -> Result<()> {
        let changed = self.db.conn().execute(
            "UPDATE notifications SET read_at = ?2 WHERE id = ?1",
            rusqlite::params![id, at],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("Notification", id));
        }
        Ok(())
    }
}

const SELECT_DRAFT: &str =
    "SELECT id, meeting_id, subject, body, recipients, status, variant, created_at, updated_at
     FROM email_drafts WHERE id = ?1";

fn map_draft(row: &Row<'_>) -> rusqlite::Result<Result<EmailDraft>> {
    let recipients_raw: String = row.get(4)?;
    let status_raw: String = row.get(5)?;

    Ok((|| {
        Ok(EmailDraft {
            id: row.get(0)?,
            meeting_id: row.get(1)?,
            subject: row.get(2)?,
            body: row.get(3)?,
            recipients: serde_json::from_str(&recipients_raw)?,
            status: decode_enum("email_drafts.status", &status_raw, DraftStatus::parse)?,
            variant: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    })())
}

fn map_notification(row: &Row<'_>) -> rusqlite::Result<Result<Notification>> {
    let channel_raw: String = row.get(4)?;
    Ok((|| {
        Ok(Notification {
            id: row.get(0)?,
            source_kind: row.get(1)?,
            source_id: row.get(2)?,
            recipient: row.get(3)?,
            channel: decode_enum(
                "notifications.channel",
                &channel_raw,
                NotificationChannel::parse,
            )?,
            body: row.get(5)?,
            read_at: row.get(6)?,
            delivered_at: row.get(7)?,
            created_at: row.get(8)?,
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn draft(db: &Database) -> EmailDraft {
        EmailDraftRepository::new(db)
            .create(NewEmailDraft {
                meeting_id: None,
                subject: "Recap".into(),
                body: "Here is what we agreed.".into(),
                recipients: vec!["a@example.com".into(), "b@example.com".into()],
                variant: Some("concise recap".into()),
            })
            .expect("create draft")
    }

    #[test]
    fn drafts_start_unapproved() {
        let db = db();
        assert_eq!(draft(&db).status, DraftStatus::Draft);
    }

    #[test]
    fn recipients_survive_the_round_trip() {
        let db = db();
        let created = draft(&db);
        let fetched = EmailDraftRepository::new(&db).get(created.id).unwrap();
        assert_eq!(fetched.recipients, vec!["a@example.com", "b@example.com"]);
        assert_eq!(fetched, created);
    }

    #[test]
    fn approval_then_send_is_the_happy_path() {
        let db = db();
        let repo = EmailDraftRepository::new(&db);
        let d = draft(&db);

        assert_eq!(repo.approve(d.id).unwrap().status, DraftStatus::Approved);
        assert_eq!(repo.mark_sent(d.id).unwrap().status, DraftStatus::Sent);
    }

    #[test]
    fn an_unapproved_draft_cannot_be_sent() {
        let db = db();
        let repo = EmailDraftRepository::new(&db);
        let d = draft(&db);

        let err = repo
            .mark_sent(d.id)
            .expect_err("sending without approval must fail");
        assert!(matches!(err, StorageError::Corrupt { .. }), "got {err:?}");
        assert_eq!(repo.get(d.id).unwrap().status, DraftStatus::Draft);
    }

    #[test]
    fn a_discarded_draft_cannot_be_sent() {
        let db = db();
        let repo = EmailDraftRepository::new(&db);
        let d = draft(&db);
        repo.discard(d.id).unwrap();

        assert!(repo.approve(d.id).is_err());
        assert!(repo.mark_sent(d.id).is_err());
        assert_eq!(repo.get(d.id).unwrap().status, DraftStatus::Discarded);
    }

    #[test]
    fn a_sent_draft_cannot_be_discarded() {
        let db = db();
        let repo = EmailDraftRepository::new(&db);
        let d = draft(&db);
        repo.approve(d.id).unwrap();
        repo.mark_sent(d.id).unwrap();

        assert!(
            repo.discard(d.id).is_err(),
            "a sent email cannot be un-sent"
        );
        assert_eq!(repo.get(d.id).unwrap().status, DraftStatus::Sent);
    }

    #[test]
    fn sending_twice_fails_the_second_time() {
        let db = db();
        let repo = EmailDraftRepository::new(&db);
        let d = draft(&db);
        repo.approve(d.id).unwrap();
        repo.mark_sent(d.id).unwrap();

        assert!(
            repo.mark_sent(d.id).is_err(),
            "must not send the same draft twice"
        );
    }

    #[test]
    fn notifications_start_unread_and_undelivered() {
        let db = db();
        let repo = NotificationRepository::new(&db);
        let n = repo
            .create(NewNotification {
                source_kind: "action_item".into(),
                source_id: Id::new(),
                recipient: "alex".into(),
                channel: NotificationChannel::Digest,
                body: "3 items due today".into(),
            })
            .unwrap();

        assert!(n.is_unread());
        assert!(n.delivered_at.is_none());
        assert_eq!(repo.unread_for("alex").unwrap().len(), 1);
        assert_eq!(repo.unread_for("jordan").unwrap().len(), 0);
    }

    #[test]
    fn pending_is_scoped_to_a_channel_and_clears_on_delivery() {
        let db = db();
        let repo = NotificationRepository::new(&db);

        let digest = repo
            .create(NewNotification {
                source_kind: "action_item".into(),
                source_id: Id::new(),
                recipient: "alex".into(),
                channel: NotificationChannel::Digest,
                body: "digest".into(),
            })
            .unwrap();
        repo.create(NewNotification {
            source_kind: "mention".into(),
            source_id: Id::new(),
            recipient: "alex".into(),
            channel: NotificationChannel::Slack,
            body: "slack".into(),
        })
        .unwrap();

        assert_eq!(
            repo.pending_on(NotificationChannel::Digest).unwrap().len(),
            1
        );

        repo.mark_delivered(digest.id, Utc::now()).unwrap();
        assert_eq!(
            repo.pending_on(NotificationChannel::Digest).unwrap().len(),
            0
        );
        assert_eq!(
            repo.pending_on(NotificationChannel::Slack).unwrap().len(),
            1
        );
    }

    #[test]
    fn marking_read_removes_from_unread() {
        let db = db();
        let repo = NotificationRepository::new(&db);
        let n = repo
            .create(NewNotification {
                source_kind: "decision".into(),
                source_id: Id::new(),
                recipient: "alex".into(),
                channel: NotificationChannel::Desktop,
                body: "new decision".into(),
            })
            .unwrap();

        repo.mark_read(n.id, Utc::now()).unwrap();
        assert_eq!(repo.unread_for("alex").unwrap().len(), 0);
    }
}
