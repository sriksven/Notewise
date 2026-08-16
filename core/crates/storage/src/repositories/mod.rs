//! Repositories — the only way to reach stored data from outside this crate.
//!
//! Each repository borrows the [`Database`](crate::Database) rather than owning it, so a
//! caller can hold several at once without cloning connections.

mod comms;
mod connector_account;
mod edge;
mod embedding;
mod external_item;
mod meeting;
mod note;
mod outbox;
mod person;
mod search;
mod series;
mod setting;
mod summary;
mod ticket;
mod workspace;

pub use comms::{EmailDraftRepository, NewEmailDraft, NewNotification, NotificationRepository};
pub use connector_account::{AccountStatus, ConnectorAccount, ConnectorAccountRepository};
pub use edge::{EdgeRecord, EdgeRepository, NewEdge};
pub use embedding::{Embedding, EmbeddingRepository, IndexedEntity, NewEmbedding};
pub use external_item::{ExternalItem, ExternalItemRepository, NewExternalItem};
pub use meeting::{MeetingRepository, NewMeeting, NewTranscriptSegment};
pub use note::{NewNote, NoteRepository};
pub use outbox::{NewOutboxEntry, OutboxRecord, OutboxRepository, OutboxStatus};
pub use person::{NewPerson, PersonRepository, VoicePrint};
pub use search::{SearchHit, SearchRepository};
pub use series::{MeetingSeriesRepository, NewMeetingSeries};
pub use setting::SettingsRepository;
pub use summary::{NewActionItem, NewDecision, NewSummary, SummaryRepository};
pub use ticket::{NewTicket, TicketEdit, TicketRepository};
pub use workspace::{NewProject, NewWorkspace, ProjectRepository, WorkspaceRepository};

use crate::error::{Result, StorageError};

/// Decode an enum column, turning an unrecognized value into a `Corrupt` error rather than
/// a panic. Stored enums can go stale when a database is written by a newer build.
pub(crate) fn decode_enum<T>(
    column: &'static str,
    raw: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<T> {
    parse(raw).ok_or_else(|| StorageError::Corrupt {
        column,
        reason: format!("unrecognized value '{raw}'"),
    })
}
