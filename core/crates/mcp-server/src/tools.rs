//! The MCP tool surface.
//!
//! **Read-only by default.** An agent can search and traverse a user's workspace; it cannot
//! change anything unless the session was started with [`WriteAccess::Allowed`]. Write access
//! through an unattended agent is a much larger trust decision than read access, so it is an
//! explicit opt-in rather than something that arrives with connecting an MCP client.
//!
//! Even with writes allowed, **nothing here deletes**. The mutating tools create rows or
//! change a status field. An agent that files a wrong ticket costs a user thirty seconds; one
//! that deletes a meeting's action items costs them work they cannot recover. Deletion lives
//! on the HTTP surface, where a human is driving.

use serde_json::{json, Value};

use notewise_graph::{EdgeKind, Graph, NodeKind, NodeRef};
use notewise_storage::{
    Database, Id, MeetingRepository, MeetingSeriesRepository, NewActionItem, NewNote, NewTicket,
    NoteRepository, SearchRepository, SummaryRepository, TicketRepository, WorkStatus,
};

use crate::error::{McpError, Result};

/// Whether this session may mutate the workspace.
///
/// Read access and write access are different trust decisions, and connecting an MCP client
/// grants only the first. An agent that can search your meetings is a convenience; an agent
/// that can file tickets and edit notes unattended is a different proposition, and it should
/// not arrive as a side effect of adding a server to a config file.
///
/// Defaults to [`WriteAccess::Denied`]. The CLI turns it on with `notewise mcp --allow-writes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WriteAccess {
    #[default]
    Denied,
    Allowed,
}

impl WriteAccess {
    pub fn allowed(&self) -> bool {
        matches!(self, WriteAccess::Allowed)
    }
}

/// Tools that change stored state.
///
/// Kept as data rather than inferred from the name so the check cannot drift from the
/// dispatch table: adding a mutating handler without listing it here fails a test.
const MUTATING_TOOLS: &[&str] = &[
    "create_action_item",
    "update_action_item",
    "promote_action_item",
    "create_ticket",
    "create_note",
];

pub fn is_mutating(name: &str) -> bool {
    MUTATING_TOOLS.contains(&name)
}

/// Declared tools, in the shape MCP's `tools/list` expects.
pub fn definitions(writes: WriteAccess) -> Vec<Value> {
    let mut defs = read_only_definitions();
    if writes.allowed() {
        defs.extend(mutating_definitions());
    }
    defs
}

fn read_only_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "list_meetings",
            "description": "List recent meetings, newest first. Returns id, title, start time, \
                            and whether the meeting is still recording.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "Maximum meetings to return (1-100, default 20)"
                    }
                },
            },
        }),
        json!({
            "name": "get_transcript",
            "description": "Get the full transcript of one meeting as plain text, \
                            speaker-prefixed where speaker separation has run.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "meeting_id": { "type": "string", "description": "The meeting's id" }
                },
                "required": ["meeting_id"],
            },
        }),
        json!({
            "name": "get_summary",
            "description": "Get the most recent summary of a meeting, along with the decisions \
                            and action items extracted from it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "meeting_id": { "type": "string", "description": "The meeting's id" }
                },
                "required": ["meeting_id"],
            },
        }),
        json!({
            "name": "search",
            "description": "Full-text search across notes, tickets, and meeting transcripts. \
                            Use this to find material before reasoning about it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Text to search for" },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum results (1-50, default 10)"
                    }
                },
                "required": ["query"],
            },
        }),
        json!({
            "name": "find_related",
            "description": "Find everything connected to an entity — the notes, summaries, \
                            decisions, and tickets that reference it or came from it. Use this \
                            to gather context around a meeting rather than searching blindly.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "description": "Entity kind, e.g. 'meeting', 'note', 'ticket'"
                    },
                    "id": { "type": "string", "description": "The entity's id" },
                    "depth": {
                        "type": "integer",
                        "description": "How many hops to traverse (1-6, default 2)"
                    }
                },
                "required": ["kind", "id"],
            },
        }),
        json!({
            "name": "list_notes",
            "description": "List recent notes, most recently updated first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "Maximum notes to return (1-100, default 20)"
                    }
                },
            },
        }),
        json!({
            "name": "list_action_items",
            "description": "List the action items from a meeting, including ones added by \
                            hand and ones whose summary has since been regenerated. Use this \
                            before completing or promoting an item, to get its id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "meeting_id": { "type": "string", "description": "The meeting's id" },
                    "open_only": {
                        "type": "boolean",
                        "description": "Only items still todo or in progress (default false)"
                    }
                },
                "required": ["meeting_id"],
            },
        }),
        json!({
            "name": "meeting_brief",
            "description": "What a recurring meeting is still carrying: open action items \
                            from earlier meetings in the same series, and the previous \
                            instance's decisions. Returns empty for a one-off meeting. \
                            Derived from stored state, so it invents nothing.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "meeting_id": { "type": "string", "description": "The meeting's id" },
                },
                "required": ["meeting_id"],
            },
        }),
    ]
}

