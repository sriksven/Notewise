//! Write endpoints for the workspace objects: notes, tickets, action items, decisions,
//! people, and meeting series.
//!
//! These live outside `routes.rs` because that file already carries the capture, model and
//! chat surfaces and is long enough that adding a dozen more handlers would make it hard to
//! read. The router here is merged in by [`crate::routes::router`].
//!
//! `storage` has had create/update/delete for most of this since the schema was written; it
//! simply had no HTTP surface, so the desktop app could read a ticket but never file one.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router as AxumRouter};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use notewise_graph::{EdgeKind, Graph, NodeKind, NodeRef};
use notewise_storage::{
    ActionItem, Decision, Id, MeetingSeries, MeetingSeriesRepository, NewActionItem, NewDecision,
    NewMeetingSeries, NewPerson, NewTicket, NoteRepository, Person, PersonRepository,
    SettingsRepository, SummaryRepository, Ticket, TicketEdit, TicketRepository, WorkStatus,
};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

type Shared = Arc<AppState>;

pub(crate) fn router() -> AxumRouter<Shared> {
    AxumRouter::new()
        .route(
            "/v1/notes/:id",
            get(get_note).put(update_note).delete(delete_note),
        )
        .route("/v1/notes/:id/restore", post(restore_note))
        // Registered before the `:id` routes above would ever match — axum's router is not
        // order-sensitive for distinct literals, but `/v1/trash` and `/v1/notes/:id` do not
        // overlap anyway. Kept adjacent to the notes block because that is what it holds.
        .route("/v1/trash", get(list_trash).delete(empty_trash))
        .route("/v1/meetings/:id/notes", get(meeting_notes))
        .route("/v1/tickets", post(create_ticket))
        .route(
            "/v1/tickets/:id",
            get(get_ticket).patch(patch_ticket).delete(delete_ticket),
        )
        .route(
            "/v1/meetings/:id/action-items",
            get(list_action_items).post(create_action_item),
        )
        .route(
            "/v1/action-items/:id",
            patch(patch_action_item).delete(delete_action_item),
        )
        .route("/v1/action-items/:id/promote", post(promote_action_item))
        .route(
            "/v1/meetings/:id/decisions",
            get(list_decisions).post(create_decision),
        )
        .route("/v1/decisions/:id", delete(delete_decision))
        .route("/v1/people", get(list_people).post(create_person))
        .route(
            "/v1/people/:id",
            get(get_person).patch(patch_person).delete(delete_person),
        )
        .route("/v1/people/:id/meetings", get(person_meetings))
        .route(
            "/v1/voiceprints",
            get(voiceprint_status)
                .post(set_voiceprints_enabled)
                .delete(forget_voiceprints),
        )
        .route("/v1/series", get(list_series).post(create_series))
        .route("/v1/meetings/:id/series", post(assign_series))
        .route("/v1/meetings/:id/brief", get(meeting_brief))
        .route(
            "/v1/meetings/:id/participants",
            get(list_participants).post(add_participant),
        )
}

fn parse_id(raw: &str) -> ApiResult<Id> {
    raw.parse()
        .map_err(|_| ApiError::BadRequest(format!("'{raw}' is not a valid id")))
}

fn parse_status(raw: &str) -> ApiResult<WorkStatus> {
    WorkStatus::parse(raw).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "'{raw}' is not a status; expected todo, in_progress, done or cancelled"
        ))
    })
}

// ---------------------------------------------------------------- notes

#[derive(Debug, Deserialize)]
struct UpdateNoteRequest {
    title: String,
    /// Serialized blocks. Opaque to the engine so the editor format can change freely.
    body: String,
}

async fn get_note(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<notewise_storage::Note>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    Ok(Json(NoteRepository::new(&db).get(id)?))
}

async fn update_note(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(req): Json<UpdateNoteRequest>,
) -> ApiResult<Json<notewise_storage::Note>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    Ok(Json(
        NoteRepository::new(&db).update(id, &req.title, &req.body)?,
    ))
}

/// Move a note to the trash, or destroy it.
///
/// Trashing is the default and `?purge=true` is the escape hatch, rather than the other way
/// round: the destructive reading of `DELETE /v1/notes/:id` is the one a mistyped script or a
/// stale client would reach for, and it should not be the one that loses work.
async fn delete_note(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Query(query): Query<DeleteNoteQuery>,
) -> ApiResult<Json<notewise_storage::Note>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    let repo = NoteRepository::new(&db);

    if query.purge {
        let note = repo.get(id)?;
        // The edge table has no foreign keys, so nothing cascades for it. Without this the
        // note's links survive it and point at a row that is gone.
        Graph::new(&db).detach(NodeRef::new(NodeKind::Note, id))?;
        repo.purge(id)?;
        return Ok(Json(note));
    }

    Ok(Json(repo.trash(id)?))
}

#[derive(Debug, Default, Deserialize)]
struct DeleteNoteQuery {
    /// Destroy the note instead of trashing it. Not undoable.
    #[serde(default)]
    purge: bool,
}

async fn restore_note(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<notewise_storage::Note>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    Ok(Json(NoteRepository::new(&db).restore(id)?))
}

/// What is in the trash.
///
/// Notes only. Meetings own audio and transcripts and are not deleted from the UI at all;
/// tickets mirror external trackers where a delete has to propagate rather than linger.
async fn list_trash(State(state): State<Shared>) -> ApiResult<Json<Vec<notewise_storage::Note>>> {
    let db = state.db().await;
    Ok(Json(NoteRepository::new(&db).list_trashed()?))
}

