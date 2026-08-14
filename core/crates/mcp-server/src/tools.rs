//! The MCP tool surface.
//!
//! Every tool here is **read-only**. An agent can search and traverse a user's workspace but
//! cannot mutate it — write access through an unattended agent is a much larger trust
//! decision than read access, and belongs behind an explicit opt-in rather than arriving as a
//! side effect of connecting an MCP client.

use serde_json::{json, Value};

use notewise_graph::{Graph, NodeKind, NodeRef};
use notewise_storage::{
    Database, Id, MeetingRepository, NoteRepository, SearchRepository, SummaryRepository,
};

use crate::error::{McpError, Result};

/// Declared tools, in the shape MCP's `tools/list` expects.
pub fn definitions() -> Vec<Value> {
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
pub fn call(db: &Database, name: &str, args: &Value) -> Result<Value> {
    match name {
        "list_meetings" => list_meetings(db, args),
        "get_transcript" => get_transcript(db, args),
        "get_summary" => get_summary(db, args),
        "search" => search(db, args),
        "find_related" => find_related(db, args),
        "list_notes" => list_notes(db, args),
        other => Err(McpError::UnknownTool(other.to_string())),
    }
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
        for def in definitions() {
            let name = def["name"].as_str().unwrap();
            // Missing required args give InvalidParams; the point is that no tool is
            // declared without a matching handler.
            let err = call(&db, name, &json!({}));
            assert!(
                !matches!(err, Err(McpError::UnknownTool(_))),
                "'{name}' is declared but has no handler"
            );
        }
    }

    #[test]
    fn tool_definitions_are_well_formed() {
        for def in definitions() {
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
            call(&db(), "delete_everything", &json!({})),
            Err(McpError::UnknownTool(_))
        ));
    }

    #[test]
    fn list_meetings_returns_seeded_meetings() {
        let db = db();
        seeded_meeting(&db);

        let result = call(&db, "list_meetings", &json!({})).unwrap();
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
            call(&db(), "get_transcript", &json!({})),
            Err(McpError::InvalidParams(_))
        ));
    }

    #[test]
    fn a_malformed_id_is_invalid_params() {
        assert!(matches!(
            call(
                &db(),
                "get_transcript",
                &json!({ "meeting_id": "not-a-uuid" })
            ),
            Err(McpError::InvalidParams(_))
        ));
    }

    #[test]
    fn an_unsummarized_meeting_reports_that_rather_than_erroring() {
        let db = db();
        let id = seeded_meeting(&db);

        let result = call(&db, "get_summary", &json!({ "meeting_id": id.to_string() })).unwrap();
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

        let result = call(&db, "search", &json!({ "query": "Postgres" })).unwrap();
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
        );
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn limits_are_clamped() {
        assert_eq!(arg_u32(&json!({"limit": 10_000}), "limit", 20, 1, 100), 100);
        assert_eq!(arg_u32(&json!({"limit": 0}), "limit", 20, 1, 100), 1);
        assert_eq!(arg_u32(&json!({}), "limit", 20, 1, 100), 20);
    }

    #[test]
    fn the_tool_surface_is_read_only() {
        // Adding a mutating tool is a trust decision that should be deliberate, not a
        // side effect of adding a handler. This guards against it happening by accident.
        for def in definitions() {
            let name = def["name"].as_str().unwrap();
            assert!(
                !["create", "delete", "update", "write", "send", "remove"]
                    .iter()
                    .any(|verb| name.starts_with(verb)),
                "'{name}' looks like a mutating tool; see the module docs before adding one"
            );
        }
    }
}