/// Tools that change stored state. Offered only under [`WriteAccess::Allowed`].
///
/// Deliberately no delete tools, at any access level. An agent that files a wrong ticket
/// costs a user thirty seconds; an agent that deletes a meeting's action items costs them
/// work they cannot recover. Deletion stays on the HTTP surface, where a human is driving.
/// Nothing here removes anything — every tool below either creates a row or changes a
/// status field.
fn mutating_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "create_action_item",
            "description": "Record an action item against a meeting. Use when the user asks \
                            you to capture follow-up work that the summary missed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "meeting_id": { "type": "string", "description": "The meeting's id" },
                    "text": { "type": "string", "description": "What needs doing" },
                    "owner": { "type": "string", "description": "Who owns it, if known" },
                },
                "required": ["meeting_id", "text"],
            },
        }),
        json!({
            "name": "update_action_item",
            "description": "Change an action item's status or owner. Status is one of todo, \
                            in_progress, done, cancelled.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action_item_id": { "type": "string", "description": "The item's id" },
                    "status": { "type": "string", "description": "New status" },
                    "owner": { "type": "string", "description": "New owner" },
                },
                "required": ["action_item_id"],
            },
        }),
        json!({
            "name": "promote_action_item",
            "description": "Turn an action item into a tracked ticket, linking the two. The \
                            action item is kept — it is the record that the meeting produced \
                            this work. Calling twice returns the existing ticket rather than \
                            filing a duplicate.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action_item_id": { "type": "string", "description": "The item's id" },
                },
                "required": ["action_item_id"],
            },
        }),
        json!({
            "name": "create_ticket",
            "description": "File a ticket. Pass meeting_id to link it back to the meeting it \
                            came out of, so it shows up in that meeting's related items.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Short summary of the work" },
                    "description": { "type": "string", "description": "Detail, if any" },
                    "owner": { "type": "string", "description": "Who owns it, if known" },
                    "meeting_id": {
                        "type": "string",
                        "description": "Meeting this came out of, to link them"
                    },
                },
                "required": ["title"],
            },
        }),
        json!({
            "name": "create_note",
            "description": "Create a workspace note. Body is plain text or markdown.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "The note's title" },
                    "body": { "type": "string", "description": "The note's content" },
                },
                "required": ["title", "body"],
            },
        }),
    ]
}

fn arg_str(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| McpError::InvalidParams(format!("'{key}' is required and must be a string")))
}

fn arg_id(args: &Value, key: &str) -> Result<Id> {
    let raw = arg_str(args, key)?;
    raw.parse()
        .map_err(|_| McpError::InvalidParams(format!("'{raw}' is not a valid id")))
}

fn arg_u32(args: &Value, key: &str, default: u32, min: u32, max: u32) -> u32 {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|v| (v as u32).clamp(min, max))
        .unwrap_or(default)
}