async fn empty_trash(State(state): State<Shared>) -> ApiResult<Json<Emptied>> {
    let db = state.db().await;
    let repo = NoteRepository::new(&db);

    // Detach before destroying, for the same reason as `purge` above. Collected first
    // because the rows are gone by the time the delete returns.
    let graph = Graph::new(&db);
    for id in repo.trashed_ids()? {
        graph.detach(NodeRef::new(NodeKind::Note, id))?;
    }

    Ok(Json(Emptied {
        deleted: repo.empty_trash()?,
    }))
}

#[derive(Debug, Serialize)]
struct Emptied {
    deleted: usize,
}

/// The acknowledgement returned by the delete endpoints that have nothing to hand back.
#[derive(Debug, Serialize)]
struct Deleted {
    deleted: bool,
}

/// The notes a user wrote against one meeting.
///
/// A graph edge rather than a `meeting_id` column, per the ownership rule: a note is not owned
/// by the meeting it was taken in. It survives the meeting, can reference several, and can be
/// unlinked without being destroyed.
async fn meeting_notes(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<notewise_storage::Note>>> {
    let meeting_id = parse_id(&id)?;
    let db = state.db().await;

    let related = Graph::new(&db).related_of_kind(
        NodeRef::new(NodeKind::Meeting, meeting_id),
        NodeKind::Note,
        1,
    )?;

    let repo = NoteRepository::new(&db);
    let mut notes = Vec::with_capacity(related.len());
    for node in related {
        // A note whose row is gone leaves a dangling edge only if something deleted it
        // without detaching; skip rather than fail the whole list over one bad edge.
        if let Ok(note) = repo.get(node.node.id) {
            // Trashed notes stay out of the meeting's tab. They are reachable in the trash,
            // which is where a user looks for something they deleted.
            if note.deleted_at.is_none() {
                notes.push(note);
            }
        }
    }

    notes.sort_by_key(|note| std::cmp::Reverse(note.updated_at));
    Ok(Json(notes))
}

// ---------------------------------------------------------------- tickets

#[derive(Debug, Deserialize)]
struct CreateTicketRequest {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    due_at: Option<DateTime<Utc>>,
    #[serde(default)]
    project_id: Option<String>,
    /// Link the new ticket back to the meeting it came out of.
    #[serde(default)]
    meeting_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PatchTicketRequest {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    due_at: Option<DateTime<Utc>>,
    #[serde(default)]
    status: Option<String>,
    /// Explicitly blank the owner. Distinct from omitting `owner`, which leaves it alone.
    #[serde(default)]
    clear_owner: bool,
    #[serde(default)]
    clear_due_at: bool,
}

async fn create_ticket(
    State(state): State<Shared>,
    Json(req): Json<CreateTicketRequest>,
) -> ApiResult<Json<Ticket>> {
    if req.title.trim().is_empty() {
        return Err(ApiError::BadRequest("a ticket needs a title".into()));
    }

    let project_id = req.project_id.as_deref().map(parse_id).transpose()?;
    let meeting_id = req.meeting_id.as_deref().map(parse_id).transpose()?;

    let db = state.db().await;
    let ticket = TicketRepository::new(&db).create(NewTicket {
        project_id,
        title: req.title,
        description: req.description,
        owner: req.owner,
        due_at: req.due_at,
    })?;

    if let Some(meeting_id) = meeting_id {
        Graph::new(&db).connect(
            NodeRef::new(NodeKind::Ticket, ticket.id),
            EdgeKind::DerivedFrom,
            NodeRef::new(NodeKind::Meeting, meeting_id),
        )?;
    }

    Ok(Json(ticket))
}

async fn get_ticket(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Ticket>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    Ok(Json(TicketRepository::new(&db).get(id)?))
}

async fn patch_ticket(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(req): Json<PatchTicketRequest>,
) -> ApiResult<Json<Ticket>> {
    let id = parse_id(&id)?;
    let status = req.status.as_deref().map(parse_status).transpose()?;

    let db = state.db().await;
    let repo = TicketRepository::new(&db);

    let mut ticket = repo.update(
        id,
        TicketEdit {
            title: req.title.as_deref(),
            description: req.description.as_deref(),
            owner: req.owner.as_deref(),
            due_at: req.due_at,
        },
    )?;

    if req.clear_owner {
        ticket = repo.clear_owner(id)?;
    }
    if req.clear_due_at {
        ticket = repo.clear_due_at(id)?;
    }
    if let Some(status) = status {
        ticket = repo.set_status(id, status)?;
    }

    Ok(Json(ticket))
}

async fn delete_ticket(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Deleted>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    TicketRepository::new(&db).delete(id)?;
    Ok(Json(Deleted { deleted: true }))
}

// ---------------------------------------------------------------- action items

#[derive(Debug, Deserialize)]
struct CreateActionItemRequest {
    text: String,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    due_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct PatchActionItemRequest {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    owner_person_id: Option<String>,
    #[serde(default)]
    due_at: Option<DateTime<Utc>>,
    #[serde(default)]
    clear_due_at: bool,
    #[serde(default)]
    clear_owner: bool,
}

async fn list_action_items(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<ActionItem>>> {
    let meeting_id = parse_id(&id)?;
    let db = state.db().await;
    // Meeting-scoped, not summary-scoped: an item a user typed by hand, or one whose
    // summary was replaced, belongs in this list too.
    Ok(Json(
        SummaryRepository::new(&db).action_items_for_meeting(meeting_id)?,
    ))
}

async fn create_action_item(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(req): Json<CreateActionItemRequest>,
) -> ApiResult<Json<ActionItem>> {
    let meeting_id = parse_id(&id)?;
    if req.text.trim().is_empty() {
        return Err(ApiError::BadRequest("an action item needs text".into()));
    }

    let db = state.db().await;
    let item = SummaryRepository::new(&db).add_action_item(NewActionItem {
        owner: req.owner,
        due_at: req.due_at,
        ..NewActionItem::on_meeting(meeting_id, req.text)
    })?;
    Ok(Json(item))
}

async fn patch_action_item(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(req): Json<PatchActionItemRequest>,
) -> ApiResult<Json<ActionItem>> {
    let id = parse_id(&id)?;
    let status = req.status.as_deref().map(parse_status).transpose()?;
    let person_id = req.owner_person_id.as_deref().map(parse_id).transpose()?;

    let db = state.db().await;
    let repo = SummaryRepository::new(&db);

    if let Some(status) = status {
        repo.set_action_item_status(id, status)?;
    }
    if req.clear_owner {
        repo.assign_action_item(id, None)?;
    } else if let Some(owner) = req.owner.as_deref() {
        repo.assign_action_item(id, Some(owner))?;
    }
    if let Some(person_id) = person_id {
        repo.set_action_item_person(id, Some(person_id))?;
    }
    if req.clear_due_at {
        repo.set_action_item_due(id, None)?;
    } else if req.due_at.is_some() {
        repo.set_action_item_due(id, req.due_at)?;
    }

    Ok(Json(repo.action_item(id)?))
}

async fn delete_action_item(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Deleted>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    SummaryRepository::new(&db).delete_action_item(id)?;
    Ok(Json(Deleted { deleted: true }))
}

/// Turn an action item into a ticket, recording the link as a `became_ticket` edge.
///
/// The action item is not deleted. It is the record that the meeting produced this work;
/// the ticket is where the work is tracked. Deleting it would erase the meeting's output the
/// moment someone decided to act on it.
async fn promote_action_item(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Ticket>> {
    let item_id = parse_id(&id)?;
    let db = state.db().await;
    let summaries = SummaryRepository::new(&db);
    let item = summaries.action_item(item_id)?;

    let graph = Graph::new(&db);

    // Promoting twice should not file a second ticket. The edge is the record of whether
    // this already happened, so consult it before creating anything.
    let existing = graph
        .related(NodeRef::new(NodeKind::ActionItem, item_id), 1)?
        .into_iter()
        .find(|n| n.via == EdgeKind::BecameTicket && n.node.kind == NodeKind::Ticket);
    if let Some(found) = existing {
        return Ok(Json(TicketRepository::new(&db).get(found.node.id)?));
    }

    let ticket = TicketRepository::new(&db).create(NewTicket {
        project_id: None,
        title: item.text.clone(),
        description: None,
        owner: item.owner.clone(),
        due_at: item.due_at,
    })?;

    graph.connect(
        NodeRef::new(NodeKind::ActionItem, item_id),
        EdgeKind::BecameTicket,
        NodeRef::new(NodeKind::Ticket, ticket.id),
    )?;
    graph.connect(
        NodeRef::new(NodeKind::Ticket, ticket.id),
        EdgeKind::DerivedFrom,
        NodeRef::new(NodeKind::Meeting, item.meeting_id),
    )?;

    Ok(Json(ticket))
}

// ---------------------------------------------------------------- decisions

#[derive(Debug, Deserialize)]
struct CreateDecisionRequest {
    text: String,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    decided_at: Option<DateTime<Utc>>,
}

async fn list_decisions(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<Decision>>> {
    let meeting_id = parse_id(&id)?;
    let db = state.db().await;
    Ok(Json(
        SummaryRepository::new(&db).decisions_for_meeting(meeting_id)?,
    ))
}

async fn create_decision(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(req): Json<CreateDecisionRequest>,
) -> ApiResult<Json<Decision>> {
    let meeting_id = parse_id(&id)?;
    if req.text.trim().is_empty() {
        return Err(ApiError::BadRequest("a decision needs text".into()));
    }

    let db = state.db().await;
    let decision = SummaryRepository::new(&db).add_decision(NewDecision {
        meeting_id,
        summary_id: None,
        text: req.text,
        reasoning: req.reasoning,
        decided_at: req.decided_at,
    })?;
    Ok(Json(decision))
}

async fn delete_decision(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Deleted>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    SummaryRepository::new(&db).delete_decision(id)?;
    Ok(Json(Deleted { deleted: true }))
}

// ---------------------------------------------------------------- people

#[derive(Debug, Deserialize)]
struct CreatePersonRequest {
    display_name: String,
    #[serde(default)]
    email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PatchPersonRequest {
    display_name: String,
}

async fn list_people(State(state): State<Shared>) -> ApiResult<Json<Vec<Person>>> {
    let db = state.db().await;
    Ok(Json(PersonRepository::new(&db).list()?))
}

async fn create_person(
    State(state): State<Shared>,
    Json(req): Json<CreatePersonRequest>,
) -> ApiResult<Json<Person>> {
    if req.display_name.trim().is_empty() {
        return Err(ApiError::BadRequest("a person needs a name".into()));
    }
    let db = state.db().await;
    Ok(Json(PersonRepository::new(&db).create(NewPerson {
        display_name: req.display_name,
        email: req.email,
    })?))
}

async fn get_person(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Person>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    Ok(Json(PersonRepository::new(&db).get(id)?))
}

async fn patch_person(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(req): Json<PatchPersonRequest>,
) -> ApiResult<Json<Person>> {
    let id = parse_id(&id)?;
    if req.display_name.trim().is_empty() {
        return Err(ApiError::BadRequest("a person needs a name".into()));
    }
    let db = state.db().await;
    Ok(Json(
        PersonRepository::new(&db).rename(id, &req.display_name)?,
    ))
}

async fn delete_person(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Deleted>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    PersonRepository::new(&db).delete(id)?;
    Ok(Json(Deleted { deleted: true }))
}

async fn person_meetings(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<notewise_storage::Meeting>>> {
    let person_id = parse_id(&id)?;
    let db = state.db().await;
    let people = PersonRepository::new(&db);
    let meetings = notewise_storage::MeetingRepository::new(&db);

    let ids = people.meeting_ids_for_person(person_id)?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(meetings.get(id)?);
    }
    Ok(Json(out))
}

#[derive(Debug, Deserialize)]
struct AddParticipantRequest {
    /// An existing person, or omit and pass `display_name` to find-or-create one.
    #[serde(default)]
    person_id: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    role: Option<String>,
}

async fn list_participants(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<Person>>> {
    let meeting_id = parse_id(&id)?;
    let db = state.db().await;
    Ok(Json(PersonRepository::new(&db).participants(meeting_id)?))
}

async fn add_participant(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(req): Json<AddParticipantRequest>,
) -> ApiResult<Json<Person>> {
    let meeting_id = parse_id(&id)?;
    let db = state.db().await;
    let people = PersonRepository::new(&db);

    let person = match (req.person_id.as_deref(), req.display_name.as_deref()) {
        (Some(raw), _) => people.get(parse_id(raw)?)?,
        (None, Some(name)) if !name.trim().is_empty() => people.find_or_create_by_name(name)?,
        _ => {
            return Err(ApiError::BadRequest(
                "pass either person_id or display_name".into(),
            ))
        }
    };

    people.add_participant(meeting_id, person.id, req.role.as_deref())?;
    Graph::new(&db).connect(
        NodeRef::new(NodeKind::Meeting, meeting_id),
        EdgeKind::Mentions,
        NodeRef::new(NodeKind::Person, person.id),
    )?;

    Ok(Json(person))
}

// ---------------------------------------------------------------- series and briefs

#[derive(Debug, Deserialize)]
struct CreateSeriesRequest {
    title: String,
}

#[derive(Debug, Deserialize)]
struct AssignSeriesRequest {
    /// An existing series, or omit both to thread by the meeting's own title.
    #[serde(default)]
    series_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    /// Remove this meeting from its series.
    #[serde(default)]
    clear: bool,
}

async fn list_series(State(state): State<Shared>) -> ApiResult<Json<Vec<MeetingSeries>>> {
    let db = state.db().await;
    Ok(Json(MeetingSeriesRepository::new(&db).list()?))
}

async fn create_series(
    State(state): State<Shared>,
    Json(req): Json<CreateSeriesRequest>,
) -> ApiResult<Json<MeetingSeries>> {
    if req.title.trim().is_empty() {
        return Err(ApiError::BadRequest("a series needs a title".into()));
    }
    let db = state.db().await;
    Ok(Json(MeetingSeriesRepository::new(&db).create(
        NewMeetingSeries {
            title: req.title,
            project_id: None,
        },
    )?))
}

async fn assign_series(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(req): Json<AssignSeriesRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let meeting_id = parse_id(&id)?;
    let db = state.db().await;
    let repo = MeetingSeriesRepository::new(&db);

    if req.clear {
        repo.assign_meeting(meeting_id, None)?;
        return Ok(Json(serde_json::json!({ "series": null })));
    }

    let series = match (req.series_id.as_deref(), req.title.as_deref()) {
        (Some(raw), _) => repo.get(parse_id(raw)?)?,
        (None, Some(title)) => repo.find_or_create_by_title(title)?,
        // Neither given: thread on the meeting's own title, which is what "this is a
        // recurring meeting" means when the user has not named the series.
        (None, None) => {
            let meeting = notewise_storage::MeetingRepository::new(&db).get(meeting_id)?;
            repo.find_or_create_by_title(&meeting.title)?
        }
    };

    repo.assign_meeting(meeting_id, Some(series.id))?;
    Graph::new(&db).connect(
        NodeRef::new(NodeKind::MeetingSeries, series.id),
        EdgeKind::Contains,
        NodeRef::new(NodeKind::Meeting, meeting_id),
    )?;

    Ok(Json(serde_json::json!({ "series": series })))
}

#[derive(Debug, Serialize)]
struct Brief {
    /// `None` when this meeting is not part of a series, which is not an error — it just
    /// means there is no history to carry forward.
    series: Option<MeetingSeries>,
    previous_meeting_id: Option<Id>,
    /// Open action items from earlier meetings in the series.
    unfinished_business: Vec<ActionItem>,
    /// Decisions from the immediately preceding instance, for context.
    recent_decisions: Vec<Decision>,
}

/// What a recurring meeting is still carrying.
///
/// Answers "what did we say last time that is still open" without the user reopening the
/// previous meeting and reading it. Deliberately a plain query rather than a model call:
/// it is derived from stored state, so it costs nothing and cannot hallucinate an
/// obligation that was never agreed.
async fn meeting_brief(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Brief>> {
    let meeting_id = parse_id(&id)?;
    let db = state.db().await;

    let meeting = notewise_storage::MeetingRepository::new(&db).get(meeting_id)?;
    let Some(series_id) = meeting.series_id else {
        return Ok(Json(Brief {
            series: None,
            previous_meeting_id: None,
            unfinished_business: Vec::new(),
            recent_decisions: Vec::new(),
        }));
    };

    let series_repo = MeetingSeriesRepository::new(&db);
    let previous = series_repo.previous_meeting(series_id, meeting_id)?;
    let recent_decisions = match previous {
        Some(prev) => SummaryRepository::new(&db).decisions_for_meeting(prev)?,
        None => Vec::new(),
    };

    Ok(Json(Brief {
        series: Some(series_repo.get(series_id)?),
        previous_meeting_id: previous,
        unfinished_business: series_repo.unfinished_business(series_id, meeting_id)?,
        recent_decisions,
    }))
}

// ---------------------------------------------------------------- voice prints

/// Whether voice prints are being stored, and how many exist.
async fn voiceprint_status(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<crate::voiceprints::VoiceprintStatus>> {
    let db = state.db().await;
    let settings = SettingsRepository::new(&db);

    Ok(Json(crate::voiceprints::VoiceprintStatus {
        enabled: crate::voiceprints::enabled(&settings),
        stored: PersonRepository::new(&db).voice_prints()?.len(),
    }))
}

/// Turn the storing of voice prints on or off.
///
/// Turning it *off* also erases what is already stored. A switch that stops future collection
/// while keeping the existing identifiers would be the least honest reading of the word off, and
/// is the behaviour people are right to be suspicious of.
async fn set_voiceprints_enabled(
    State(state): State<Arc<AppState>>,
    Json(body): Json<crate::voiceprints::SetEnabled>,
) -> ApiResult<Json<crate::voiceprints::VoiceprintStatus>> {
    let db = state.db().await;
    let settings = SettingsRepository::new(&db);
    let people = PersonRepository::new(&db);

    settings.set(
        crate::voiceprints::ENABLED_KEY,
        if body.enabled { "true" } else { "false" },
    )?;

    let mut stored = people.voice_prints()?.len();
    if !body.enabled {
        for print in people.voice_prints()? {
            people.clear_voice_print(print.person_id)?;
        }
        stored = 0;
        tracing::info!("voice prints disabled and erased");
    }

    Ok(Json(crate::voiceprints::VoiceprintStatus {
        enabled: body.enabled,
        stored,
    }))
}

/// Erase every stored voice print, leaving the people and their names.
///
/// Separate from the switch so someone can clear what is held without also turning the feature
/// off, and so there is a single obvious control for "forget what I sound like".
async fn forget_voiceprints(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<serde_json::Value>> {
    let db = state.db().await;
    let people = PersonRepository::new(&db);

    let prints = people.voice_prints()?;
    let erased = prints.len();
    for print in prints {
        people.clear_voice_print(print.person_id)?;
    }

    tracing::info!(erased, "voice prints erased");
    Ok(Json(serde_json::json!({ "erased": erased })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use notewise_ai_router::{Router as AiRouter, RouterConfig};
    use notewise_storage::{Database, MeetingRepository, MeetingSource, NewMeeting};
    use tower::ServiceExt;

    /// The app under test, plus a handle to seed the same database directly.
    fn app() -> (AxumRouter, Arc<AppState>) {
        let state = Arc::new(AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        ));
        (router().with_state(state.clone()), state)
    }

    async fn call(app: &AxumRouter, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = app.clone().oneshot(request).await.expect("request");
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        (status, json)
    }

    fn json_request(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request")
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("request")
    }

    async fn seed_meeting(state: &Arc<AppState>, title: &str, started: i64) -> Id {
        let db = state.db().await;
        MeetingRepository::new(&db)
            .create(NewMeeting {
                project_id: None,
                title: title.into(),
                source: MeetingSource::Import,
                started_at: chrono::Utc.timestamp_opt(started, 0).single().unwrap(),
            })
            .expect("meeting")
            .id
    }

    use chrono::TimeZone;

    #[tokio::test]
    async fn a_ticket_can_be_filed_edited_and_closed() {
        let (app, _state) = app();

        let (status, created) = call(
            &app,
            json_request(
                "POST",
                "/v1/tickets",
                serde_json::json!({"title": "Fix the thing"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let id = created["id"].as_str().unwrap().to_string();
        assert_eq!(created["status"], "todo");

        let (status, patched) = call(
            &app,
            json_request(
                "PATCH",
                &format!("/v1/tickets/{id}"),
                serde_json::json!({"owner": "priya", "status": "in_progress"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{patched}");
        assert_eq!(patched["owner"], "priya");
        assert_eq!(patched["status"], "in_progress");

        let (status, _) = call(
            &app,
            json_request(
                "DELETE",
                &format!("/v1/tickets/{id}"),
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = call(&app, get(&format!("/v1/tickets/{id}"))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// A partial PATCH must not blank the fields it did not mention.
    #[tokio::test]
    async fn patching_one_field_leaves_the_others_alone() {
        let (app, _state) = app();
        let (_, created) = call(
            &app,
            json_request(
                "POST",
                "/v1/tickets",
                serde_json::json!({"title": "Fix", "description": "the detail", "owner": "priya"}),
            ),
        )
        .await;
        let id = created["id"].as_str().unwrap().to_string();

        let (_, patched) = call(
            &app,
            json_request(
                "PATCH",
                &format!("/v1/tickets/{id}"),
                serde_json::json!({"status": "done"}),
            ),
        )
        .await;

        assert_eq!(
            patched["description"], "the detail",
            "description was wiped"
        );
        assert_eq!(patched["owner"], "priya", "owner was wiped");
    }

    /// Clearing has to be explicit, because omitting a field means "leave it".
    #[tokio::test]
    async fn an_owner_is_cleared_only_when_asked() {
        let (app, _state) = app();
        let (_, created) = call(
            &app,
            json_request(
                "POST",
                "/v1/tickets",
                serde_json::json!({"title": "Fix", "owner": "priya"}),
            ),
        )
        .await;
        let id = created["id"].as_str().unwrap().to_string();

        let (_, patched) = call(
            &app,
            json_request(
                "PATCH",
                &format!("/v1/tickets/{id}"),
                serde_json::json!({"clear_owner": true}),
            ),
        )
        .await;

        assert!(patched["owner"].is_null(), "{patched}");
    }

    #[tokio::test]
    async fn a_ticket_needs_a_title() {
        let (app, _state) = app();
        let (status, _) = call(
            &app,
            json_request("POST", "/v1/tickets", serde_json::json!({"title": "   "})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn promoting_an_action_item_files_a_ticket_and_keeps_the_item() {
        let (app, state) = app();
        let meeting = seed_meeting(&state, "Standup", 1_700_000_000).await;

        let (_, item) = call(
            &app,
            json_request(
                "POST",
                &format!("/v1/meetings/{meeting}/action-items"),
                serde_json::json!({"text": "Chase the vendor", "owner": "priya"}),
            ),
        )
        .await;
        let item_id = item["id"].as_str().unwrap().to_string();

        let (status, ticket) = call(
            &app,
            json_request(
                "POST",
                &format!("/v1/action-items/{item_id}/promote"),
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{ticket}");
        assert_eq!(ticket["title"], "Chase the vendor");
        assert_eq!(ticket["owner"], "priya", "the owner should carry across");

        // The action item is the meeting's output and must survive being acted on.
        let (_, items) = call(&app, get(&format!("/v1/meetings/{meeting}/action-items"))).await;
        assert_eq!(items.as_array().unwrap().len(), 1);
    }

    /// Promoting twice is a double-click, not a request for two tickets.
    #[tokio::test]
    async fn promoting_twice_returns_the_same_ticket() {
        let (app, state) = app();
        let meeting = seed_meeting(&state, "Standup", 1_700_000_000).await;
        let (_, item) = call(
            &app,
            json_request(
                "POST",
                &format!("/v1/meetings/{meeting}/action-items"),
                serde_json::json!({"text": "Chase the vendor"}),
            ),
        )
        .await;
        let item_id = item["id"].as_str().unwrap().to_string();
        let uri = format!("/v1/action-items/{item_id}/promote");

        let (_, first) = call(&app, json_request("POST", &uri, serde_json::json!({}))).await;
        let (_, second) = call(&app, json_request("POST", &uri, serde_json::json!({}))).await;

        assert_eq!(first["id"], second["id"], "filed a duplicate ticket");
    }

    #[tokio::test]
    async fn an_action_item_can_be_ticked_off() {
        let (app, state) = app();
        let meeting = seed_meeting(&state, "Standup", 1_700_000_000).await;
        let (_, item) = call(
            &app,
            json_request(
                "POST",
                &format!("/v1/meetings/{meeting}/action-items"),
                serde_json::json!({"text": "Book the room"}),
            ),
        )
        .await;
        let id = item["id"].as_str().unwrap().to_string();

        let (status, patched) = call(
            &app,
            json_request(
                "PATCH",
                &format!("/v1/action-items/{id}"),
                serde_json::json!({"status": "done"}),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{patched}");
        assert_eq!(patched["status"], "done");
    }

    #[tokio::test]
    async fn an_unknown_status_is_a_400_not_a_500() {
        let (app, state) = app();
        let meeting = seed_meeting(&state, "Standup", 1_700_000_000).await;
        let (_, item) = call(
            &app,
            json_request(
                "POST",
                &format!("/v1/meetings/{meeting}/action-items"),
                serde_json::json!({"text": "Book the room"}),
            ),
        )
        .await;
        let id = item["id"].as_str().unwrap().to_string();

        let (status, body) = call(
            &app,
            json_request(
                "PATCH",
                &format!("/v1/action-items/{id}"),
                serde_json::json!({"status": "nearly"}),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "bad_request");
    }

    #[tokio::test]
    async fn a_brief_carries_open_work_from_the_previous_meeting() {
        let (app, state) = app();
        let last_week = seed_meeting(&state, "Standup", 1_700_000_000).await;
        let this_week = seed_meeting(&state, "Standup", 1_700_600_000).await;

        for m in [last_week, this_week] {
            let (status, body) = call(
                &app,
                json_request(
                    "POST",
                    &format!("/v1/meetings/{m}/series"),
                    serde_json::json!({}),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
        }

        // One left open, one finished.
        let (_, open) = call(
            &app,
            json_request(
                "POST",
                &format!("/v1/meetings/{last_week}/action-items"),
                serde_json::json!({"text": "Chase the vendor"}),
            ),
        )
        .await;
        let (_, done) = call(
            &app,
            json_request(
                "POST",
                &format!("/v1/meetings/{last_week}/action-items"),
                serde_json::json!({"text": "Book the room"}),
            ),
        )
        .await;
        call(
            &app,
            json_request(
                "PATCH",
                &format!("/v1/action-items/{}", done["id"].as_str().unwrap()),
                serde_json::json!({"status": "done"}),
            ),
        )
        .await;

        let (status, brief) = call(&app, get(&format!("/v1/meetings/{this_week}/brief"))).await;

        assert_eq!(status, StatusCode::OK, "{brief}");
        assert_eq!(brief["previous_meeting_id"], last_week.to_string());
        let carried = brief["unfinished_business"].as_array().unwrap();
        assert_eq!(carried.len(), 1, "{brief}");
        assert_eq!(carried[0]["id"], open["id"]);
    }

    /// Not being in a series is an ordinary state, not an error.
    #[tokio::test]
    async fn a_standalone_meeting_has_an_empty_brief() {
        let (app, state) = app();
        let meeting = seed_meeting(&state, "One-off", 1_700_000_000).await;

        let (status, brief) = call(&app, get(&format!("/v1/meetings/{meeting}/brief"))).await;

        assert_eq!(status, StatusCode::OK, "{brief}");
        assert!(brief["series"].is_null());
        assert!(brief["unfinished_business"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn adding_a_participant_by_name_creates_the_person_once() {
        let (app, state) = app();
        let meeting = seed_meeting(&state, "Standup", 1_700_000_000).await;
        let uri = format!("/v1/meetings/{meeting}/participants");

        let (status, first) = call(
            &app,
            json_request(
                "POST",
                &uri,
                serde_json::json!({"display_name": "Priya Raman"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{first}");

        let (_, second) = call(
            &app,
            json_request(
                "POST",
                &uri,
                serde_json::json!({"display_name": "priya raman"}),
            ),
        )
        .await;
        assert_eq!(first["id"], second["id"], "created a duplicate person");

        let (_, people) = call(&app, get("/v1/people")).await;
        assert_eq!(people.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_participant_needs_an_identity() {
        let (app, state) = app();
        let meeting = seed_meeting(&state, "Standup", 1_700_000_000).await;

        let (status, _) = call(
            &app,
            json_request(
                "POST",
                &format!("/v1/meetings/{meeting}/participants"),
                serde_json::json!({}),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_decision_can_be_recorded_by_hand_without_a_summary() {
        let (app, state) = app();
        let meeting = seed_meeting(&state, "Standup", 1_700_000_000).await;

        let (status, decision) = call(
            &app,
            json_request(
                "POST",
                &format!("/v1/meetings/{meeting}/decisions"),
                serde_json::json!({"text": "We ship on Friday"}),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{decision}");
        assert!(
            decision["summary_id"].is_null(),
            "a hand-recorded decision has no summary behind it"
        );

        let (_, listed) = call(&app, get(&format!("/v1/meetings/{meeting}/decisions"))).await;
        assert_eq!(listed.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_malformed_id_is_a_400_not_a_500() {
        let (app, _state) = app();
        let (status, body) = call(&app, get("/v1/tickets/not-a-real-id")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    // ------------------------------------------------------------------ voice prints

    /// Off until switched on, and a missing setting is off.
    ///
    /// A voice print identifies the other people in a meeting, who never installed this app.
    /// An upgrade that adds the key must not read its own absence as permission.
    #[tokio::test]
    async fn voiceprints_are_off_by_default() {
        let (app, _) = app();
        let (status, body) = call(&app, get("/v1/voiceprints")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["enabled"], false);
        assert_eq!(body["stored"], 0);
    }

    /// The behaviour the switch has to have to deserve the word off: turning it off erases
    /// what was already collected. Stopping future collection while keeping the identifiers
    /// already on disk is exactly what people are right to suspect.
    #[tokio::test]
    async fn turning_it_off_erases_what_was_stored() {
        let (app, state) = app();

        let (status, _) = call(
            &app,
            json_request(
                "POST",
                "/v1/voiceprints",
                serde_json::json!({"enabled": true}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Two people with prints, written directly — enrolment runs in the capture pipeline.
        {
            let db = state.db().await;
            let people = PersonRepository::new(&db);
            for name in ["Ana", "Ravi"] {
                let person = people.find_or_create_by_name(name).expect("person");
                people
                    .set_voice_print(person.id, &[0.1, 0.2, 0.3], "test-model")
                    .expect("store");
            }
        }

        let (_, body) = call(&app, get("/v1/voiceprints")).await;
        assert_eq!(body["stored"], 2, "seeding failed: {body}");

        let (status, body) = call(
            &app,
            json_request(
                "POST",
                "/v1/voiceprints",
                serde_json::json!({"enabled": false}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["enabled"], false);
        assert_eq!(
            body["stored"], 0,
            "off must erase, not merely stop collecting"
        );

        // And the people themselves survive — this forgets a voice, not a colleague.
        let db = state.db().await;
        assert_eq!(PersonRepository::new(&db).list().expect("list").len(), 2);
    }

    #[tokio::test]
    async fn voiceprints_can_be_erased_without_switching_off() {
        let (app, state) = app();
        call(
            &app,
            json_request(
                "POST",
                "/v1/voiceprints",
                serde_json::json!({"enabled": true}),
            ),
        )
        .await;

        {
            let db = state.db().await;
            let people = PersonRepository::new(&db);
            let person = people.find_or_create_by_name("Mei").expect("person");
            people
                .set_voice_print(person.id, &[0.4, 0.5], "test-model")
                .expect("store");
        }

        let (status, body) = call(
            &app,
            Request::builder()
                .method("DELETE")
                .uri("/v1/voiceprints")
                .body(Body::empty())
                .expect("request"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["erased"], 1);

        let (_, after) = call(&app, get("/v1/voiceprints")).await;
        assert_eq!(after["stored"], 0);
        assert_eq!(
            after["enabled"], true,
            "erasing is not the same as switching off"
        );
    }

    // ------------------------------------------------------------ trash

    async fn seed_note(state: &Arc<AppState>, title: &str) -> Id {
        let db = state.db().await;
        NoteRepository::new(&db)
            .create(notewise_storage::NewNote {
                project_id: None,
                title: title.into(),
                body: format!("body of {title}"),
            })
            .expect("note")
            .id
    }

    fn delete(uri: &str) -> Request<Body> {
        Request::builder()
            .method("DELETE")
            .uri(uri)
            .body(Body::empty())
            .expect("request")
    }

    #[tokio::test]
    async fn deleting_a_note_trashes_it_and_it_can_be_restored() {
        let (app, state) = app();
        let id = seed_note(&state, "Recoverable").await;

        let (status, trashed) = call(&app, delete(&format!("/v1/notes/{id}"))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            !trashed["deleted_at"].is_null(),
            "a plain DELETE should trash, not destroy: {trashed}"
        );

        let (_, trash) = call(&app, get("/v1/trash")).await;
        assert_eq!(trash.as_array().expect("array").len(), 1);

        let (status, restored) = call(
            &app,
            json_request(
                "POST",
                &format!("/v1/notes/{id}/restore"),
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(restored["deleted_at"].is_null());

        let (_, trash) = call(&app, get("/v1/trash")).await;
        assert!(trash.as_array().expect("array").is_empty());
    }

    /// The destructive reading of `DELETE` has to be asked for by name.
    #[tokio::test]
    async fn purge_destroys_the_note_outright() {
        let (app, state) = app();
        let id = seed_note(&state, "Doomed").await;

        let (status, _) = call(&app, delete(&format!("/v1/notes/{id}?purge=true"))).await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = call(&app, get(&format!("/v1/notes/{id}"))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (_, trash) = call(&app, get("/v1/trash")).await;
        assert!(
            trash.as_array().expect("array").is_empty(),
            "a purged note should not land in the trash"
        );
    }

    #[tokio::test]
    async fn emptying_the_trash_destroys_what_is_in_it_and_nothing_else() {
        let (app, state) = app();
        let kept = seed_note(&state, "Kept").await;
        let discarded = seed_note(&state, "Discarded").await;

        call(&app, delete(&format!("/v1/notes/{discarded}"))).await;

        let (status, body) = call(&app, delete("/v1/trash")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["deleted"], 1);

        let (status, _) = call(&app, get(&format!("/v1/notes/{kept}"))).await;
        assert_eq!(status, StatusCode::OK, "the live note should survive");
        let (status, _) = call(&app, get(&format!("/v1/notes/{discarded}"))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// Editing a trashed note must fail rather than quietly restore it — the notes editor
    /// autosaves on a timer, so a save can land after the delete.
    #[tokio::test]
    async fn a_trashed_note_rejects_edits() {
        let (app, state) = app();
        let id = seed_note(&state, "Gone").await;
        call(&app, delete(&format!("/v1/notes/{id}"))).await;

        let (status, _) = call(
            &app,
            json_request(
                "PUT",
                &format!("/v1/notes/{id}"),
                serde_json::json!({"title": "Back", "body": "resurrected"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (_, note) = call(&app, get(&format!("/v1/notes/{id}"))).await;
        assert_eq!(note["title"], "Gone");
        assert!(!note["deleted_at"].is_null());
    }

    // ------------------------------------------------------------ meeting notes

    #[tokio::test]
    async fn a_meeting_lists_the_notes_that_reference_it() {
        let (app, state) = app();
        let meeting = seed_meeting(&state, "Planning", 1_700_000_000).await;
        let other = seed_meeting(&state, "Retro", 1_700_003_600).await;

        let linked = seed_note(&state, "My notes").await;
        let unlinked = seed_note(&state, "Unrelated").await;
        {
            let db = state.db().await;
            Graph::new(&db)
                .connect(
                    NodeRef::new(NodeKind::Note, linked),
                    EdgeKind::References,
                    NodeRef::new(NodeKind::Meeting, meeting),
                )
                .expect("link");
        }

        let (status, notes) = call(&app, get(&format!("/v1/meetings/{meeting}/notes"))).await;
        assert_eq!(status, StatusCode::OK);
        let notes = notes.as_array().expect("array");
        assert_eq!(notes.len(), 1, "only the linked note: {notes:?}");
        assert_eq!(notes[0]["id"], linked.to_string());
        assert_ne!(notes[0]["id"], unlinked.to_string());

        let (_, none) = call(&app, get(&format!("/v1/meetings/{other}/notes"))).await;
        assert!(none.as_array().expect("array").is_empty());
    }

    #[tokio::test]
    async fn a_trashed_note_leaves_its_meeting_tab() {
        let (app, state) = app();
        let meeting = seed_meeting(&state, "Standup", 1_700_000_000).await;
        let note = seed_note(&state, "Scratch").await;
        {
            let db = state.db().await;
            Graph::new(&db)
                .connect(
                    NodeRef::new(NodeKind::Note, note),
                    EdgeKind::References,
                    NodeRef::new(NodeKind::Meeting, meeting),
                )
                .expect("link");
        }

        call(&app, delete(&format!("/v1/notes/{note}"))).await;

        let (_, notes) = call(&app, get(&format!("/v1/meetings/{meeting}/notes"))).await;
        assert!(
            notes.as_array().expect("array").is_empty(),
            "a trashed note should not show on the meeting"
        );
    }

    /// Purging must take the note's edges with it. Nothing cascades for the edge table, so
    /// without an explicit detach the meeting would keep listing a note that no longer exists.
    #[tokio::test]
    async fn purging_a_note_removes_its_edges() {
        let (app, state) = app();
        let meeting = seed_meeting(&state, "Kickoff", 1_700_000_000).await;
        let note = seed_note(&state, "Doomed").await;
        {
            let db = state.db().await;
            Graph::new(&db)
                .connect(
                    NodeRef::new(NodeKind::Note, note),
                    EdgeKind::References,
                    NodeRef::new(NodeKind::Meeting, meeting),
                )
                .expect("link");
        }

        call(&app, delete(&format!("/v1/notes/{note}?purge=true"))).await;

        {
            let db = state.db().await;
            assert_eq!(
                Graph::new(&db).edge_count().expect("count"),
                0,
                "the note's edge should have gone with it"
            );
        }

        let (_, notes) = call(&app, get(&format!("/v1/meetings/{meeting}/notes"))).await;
        assert!(notes.as_array().expect("array").is_empty());
    }
}