/// Dispatch a `tools/call`.
pub fn call(db: &Database, name: &str, args: &Value, writes: WriteAccess) -> Result<Value> {
    // Enforced here, not only by omitting the tool from `definitions`. A client is free to
    // call a name it was never offered, so hiding a tool is not the same as refusing it.
    if is_mutating(name) && !writes.allowed() {
        return Err(McpError::WriteDenied(name.to_string()));
    }

    match name {
        "list_meetings" => list_meetings(db, args),
        "get_transcript" => get_transcript(db, args),
        "get_summary" => get_summary(db, args),
        "search" => search(db, args),
        "find_related" => find_related(db, args),
        "list_notes" => list_notes(db, args),
        "list_action_items" => list_action_items(db, args),
        "meeting_brief" => meeting_brief(db, args),
        "create_action_item" => create_action_item(db, args),
        "update_action_item" => update_action_item(db, args),
        "promote_action_item" => promote_action_item(db, args),
        "create_ticket" => create_ticket(db, args),
        "create_note" => create_note(db, args),
        other => Err(McpError::UnknownTool(other.to_string())),
    }
}

fn action_item_json(item: &notewise_storage::ActionItem) -> Value {
    json!({
        "id": item.id.to_string(),
        "meeting_id": item.meeting_id.to_string(),
        "text": item.text,
        "owner": item.owner,
        "status": item.status.as_str(),
        "due_at": item.due_at,
    })
}

fn list_action_items(db: &Database, args: &Value) -> Result<Value> {
    let meeting_id = arg_id(args, "meeting_id")?;
    let repo = SummaryRepository::new(db);

    let items = if args
        .get("open_only")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        repo.open_action_items_for_meeting(meeting_id)?
    } else {
        repo.action_items_for_meeting(meeting_id)?
    };

    Ok(json!({
        "action_items": items.iter().map(action_item_json).collect::<Vec<_>>()
    }))
}

fn meeting_brief(db: &Database, args: &Value) -> Result<Value> {
    let meeting_id = arg_id(args, "meeting_id")?;
    let meeting = MeetingRepository::new(db).get(meeting_id)?;

    let Some(series_id) = meeting.series_id else {
        return Ok(json!({
            "series": null,
            "unfinished_business": [],
            "recent_decisions": [],
        }));
    };

    let series = MeetingSeriesRepository::new(db);
    let previous = series.previous_meeting(series_id, meeting_id)?;
    let decisions = match previous {
        Some(prev) => SummaryRepository::new(db).decisions_for_meeting(prev)?,
        None => Vec::new(),
    };

    Ok(json!({
        "series": series.get(series_id)?.title,
        "previous_meeting_id": previous.map(|id| id.to_string()),
        "unfinished_business": series
            .unfinished_business(series_id, meeting_id)?
            .iter()
            .map(action_item_json)
            .collect::<Vec<_>>(),
        "recent_decisions": decisions.iter().map(|d| d.text.clone()).collect::<Vec<_>>(),
    }))
}

fn create_action_item(db: &Database, args: &Value) -> Result<Value> {
    let meeting_id = arg_id(args, "meeting_id")?;
    let text = arg_str(args, "text")?;
    if text.trim().is_empty() {
        return Err(McpError::InvalidParams("'text' must not be blank".into()));
    }

    let item = SummaryRepository::new(db).add_action_item(NewActionItem {
        owner: args
            .get("owner")
            .and_then(Value::as_str)
            .map(str::to_string),
        ..NewActionItem::on_meeting(meeting_id, text)
    })?;

    Ok(action_item_json(&item))
}

fn update_action_item(db: &Database, args: &Value) -> Result<Value> {
    let id = arg_id(args, "action_item_id")?;
    let repo = SummaryRepository::new(db);

    if let Some(raw) = args.get("status").and_then(Value::as_str) {
        let status = WorkStatus::parse(raw).ok_or_else(|| {
            McpError::InvalidParams(format!(
                "'{raw}' is not a status; expected todo, in_progress, done or cancelled"
            ))
        })?;
        repo.set_action_item_status(id, status)?;
    }
    if let Some(owner) = args.get("owner").and_then(Value::as_str) {
        repo.assign_action_item(id, Some(owner))?;
    }

    Ok(action_item_json(&repo.action_item(id)?))
}

fn promote_action_item(db: &Database, args: &Value) -> Result<Value> {
    let item_id = arg_id(args, "action_item_id")?;
    let item = SummaryRepository::new(db).action_item(item_id)?;
    let graph = Graph::new(db);
    let tickets = TicketRepository::new(db);

    // Idempotent: an agent retrying after a timeout must not file a second ticket.
    let existing = graph
        .related(NodeRef::new(NodeKind::ActionItem, item_id), 1)?
        .into_iter()
        .find(|n| n.via == EdgeKind::BecameTicket && n.node.kind == NodeKind::Ticket);
    if let Some(found) = existing {
        let ticket = tickets.get(found.node.id)?;
        return Ok(json!({
            "id": ticket.id.to_string(),
            "title": ticket.title,
            "status": ticket.status.as_str(),
            "already_existed": true,
        }));
    }

    let ticket = tickets.create(NewTicket {
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

    Ok(json!({
        "id": ticket.id.to_string(),
        "title": ticket.title,
        "status": ticket.status.as_str(),
        "already_existed": false,
    }))
}

fn create_ticket(db: &Database, args: &Value) -> Result<Value> {
    let title = arg_str(args, "title")?;
    if title.trim().is_empty() {
        return Err(McpError::InvalidParams("'title' must not be blank".into()));
    }

    let ticket = TicketRepository::new(db).create(NewTicket {
        project_id: None,
        title,
        description: args
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        owner: args
            .get("owner")
            .and_then(Value::as_str)
            .map(str::to_string),
        due_at: None,
    })?;

    if let Some(raw) = args.get("meeting_id").and_then(Value::as_str) {
        let meeting_id: Id = raw
            .parse()
            .map_err(|_| McpError::InvalidParams(format!("'{raw}' is not a valid id")))?;
        Graph::new(db).connect(
            NodeRef::new(NodeKind::Ticket, ticket.id),
            EdgeKind::DerivedFrom,
            NodeRef::new(NodeKind::Meeting, meeting_id),
        )?;
    }

    Ok(json!({
        "id": ticket.id.to_string(),
        "title": ticket.title,
        "status": ticket.status.as_str(),
    }))
}

fn create_note(db: &Database, args: &Value) -> Result<Value> {
    let title = arg_str(args, "title")?;
    let body = arg_str(args, "body")?;
    if title.trim().is_empty() {
        return Err(McpError::InvalidParams("'title' must not be blank".into()));
    }

    let note = NoteRepository::new(db).create(NewNote {
        project_id: None,
        title,
        body,
    })?;

    Ok(json!({
        "id": note.id.to_string(),
        "title": note.title,
    }))
}

fn list_meetings(db: &Database, args: &Value) -> Result<Value> {
    let limit = arg_u32(args, "limit", 20, 1, 100);
    let meetings = MeetingRepository::new(db).list_recent(limit)?;

    Ok(json!({
        "meetings": meetings.iter().map(|m| json!({
            "id": m.id.to_string(),
            "title": m.title,
            "started_at": m.started_at,
            "duration_ms": m.duration_ms(),
            "is_recording": m.is_recording(),
        })).collect::<Vec<_>>()
    }))
}

fn get_transcript(db: &Database, args: &Value) -> Result<Value> {
    let meeting_id = arg_id(args, "meeting_id")?;
    let repo = MeetingRepository::new(db);
    let meeting = repo.get(meeting_id)?;

    Ok(json!({
        "meeting_id": meeting.id.to_string(),
        "title": meeting.title,
        "transcript": repo.transcript_text(meeting_id)?,
    }))
}

fn get_summary(db: &Database, args: &Value) -> Result<Value> {
    let meeting_id = arg_id(args, "meeting_id")?;
    MeetingRepository::new(db).get(meeting_id)?;

    let repo = SummaryRepository::new(db);
    let Some(summary) = repo.latest_for_meeting(meeting_id)? else {
        // Not an error — an unsummarized meeting is a normal state, and telling the agent
        // so is more useful than an error it has to interpret.
        return Ok(json!({
            "meeting_id": meeting_id.to_string(),
            "summary": Value::Null,
            "note": "This meeting has not been summarized yet.",
        }));
    };

    Ok(json!({
        "meeting_id": meeting_id.to_string(),
        "summary": summary.text,
        "model": summary.model,
        "decisions": repo.decisions(summary.id)?.iter().map(|d| json!({
            "text": d.text,
            "reasoning": d.reasoning,
        })).collect::<Vec<_>>(),
        "action_items": repo.action_items(summary.id)?.iter().map(|a| json!({
            "text": a.text,
            "owner": a.owner,
            "status": a.status.as_str(),
            "due_at": a.due_at,
        })).collect::<Vec<_>>(),
    }))
}

fn search(db: &Database, args: &Value) -> Result<Value> {
    let query = arg_str(args, "query")?;
    let limit = arg_u32(args, "limit", 10, 1, 50);

    let hits = SearchRepository::new(db).search(&query, limit)?;
    Ok(json!({
        "query": query,
        "hits": hits.iter().map(|h| json!({
            "kind": h.entity_kind,
            "id": h.entity_id.to_string(),
            "title": h.title,
            "snippet": h.snippet,
        })).collect::<Vec<_>>()
    }))
}

fn find_related(db: &Database, args: &Value) -> Result<Value> {
    let kind_raw = arg_str(args, "kind")?;
    let kind = NodeKind::parse(&kind_raw).ok_or_else(|| {
        McpError::InvalidParams(format!(
            "unknown entity kind '{kind_raw}'; expected one of: {}",
            NodeKind::ALL
                .iter()
                .map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;
    let id = arg_id(args, "id")?;
    let depth = arg_u32(args, "depth", 2, 1, Graph::MAX_DEPTH);

    let related = Graph::new(db).related(NodeRef::new(kind, id), depth)?;

    Ok(json!({
        "start": { "kind": kind.as_str(), "id": id.to_string() },
        "related": related.iter().map(|r| json!({
            "kind": r.node.kind.as_str(),
            "id": r.node.id.to_string(),
            "distance": r.distance,
            "via": r.via.as_str(),
        })).collect::<Vec<_>>()
    }))
}

fn list_notes(db: &Database, args: &Value) -> Result<Value> {
    let limit = arg_u32(args, "limit", 20, 1, 100);
    let notes = NoteRepository::new(db).list_recent(limit)?;

    Ok(json!({
        "notes": notes.iter().map(|n| json!({
            "id": n.id.to_string(),
            "title": n.title,
            "updated_at": n.updated_at,
        })).collect::<Vec<_>>()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use notewise_graph::EdgeKind;
    use notewise_storage::{MeetingSource, NewMeeting, NewNote, NewSummary, NewTranscriptSegment};

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn seeded_meeting(db: &Database) -> Id {
        let repo = MeetingRepository::new(db);
        let meeting = repo
            .create(NewMeeting {
                project_id: None,
                title: "Infra sync".into(),
                source: MeetingSource::Combined,
                started_at: Utc.timestamp_opt(1_700_000_000, 0).single().unwrap(),
            })
            .unwrap();

        repo.add_segment(NewTranscriptSegment {
            meeting_id: meeting.id,
            speaker: Some("Alex".into()),
            text: "We will migrate to Postgres.".into(),
            start_ms: 0,
            end_ms: 3000,
            confidence: None,
        })
        .unwrap();

        meeting.id
    }

    #[test]
    fn every_declared_tool_is_dispatchable() {
        let db = db();
        for def in definitions(WriteAccess::Denied) {
            let name = def["name"].as_str().unwrap();
            // Missing required args give InvalidParams; the point is that no tool is
            // declared without a matching handler.
            let err = call(&db, name, &json!({}), WriteAccess::Denied);
            assert!(
                !matches!(err, Err(McpError::UnknownTool(_))),
                "'{name}' is declared but has no handler"
            );
        }
    }

    #[test]
    fn tool_definitions_are_well_formed() {
        for def in definitions(WriteAccess::Denied) {
            assert!(def["name"].is_string());
            let description = def["description"].as_str().expect("description");
            assert!(
                description.len() > 40,
                "'{}' needs a description an agent can route on",
                def["name"]
            );
            assert_eq!(def["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn unknown_tools_are_reported_as_such() {
        assert!(matches!(
            call(&db(), "delete_everything", &json!({}), WriteAccess::Denied),
            Err(McpError::UnknownTool(_))
        ));
    }

    #[test]
    fn list_meetings_returns_seeded_meetings() {
        let db = db();
        seeded_meeting(&db);

        let result = call(&db, "list_meetings", &json!({}), WriteAccess::Denied).unwrap();
        let meetings = result["meetings"].as_array().unwrap();

        assert_eq!(meetings.len(), 1);
        assert_eq!(meetings[0]["title"], "Infra sync");
        assert_eq!(meetings[0]["is_recording"], true);
    }

    #[test]
    fn get_transcript_returns_speaker_prefixed_text() {
        let db = db();
        let id = seeded_meeting(&db);

        let result = call(
            &db,
            "get_transcript",
            &json!({ "meeting_id": id.to_string() }),
            WriteAccess::Denied,
        )
        .unwrap();
        assert_eq!(
            result["transcript"].as_str().unwrap().trim(),
            "Alex: We will migrate to Postgres."
        );
    }

    #[test]
    fn a_missing_required_argument_is_invalid_params_not_a_panic() {
        assert!(matches!(
            call(&db(), "get_transcript", &json!({}), WriteAccess::Denied),
            Err(McpError::InvalidParams(_))
        ));
    }

    #[test]
    fn a_malformed_id_is_invalid_params() {
        assert!(matches!(
            call(
                &db(),
                "get_transcript",
                &json!({ "meeting_id": "not-a-uuid" }),
                WriteAccess::Denied,
            ),
            Err(McpError::InvalidParams(_))
        ));
    }

    #[test]
    fn an_unsummarized_meeting_reports_that_rather_than_erroring() {
        let db = db();
        let id = seeded_meeting(&db);

        let result = call(
            &db,
            "get_summary",
            &json!({ "meeting_id": id.to_string() }),
            WriteAccess::Denied,
        )
        .unwrap();
        assert!(result["summary"].is_null());
        assert!(result["note"]
            .as_str()
            .unwrap()
            .contains("not been summarized"));
    }

    #[test]
    fn get_summary_returns_decisions_and_action_items() {
        let db = db();
        let meeting_id = seeded_meeting(&db);

        let repo = SummaryRepository::new(&db);
        let summary = repo
            .create(NewSummary {
                meeting_id,
                text: "Agreed to migrate.".into(),
                model: "mock".into(),
            })
            .unwrap();
        repo.add_decision(notewise_storage::NewDecision {
            meeting_id,
            summary_id: Some(summary.id),
            text: "Migrate to Postgres".into(),
            reasoning: Some("Better JSON support".into()),
            decided_at: None,
        })
        .unwrap();
        repo.add_action_item(notewise_storage::NewActionItem {
            meeting_id,
            summary_id: Some(summary.id),
            text: "Draft the migration plan".into(),
            owner: Some("alex".into()),
            owner_person_id: None,
            due_at: None,
        })
        .unwrap();

        let result = call(
            &db,
            "get_summary",
            &json!({ "meeting_id": meeting_id.to_string() }),
            WriteAccess::Denied,
        )
        .unwrap();

        assert_eq!(result["summary"], "Agreed to migrate.");
        assert_eq!(result["decisions"][0]["reasoning"], "Better JSON support");
        assert_eq!(result["action_items"][0]["owner"], "alex");
        assert_eq!(result["action_items"][0]["status"], "todo");
    }

    #[test]
    fn search_finds_transcript_text() {
        let db = db();
        seeded_meeting(&db);

        let result = call(
            &db,
            "search",
            &json!({ "query": "Postgres" }),
            WriteAccess::Denied,
        )
        .unwrap();
        let hits = result["hits"].as_array().unwrap();

        assert!(!hits.is_empty());
        assert!(hits.iter().any(|h| h["kind"] == "transcript_segment"));
    }

    #[test]
    fn find_related_walks_the_graph() {
        let db = db();
        let meeting_id = seeded_meeting(&db);

        let note = NoteRepository::new(&db)
            .create(NewNote {
                project_id: None,
                title: "Follow-up".into(),
                body: "Recap".into(),
            })
            .unwrap();
        Graph::new(&db)
            .connect(
                NodeRef::new(NodeKind::Note, note.id),
                EdgeKind::References,
                NodeRef::new(NodeKind::Meeting, meeting_id),
            )
            .unwrap();

        let result = call(
            &db,
            "find_related",
            &json!({ "kind": "meeting", "id": meeting_id.to_string() }),
            WriteAccess::Denied,
        )
        .unwrap();

        let related = result["related"].as_array().unwrap();
        assert_eq!(related.len(), 1);
        assert_eq!(related[0]["kind"], "note");
        assert_eq!(related[0]["via"], "references");
    }

    #[test]
    fn an_unknown_entity_kind_lists_the_valid_ones() {
        let err = call(
            &db(),
            "find_related",
            &json!({ "kind": "wormhole", "id": Id::new().to_string() }),
            WriteAccess::Denied,
        )
        .expect_err("should be rejected");

        let message = err.to_string();
        assert!(message.contains("meeting"), "{message}");
        assert!(message.contains("ticket"), "{message}");
    }

    #[test]
    fn traversal_depth_is_clamped_rather_than_erroring() {
        let db = db();
        let id = seeded_meeting(&db);

        // 99 exceeds Graph::MAX_DEPTH; clamping keeps an over-eager agent working.
        let result = call(
            &db,
            "find_related",
            &json!({ "kind": "meeting", "id": id.to_string(), "depth": 99 }),
            WriteAccess::Denied,
        );
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn limits_are_clamped() {
        assert_eq!(arg_u32(&json!({"limit": 10_000}), "limit", 20, 1, 100), 100);
        assert_eq!(arg_u32(&json!({"limit": 0}), "limit", 20, 1, 100), 1);
        assert_eq!(arg_u32(&json!({}), "limit", 20, 1, 100), 20);
    }

    /// The default surface must stay read-only.
    ///
    /// This is the original guard, kept with its original meaning: adding a mutating tool
    /// should be a deliberate act, not a side effect of adding a handler. What changed is
    /// that mutating tools now exist — so the assertion moved from "no such tool exists" to
    /// "no such tool is reachable without an explicit opt-in", which is the property the
    /// trust story actually rests on.
    #[test]
    fn the_tool_surface_is_read_only() {
        for def in definitions(WriteAccess::Denied) {
            let name = def["name"].as_str().unwrap();
            assert!(
                !["create", "delete", "update", "write", "send", "remove"]
                    .iter()
                    .any(|verb| name.starts_with(verb)),
                "'{name}' looks like a mutating tool; see the module docs before adding one"
            );
            assert!(
                !is_mutating(name),
                "'{name}' mutates and must not be offered to a read-only session"
            );
        }
    }

    /// Hiding a tool is not refusing it. A client can call any name it likes, so the check
    /// has to live in `call`, and this is what proves it does.
    #[test]
    fn a_mutating_tool_is_refused_even_when_called_directly() {
        let db = db();
        let meeting = seeded_meeting(&db);

        for name in MUTATING_TOOLS {
            let err = call(
                &db,
                name,
                &json!({
                    "meeting_id": meeting.to_string(),
                    "action_item_id": meeting.to_string(),
                    "text": "should not happen",
                    "title": "should not happen",
                    "body": "should not happen",
                }),
                WriteAccess::Denied,
            )
            .expect_err("a read-only session must refuse this");

            assert!(
                matches!(err, McpError::WriteDenied(_)),
                "'{name}' gave {err:?} rather than refusing"
            );
        }
    }

    #[test]
    fn write_access_reveals_the_mutating_tools() {
        let read_only: Vec<String> = definitions(WriteAccess::Denied)
            .iter()
            .map(|d| d["name"].as_str().unwrap().to_string())
            .collect();
        let with_writes: Vec<String> = definitions(WriteAccess::Allowed)
            .iter()
            .map(|d| d["name"].as_str().unwrap().to_string())
            .collect();

        assert!(
            with_writes.len() > read_only.len(),
            "allowing writes should offer more tools"
        );
        for name in MUTATING_TOOLS {
            assert!(
                with_writes.iter().any(|n| n == name),
                "'{name}' is listed as mutating but never offered"
            );
            assert!(
                !read_only.iter().any(|n| n == name),
                "'{name}' leaked into the read-only surface"
            );
        }
    }

    /// `MUTATING_TOOLS` gates `call`, and `mutating_definitions` decides what is offered.
    /// If they drift, a tool is either unreachable or ungated — the second being the
    /// dangerous direction.
    #[test]
    fn the_mutating_list_matches_the_mutating_definitions() {
        let mut declared: Vec<String> = mutating_definitions()
            .iter()
            .map(|d| d["name"].as_str().unwrap().to_string())
            .collect();
        let mut listed: Vec<String> = MUTATING_TOOLS.iter().map(|s| s.to_string()).collect();

        declared.sort();
        listed.sort();
        assert_eq!(
            declared, listed,
            "MUTATING_TOOLS and mutating_definitions() disagree"
        );
    }

    /// No tool deletes, at any access level. A wrong ticket costs seconds; deleted action
    /// items cost work that cannot be recovered.
    #[test]
    fn nothing_deletes_even_with_writes_allowed() {
        for def in definitions(WriteAccess::Allowed) {
            let name = def["name"].as_str().unwrap();
            assert!(
                !["delete", "remove", "purge", "clear", "drop"]
                    .iter()
                    .any(|verb| name.contains(verb)),
                "'{name}' looks destructive; deletion belongs on the HTTP surface"
            );
        }
    }

    #[test]
    fn a_write_session_can_file_and_complete_work() {
        let db = db();
        let meeting = seeded_meeting(&db);

        let item = call(
            &db,
            "create_action_item",
            &json!({ "meeting_id": meeting.to_string(), "text": "Chase the vendor" }),
            WriteAccess::Allowed,
        )
        .unwrap();
        assert_eq!(item["status"], "todo");

        let updated = call(
            &db,
            "update_action_item",
            &json!({ "action_item_id": item["id"], "status": "done" }),
            WriteAccess::Allowed,
        )
        .unwrap();
        assert_eq!(updated["status"], "done");
    }

    /// An agent retrying after a timeout must not file the work twice.
    #[test]
    fn promoting_twice_returns_the_same_ticket() {
        let db = db();
        let meeting = seeded_meeting(&db);
        let item = call(
            &db,
            "create_action_item",
            &json!({ "meeting_id": meeting.to_string(), "text": "Chase the vendor" }),
            WriteAccess::Allowed,
        )
        .unwrap();
        let args = json!({ "action_item_id": item["id"] });

        let first = call(&db, "promote_action_item", &args, WriteAccess::Allowed).unwrap();
        let second = call(&db, "promote_action_item", &args, WriteAccess::Allowed).unwrap();

        assert_eq!(first["id"], second["id"], "filed a duplicate ticket");
        assert_eq!(first["already_existed"], false);
        assert_eq!(second["already_existed"], true);
    }

    #[test]
    fn a_blank_title_is_refused_rather_than_stored() {
        let err = call(
            &db(),
            "create_ticket",
            &json!({ "title": "   " }),
            WriteAccess::Allowed,
        )
        .expect_err("a blank ticket helps nobody");
        assert!(matches!(err, McpError::InvalidParams(_)), "{err:?}");
    }

    #[test]
    fn every_mutating_tool_is_dispatchable() {
        let db = db();
        for def in mutating_definitions() {
            let name = def["name"].as_str().unwrap();
            let err = call(&db, name, &json!({}), WriteAccess::Allowed);
            assert!(
                !matches!(err, Err(McpError::UnknownTool(_))),
                "'{name}' is declared but has no handler"
            );
        }
    }
}
