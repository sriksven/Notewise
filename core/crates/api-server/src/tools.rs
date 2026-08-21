//! External tool calls: configuring servers, proposing a call, and running a confirmed one.
//!
//! # A human is in the loop, and the loop is here
//!
//! Nothing in this module can send a tool call without a row moving from `proposed` to `confirmed`
//! first, and only a request from the interface moves it. There is no "always allow this tool", no
//! batch confirm, and no path that starts a call from a background task — see [`crate::jobs`],
//! which says the same thing from the other side.
//!
//! The reason is that an external tool's blast radius is unknowable. `mcp-server`'s `MUTATING_TOOLS`
//! can be a list because somebody reasoned about each of its tools one at a time. A tool on
//! somebody else's server might send a message, file a ticket, or charge a card, and nothing here
//! can tell which. So the gate is not "which tools are safe" but "a person looked at this call."
//!
//! # Three verbs, and they are deliberately separate
//!
//! `propose` asks a model for a call and stores it, sending nothing. `confirm` records that a human
//! approved it. `execute` sends it. `confirm` calls `execute` for the caller's convenience, but the
//! transition is written first and `execute` refuses a row that is not `confirmed` — so the
//! guarantee is enforced by the database and not by the order in which handlers happen to run.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post, put};
use axum::{Json, Router as AxumRouter};
use notewise_ai_router::{AiBackend, ChatMessage, ChatRequest, Role};
use notewise_connectors::{CredentialStore, KeychainStore, Secret};
use notewise_mcp_client::{
    parse_proposal, text_of, validate, Allowlist, McpError as ClientError, Proposal, ServerConfig,
    ToolDef, TransportKind,
};
use notewise_storage::{
    Id, McpServer, McpServerRepository, McpTransport, NewMcpServer, NewToolExecution, Outcome,
    SummaryRepository, ToolExecution, ToolExecutionRepository,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

type Shared = Arc<AppState>;

/// How many executions a listing returns by default.
const DEFAULT_HISTORY: usize = 50;

/// How many tools may go into a proposal prompt.
///
/// A user with six servers connected could otherwise put several hundred JSON Schemas in front of a
/// local model, which would both exhaust its context and make the proposal worse. Capped rather
/// than trimmed silently: the response says how many were considered.
const MAX_TOOLS_IN_PROMPT: usize = 40;

/// Keychain namespace for a server's secrets.
///
/// Environment variables for a stdio server, headers for an HTTP one. Stored as one JSON object per
/// server so reaching them costs a single keychain read, and stored *there* rather than in
/// `mcp_servers` because they are credentials — the same split routing rules make for provider keys.
fn secret_scope(server_id: Id) -> String {
    format!("mcp.{server_id}")
}

const SECRET_KEY: &str = "config";

pub fn routes() -> AxumRouter<Shared> {
    AxumRouter::new()
        .route("/v1/mcp/servers", get(list_servers).post(add_server))
        .route("/v1/mcp/servers/:id", axum::routing::delete(remove_server))
        .route("/v1/mcp/servers/:id/enabled", put(set_enabled))
        .route("/v1/mcp/servers/:id/auto-start", put(set_auto_start))
        .route("/v1/mcp/servers/:id/start", post(start_server))
        .route("/v1/mcp/servers/:id/stop", post(stop_server))
        .route("/v1/mcp/servers/:id/tools", get(discover_tools))
        .route(
            "/v1/mcp/servers/:id/tools/:tool",
            put(enable_tool).delete(disable_tool),
        )
        .route("/v1/mcp/proposals", post(propose))
        .route("/v1/mcp/executions", get(list_executions))
        .route("/v1/mcp/executions/:id/confirm", post(confirm))
        .route("/v1/mcp/executions/:id/reject", post(reject))
        .route("/v1/mcp/executions/:id/execute", post(execute_confirmed))
}

// ---------------------------------------------------------------- servers

#[derive(Debug, Serialize)]
struct ServerBody {
    id: String,
    name: String,
    transport: &'static str,
    command: Option<String>,
    args: Vec<String>,
    url: Option<String>,
    enabled: bool,
    auto_start: bool,
    /// Whether a process or session exists right now.
    running: bool,
    /// Which of its tools the user has allowed. Independent of `enabled`, so turning a server off
    /// and on again does not lose the choices.
    enabled_tools: Vec<String>,
    /// Whether secrets are held for this server. The values are never returned.
    has_secrets: bool,
}

async fn list_servers(State(state): State<Shared>) -> ApiResult<Json<Vec<ServerBody>>> {
    let servers = {
        let db = state.db().await;
        let repo = McpServerRepository::new(&db);
        let servers = repo.list()?;
        let mut with_tools = Vec::with_capacity(servers.len());
        for server in servers {
            let tools = repo.enabled_tools(server.id)?;
            with_tools.push((server, tools));
        }
        with_tools
    };

    let credentials = KeychainStore::new();
    let mut out = Vec::with_capacity(servers.len());
    for (server, enabled_tools) in servers {
        let running = state.mcp().is_running(&server.id.to_string()).await;
        let has_secrets = secrets_for(&server, &credentials).is_some_and(|s| !s.is_empty());
        out.push(describe(server, enabled_tools, running, has_secrets));
    }
    Ok(Json(out))
}

fn describe(
    server: McpServer,
    enabled_tools: Vec<String>,
    running: bool,
    has_secrets: bool,
) -> ServerBody {
    ServerBody {
        id: server.id.to_string(),
        name: server.name,
        transport: server.transport.as_str(),
        command: server.command,
        args: server.args,
        url: server.url,
        enabled: server.enabled,
        auto_start: server.auto_start,
        running,
        enabled_tools,
        has_secrets,
    }
}

#[derive(Debug, Deserialize)]
struct AddServerBody {
    name: String,
    /// `"stdio"` or `"http"`.
    transport: String,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    auto_start: Option<bool>,
    /// Environment variables for a stdio server, headers for an HTTP one.
    ///
    /// Written to the keychain and never to the database, and never read back out through this API.
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

/// Add a server. It is disabled, and none of its tools are allowed.
///
/// Connecting a client must not grant capability as a side effect of having typed a command in —
/// the same reasoning `mcp-server` applies to its own write scope.
async fn add_server(
    State(state): State<Shared>,
    Json(body): Json<AddServerBody>,
) -> ApiResult<Json<ServerBody>> {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::BadRequest("a server needs a name".into()));
    }

    let transport = McpTransport::parse(&body.transport).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "'{}' is not a transport; use 'stdio' or 'http'",
            body.transport
        ))
    })?;

    let server = {
        let db = state.db().await;
        McpServerRepository::new(&db).create(NewMcpServer {
            name,
            transport,
            command: body.command.filter(|c| !c.trim().is_empty()),
            args: body.args,
            url: body.url.filter(|u| !u.trim().is_empty()),
            auto_start: body.auto_start.unwrap_or(true),
        })?
    };

    // Checked after the row exists so the message can name the server, and checked at all so a
    // stdio entry with no command is refused here rather than at the first attempt to use it.
    if let Err(e) = config_of(&server, BTreeMap::new()).and_then(|c| {
        c.validate()
            .map_err(|e| ApiError::BadRequest(e.to_string()))
    }) {
        let db = state.db().await;
        McpServerRepository::new(&db).delete(server.id)?;
        return Err(e);
    }

    if !body.secrets.is_empty() {
        store_secrets(server.id, &body.secrets)?;
    }

    Ok(Json(describe(
        server,
        Vec::new(),
        false,
        !body.secrets.is_empty(),
    )))
}

#[derive(Debug, Deserialize)]
struct EnabledBody {
    enabled: bool,
}

/// Turn a server on or off.
///
/// Turning it off stops it and withdraws all of its tools, without forgetting which ones were
/// allowed — a user who disables a server in a hurry should not have to reconstruct their choices.
async fn set_enabled(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<EnabledBody>,
) -> ApiResult<Json<ServerBody>> {
    let id = parse_id(&id)?;
    let server = {
        let db = state.db().await;
        McpServerRepository::new(&db).set_enabled(id, body.enabled)?
    };

    if !body.enabled {
        state.mcp().stop(&id.to_string()).await;
    }

    let tools = {
        let db = state.db().await;
        McpServerRepository::new(&db).enabled_tools(id)?
    };
    let running = state.mcp().is_running(&id.to_string()).await;
    Ok(Json(describe(server, tools, running, false)))
}

#[derive(Debug, Deserialize)]
struct AutoStartBody {
    auto_start: bool,
}

async fn set_auto_start(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<AutoStartBody>,
) -> ApiResult<Json<ServerBody>> {
    let id = parse_id(&id)?;
    let (server, tools) = {
        let db = state.db().await;
        let repo = McpServerRepository::new(&db);
        (
            repo.set_auto_start(id, body.auto_start)?,
            repo.enabled_tools(id)?,
        )
    };

    let running = state.mcp().is_running(&id.to_string()).await;
    Ok(Json(describe(server, tools, running, false)))
}

async fn remove_server(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&id)?;

    state.mcp().stop(&id.to_string()).await;
    {
        let db = state.db().await;
        McpServerRepository::new(&db).delete(id)?;
    }
    // Left behind, the secret would outlive every reference to it and be unreachable through this
    // API — a credential nobody can see and nobody can delete.
    let _ = KeychainStore::new().delete(&secret_scope(id), SECRET_KEY);

    Ok(Json(serde_json::json!({ "deleted": true })))
}

#[derive(Debug, Serialize)]
struct DiscoveredTools {
    tools: Vec<DiscoveredTool>,
    /// Why the list is empty, when it is. Returned as a field rather than an error status so the
    /// interface can render the server's row with the reason attached instead of a failed request.
    error: Option<String>,
    running: bool,
}

#[derive(Debug, Serialize)]
struct DiscoveredTool {
    name: String,
    description: Option<String>,
    input_schema: Value,
    enabled: bool,
}

/// Ask a server what it can do.
///
/// Starts it if it is allowed to start. A server pinned `auto_start: false` answers with the reason
/// it did not, so the interface can offer a start button rather than an empty list.
async fn discover_tools(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<DiscoveredTools>> {
    let id = parse_id(&id)?;
    let (config, enabled) = load_config(&state, id).await?;

    let listed = state.mcp().tools(&config).await;
    let running = state.mcp().is_running(&id.to_string()).await;

    Ok(Json(match listed {
        Ok(tools) => DiscoveredTools {
            tools: tools
                .into_iter()
                .map(|tool| DiscoveredTool {
                    enabled: enabled.contains(&tool.name),
                    name: tool.name,
                    description: tool.description,
                    input_schema: tool.input_schema,
                })
                .collect(),
            error: None,
            running,
        },
        Err(e) => DiscoveredTools {
            tools: Vec::new(),
            error: Some(e.to_string()),
            running,
        },
    }))
}

async fn start_server(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<DiscoveredTools>> {
    let id = parse_id(&id)?;
    let (config, enabled) = load_config(&state, id).await?;

    let tools = state
        .mcp()
        .start(&config)
        .await
        .map_err(client_error_to_api)?;

    Ok(Json(DiscoveredTools {
        tools: tools
            .into_iter()
            .map(|tool| DiscoveredTool {
                enabled: enabled.contains(&tool.name),
                name: tool.name,
                description: tool.description,
                input_schema: tool.input_schema,
            })
            .collect(),
        error: None,
        running: true,
    }))
}

async fn stop_server(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&id)?;
    let stopped = state.mcp().stop(&id.to_string()).await;
    Ok(Json(serde_json::json!({ "stopped": stopped })))
}

async fn enable_tool(
    State(state): State<Shared>,
    Path((id, tool)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    let repo = McpServerRepository::new(&db);
    repo.get(id)?;
    repo.enable_tool(id, &tool)?;
    Ok(Json(serde_json::json!({
        "enabled_tools": repo.enabled_tools(id)?
    })))
}

async fn disable_tool(
    State(state): State<Shared>,
    Path((id, tool)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    let repo = McpServerRepository::new(&db);
    repo.get(id)?;
    repo.disable_tool(id, &tool)?;
    Ok(Json(serde_json::json!({
        "enabled_tools": repo.enabled_tools(id)?
    })))
}

// ---------------------------------------------------------------- proposing

#[derive(Debug, Deserialize)]
struct ProposeBody {
    /// The action item this is for. Its text becomes the task, and the record is linked to it.
    #[serde(default)]
    action_item_id: Option<String>,
    /// A task in the user's own words, for a proposal that is not about a stored action item.
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Serialize)]
struct ProposalBody {
    /// The stored proposal, when there is one.
    execution: Option<ExecutionBody>,
    /// Why there is not, when there is not. Shown to the user verbatim.
    declined: Option<String>,
    /// How many tools the model was shown, so "it did not use my tool" has an answer.
    tools_considered: usize,
}

/// Ask a model for one tool call.
///
/// Sends nothing. The result is a row a human has to approve.
async fn propose(
    State(state): State<Shared>,
    Json(body): Json<ProposeBody>,
) -> ApiResult<Json<ProposalBody>> {
    let (task, action_item_id) = match (&body.action_item_id, &body.text) {
        (Some(raw), _) => {
            let id = parse_id(raw)?;
            let db = state.db().await;
            let item = SummaryRepository::new(&db).action_item(id)?;
            (item.text, Some(id))
        }
        (None, Some(text)) if !text.trim().is_empty() => (text.trim().to_string(), None),
        _ => {
            return Err(ApiError::BadRequest(
                "give either an action_item_id or some text".into(),
            ))
        }
    };

    let available = gather_tools(&state).await?;
    if available.is_empty() {
        return Ok(Json(ProposalBody {
            execution: None,
            declined: Some(
                "No external tools are enabled. Add a server and enable its tools in Settings."
                    .into(),
            ),
            tools_considered: 0,
        }));
    }

    let considered = available.len().min(MAX_TOOLS_IN_PROMPT);
    let allowlist = Allowlist::from_pairs(
        available
            .iter()
            .map(|(server, tool)| (server.name.clone(), tool.name.clone())),
    );

    let ai = state.ai();
    let mut messages = vec![ChatMessage::user(
        "Propose one tool call for the task, or say none.",
    )];

    // Two attempts. A model that produced something unusable gets told exactly what was wrong,
    // which is the same recovery the agent loop uses and which in practice fixes it. A third
    // attempt has never been the difference between working and not, and each one costs a
    // round trip against a local model the user is waiting on.
    for attempt in 0..2 {
        let request = ChatRequest::new(messages.clone())
            .with_context(vec![proposal_prompt(&task, &available[..considered])]);
        let reply = ai.chat(&request).await?;

        let observation = match read_reply(&reply.text) {
            Ok(Proposed::None { reason }) => {
                return Ok(Json(ProposalBody {
                    execution: None,
                    declined: Some(
                        reason.unwrap_or_else(|| {
                            "The model found no tool that fits this task.".into()
                        }),
                    ),
                    tools_considered: considered,
                }))
            }
            Ok(Proposed::One(proposal)) => {
                match check_proposal(&proposal, &allowlist, &available) {
                    Ok(()) => {
                        let execution = store_proposal(&state, action_item_id, &proposal).await?;
                        return Ok(Json(ProposalBody {
                            execution: Some(ExecutionBody::from(execution)),
                            declined: None,
                            tools_considered: considered,
                        }));
                    }
                    Err(problem) => problem,
                }
            }
            Err(problem) => problem,
        };

        if attempt == 0 {
            messages.push(ChatMessage {
                role: Role::Assistant,
                content: reply.text,
            });
            messages.push(ChatMessage {
                role: Role::User,
                content: format!("{observation}\n\nTry again with one JSON object."),
            });
        } else {
            // Reported rather than retried forever, and reported as a declined proposal rather
            // than a server error: nothing is broken here, the model could not produce a call.
            return Ok(Json(ProposalBody {
                execution: None,
                declined: Some(format!(
                    "The model could not produce a usable call: {observation}"
                )),
                tools_considered: considered,
            }));
        }
    }

    unreachable!("the loop returns on both attempts")
}

/// What a model replied with.
#[derive(Debug, PartialEq)]
enum Proposed {
    One(Proposal),
    None { reason: Option<String> },
}

/// Read a model's reply into a proposal, a refusal, or a complaint to send back.
///
/// Pure, so every shape a model produces is a unit test rather than something discovered against a
/// live backend.
fn read_reply(reply: &str) -> Result<Proposed, String> {
    // "None" is checked first and by hand, because it is not a `Proposal` and `parse_proposal`
    // would report a missing `server` field for a reply that was perfectly clear.
    if let Some(reason) = declined_reason(reply) {
        return Ok(Proposed::None { reason });
    }

    match parse_proposal(reply) {
        Ok(proposal) => Ok(Proposed::One(proposal)),
        Err(e) => Err(e.to_string()),
    }
}

/// Whether the reply says "no tool fits", and why.
fn declined_reason(reply: &str) -> Option<Option<String>> {
    let value: Value = serde_json::from_str(first_object(reply)?.as_str()).ok()?;

    let says_none = value
        .get("action")
        .and_then(Value::as_str)
        .is_some_and(|a| a.eq_ignore_ascii_case("none"))
        || value.get("none").and_then(Value::as_bool) == Some(true)
        || value
            .get("tool")
            .and_then(Value::as_str)
            .is_some_and(|t| t.eq_ignore_ascii_case("none"));

    if !says_none {
        return None;
    }

    Some(
        value
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_string),
    )
}

/// The first balanced JSON object in a string.
///
/// A smaller version of what `parse_proposal` does internally, needed here because the "none"
/// reply is not a proposal and so cannot be recognised by parsing one.
fn first_object(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.iter().position(|c| *c == '{')?;

    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, ch) in chars[start..].iter().enumerate() {
        if in_string {
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(chars[start..=start + offset].iter().collect());
                }
            }
            _ => {}
        }
    }
    None
}

/// Check a proposal against what is allowed and what the tool's schema says.
///
/// Both, and in that order, and the message on failure is written for the *model* rather than the
/// user: it goes back as an observation.
fn check_proposal(
    proposal: &Proposal,
    allowlist: &Allowlist,
    available: &[(McpServer, ToolDef)],
) -> Result<(), String> {
    if let Err(e) = allowlist.require(&proposal.server, &proposal.tool) {
        return Err(format!(
            "{e}. Use only a server and tool from the list you were given."
        ));
    }

    let tool = available
        .iter()
        .find(|(server, tool)| server.name == proposal.server && tool.name == proposal.tool)
        .map(|(_, tool)| tool)
        .ok_or_else(|| format!("'{}' is not in the list.", proposal.tool))?;

    let arguments = if proposal.arguments.is_null() {
        Value::Object(Default::default())
    } else {
        proposal.arguments.clone()
    };

    let problems = validate(&tool.input_schema, &arguments);
    if problems.is_empty() {
        return Ok(());
    }

    Err(format!(
        "The arguments for '{}' are not valid: {}",
        proposal.tool,
        problems
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    ))
}

async fn store_proposal(
    state: &Shared,
    action_item_id: Option<Id>,
    proposal: &Proposal,
) -> ApiResult<ToolExecution> {
    let db = state.db().await;
    let server = McpServerRepository::new(&db)
        .by_name(&proposal.server)?
        .ok_or_else(|| ApiError::NotFound(format!("no server named '{}'", proposal.server)))?;

    let arguments = if proposal.arguments.is_null() {
        "{}".to_string()
    } else {
        serde_json::to_string(&proposal.arguments)
            .map_err(|e| ApiError::Internal(format!("could not store the arguments: {e}")))?
    };

    Ok(ToolExecutionRepository::new(&db).propose(NewToolExecution {
        action_item_id,
        server_id: server.id,
        tool_name: proposal.tool.clone(),
        arguments,
    })?)
}

/// Every tool that could actually be called right now.
///
/// A server that will not start contributes nothing and does not fail the request: its tools are
/// absent from the proposal rather than proposed and then refused, because a confirmation for a
/// call that cannot run spends the only thing this feature costs — the user's attention.
async fn gather_tools(state: &Shared) -> ApiResult<Vec<(McpServer, ToolDef)>> {
    let (servers, allowed) = {
        let db = state.db().await;
        let repo = McpServerRepository::new(&db);
        (repo.list()?, repo.allowed_pairs()?)
    };

    let mut out = Vec::new();
    for server in servers.into_iter().filter(|s| s.enabled) {
        let allowed_here: Vec<&String> = allowed
            .iter()
            .filter(|(name, _)| *name == server.name)
            .map(|(_, tool)| tool)
            .collect();
        if allowed_here.is_empty() {
            continue;
        }

        let config = match config_of(
            &server,
            secrets_for(&server, &KeychainStore::new()).unwrap_or_default(),
        ) {
            Ok(config) => config,
            Err(e) => {
                tracing::warn!(server = %server.name, error = %e, "skipping a server in proposals");
                continue;
            }
        };

        match state.mcp().tools(&config).await {
            Ok(tools) => {
                for tool in tools {
                    if allowed_here.iter().any(|name| **name == tool.name) {
                        out.push((server.clone(), tool));
                    }
                }
            }
            Err(e) => {
                tracing::warn!(server = %server.name, error = %e, "a server contributed no tools");
            }
        }
    }

    Ok(out)
}

/// The instruction block, as one string so the whole contract is visible at once.
fn proposal_prompt(task: &str, available: &[(McpServer, ToolDef)]) -> String {
    let mut tools = String::new();
    for (server, tool) in available {
        tools.push_str(&format!(
            "- server \"{}\", tool \"{}\"{}\n  arguments schema: {}\n",
            server.name,
            tool.name,
            tool.description
                .as_deref()
                .map(|d| format!(" — {d}"))
                .unwrap_or_default(),
            tool.input_schema
        ));
    }

    format!(
        "You propose ONE external tool call for a task taken from a meeting. A person will read \
your proposal and decide whether it runs. Nothing happens until they do.

THE TASK:
{task}

TOOLS YOU MAY USE — nothing else exists:
{tools}
Reply with EXACTLY ONE JSON object and no other text.

To propose a call:
{{\"server\": \"<server>\", \"tool\": \"<tool>\", \"arguments\": {{...}}, \"reason\": \"<one line>\"}}

If no tool here fits the task:
{{\"action\": \"none\", \"reason\": \"<one line saying why>\"}}

Rules:
- Use only a server and tool from the list above, spelled exactly as written.
- Every argument must satisfy that tool's schema. Do not add fields the schema does not list.
- Do not invent identifiers, project keys, or email addresses you were not given. If the task \
needs one you do not have, answer none and say which.
- The person reviewing this sees your arguments verbatim. Write ones they can check."
    )
}

// ---------------------------------------------------------------- confirming and running

#[derive(Debug, Serialize)]
struct ExecutionBody {
    id: String,
    action_item_id: Option<String>,
    server_id: String,
    tool_name: String,
    /// The arguments, exactly as stored and exactly as they will be sent.
    arguments: Value,
    status: &'static str,
    result: Option<String>,
    proposed_at: String,
    executed_at: Option<String>,
    /// Whether it is genuinely unknown if this took effect. Set only for a timeout.
    outcome_unknown: bool,
}

impl From<ToolExecution> for ExecutionBody {
    fn from(execution: ToolExecution) -> Self {
        Self {
            id: execution.id.to_string(),
            action_item_id: execution.action_item_id.map(|id| id.to_string()),
            server_id: execution.server_id.to_string(),
            tool_name: execution.tool_name,
            // A value rather than a string, so the interface can render the fields as fields. On
            // the impossible path where it will not parse, the raw text goes through instead of
            // the request failing — a record of something that ran must stay readable.
            arguments: serde_json::from_str(&execution.arguments)
                .unwrap_or(Value::String(execution.arguments.clone())),
            status: execution.status.as_str(),
            result: execution.result,
            proposed_at: execution.proposed_at.to_rfc3339(),
            executed_at: execution.executed_at.map(|t| t.to_rfc3339()),
            outcome_unknown: execution.status == notewise_storage::ExecutionStatus::Unknown,
        }
    }
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    #[serde(default)]
    limit: Option<usize>,
    /// Only what is waiting on a human.
    #[serde(default)]
    pending: bool,
}

async fn list_executions(
    State(state): State<Shared>,
    Query(query): Query<HistoryQuery>,
) -> ApiResult<Json<Vec<ExecutionBody>>> {
    let db = state.db().await;
    let repo = ToolExecutionRepository::new(&db);

    let executions = if query.pending {
        repo.awaiting_confirmation()?
    } else {
        repo.list(query.limit.unwrap_or(DEFAULT_HISTORY).min(500))?
    };

    Ok(Json(
        executions.into_iter().map(ExecutionBody::from).collect(),
    ))
}

/// A human approved it — so run it.
///
/// The transition is written before anything is sent, and [`execute`] refuses a row that is not
/// `confirmed`. So the "no execution without confirmation" guarantee is the database's and not
/// this handler's.
async fn confirm(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<ExecutionBody>> {
    let id = parse_id(&id)?;

    let confirmed = {
        let db = state.db().await;
        ToolExecutionRepository::new(&db).confirm(id)?
    };

    let finished = execute(&state, confirmed).await?;
    Ok(Json(ExecutionBody::from(finished)))
}

async fn reject(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<ExecutionBody>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    let rejected = ToolExecutionRepository::new(&db).reject(id)?;
    Ok(Json(ExecutionBody::from(rejected)))
}

/// Run a call that was confirmed and never sent.
///
/// The only case that produces one is a crash between the confirmation and the call. Deliberately
/// not a retry: a `failed` row stays failed, because a failed external call may have taken effect
/// and a retry that duplicates a filed ticket is worse than a visible failure a person resolves.
async fn execute_confirmed(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<ExecutionBody>> {
    let id = parse_id(&id)?;
    let execution = {
        let db = state.db().await;
        ToolExecutionRepository::new(&db).get(id)?
    };

    if execution.status != notewise_storage::ExecutionStatus::Confirmed {
        return Err(ApiError::Conflict(format!(
            "this call is {}, and only a confirmed call can be sent",
            execution.status.as_str()
        )));
    }

    Ok(Json(ExecutionBody::from(execute(&state, execution).await?)))
}

/// Send a confirmed call and record what came back.
///
/// A call that fails is not an error of this request: the request did its job, and the row says
/// what happened. So this returns `Ok` with a `failed` or `unknown` row rather than a 502 — the
/// interface has to show the user which of those it was, and an error body cannot carry it.
async fn execute(state: &Shared, execution: ToolExecution) -> ApiResult<ToolExecution> {
    let id = execution.id;

    let (server, allowlist) = {
        let db = state.db().await;
        let repo = McpServerRepository::new(&db);
        let server = repo.get(execution.server_id)?;
        let allowlist = Allowlist::from_pairs(repo.allowed_pairs()?);
        (server, allowlist)
    };

    // A server turned off between the confirmation and the call. The row is failed rather than the
    // request, because the user needs to see which call did not go out and why.
    if !server.enabled {
        return fail(state, id, format!("'{}' is turned off", server.name)).await;
    }

    let secrets = secrets_for(&server, &KeychainStore::new()).unwrap_or_default();
    let config = config_of(&server, secrets)?;

    let arguments = match execution.arguments_value() {
        Ok(arguments) => arguments,
        Err(e) => return fail(state, id, e.to_string()).await,
    };

    let sent = state
        .mcp()
        .call(&config, &allowlist, &execution.tool_name, arguments)
        .await;

    let outcome = match sent {
        Ok(result) => Outcome::Succeeded {
            // The text the server produced, when it produced any, because that is what a person
            // reads. The whole payload is kept when it did not.
            result: text_of(&result).unwrap_or_else(|| result.to_string()),
        },
        Err(e) if e.outcome_unknown() => Outcome::Unknown {
            detail: e.to_string(),
        },
        Err(e) => Outcome::Failed {
            error: e.to_string(),
        },
    };

    let db = state.db().await;
    Ok(ToolExecutionRepository::new(&db).finish(id, outcome)?)
}

async fn fail(state: &Shared, id: Id, error: String) -> ApiResult<ToolExecution> {
    let db = state.db().await;
    Ok(ToolExecutionRepository::new(&db).finish(id, Outcome::Failed { error })?)
}

// ---------------------------------------------------------------- configuration

/// Build a client config for a stored row.
async fn load_config(state: &Shared, id: Id) -> ApiResult<(ServerConfig, Vec<String>)> {
    let (server, enabled) = {
        let db = state.db().await;
        let repo = McpServerRepository::new(&db);
        let server = repo.get(id)?;
        let enabled = repo.enabled_tools(id)?;
        (server, enabled)
    };

    let secrets = secrets_for(&server, &KeychainStore::new()).unwrap_or_default();
    Ok((config_of(&server, secrets)?, enabled))
}

/// Turn a row plus its secrets into something the client can connect with.
///
/// Pure, so the mapping from a database row to a transport is testable without a keychain or a
/// process.
fn config_of(server: &McpServer, secrets: BTreeMap<String, String>) -> ApiResult<ServerConfig> {
    let transport = match server.transport {
        McpTransport::Stdio => TransportKind::Stdio {
            command: server.command.clone().ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "'{}' is a stdio server with no command",
                    server.name
                ))
            })?,
            args: server.args.clone(),
            env: secrets,
        },
        McpTransport::Http => TransportKind::Http {
            url: server.url.clone().ok_or_else(|| {
                ApiError::BadRequest(format!("'{}' is an http server with no url", server.name))
            })?,
            headers: secrets,
        },
    };

    Ok(ServerConfig {
        id: server.id.to_string(),
        name: server.name.clone(),
        transport,
        auto_start: server.auto_start,
    })
}

/// Read a server's secrets.
///
/// `None` when there are none, and *also* `None` when the credential store cannot be reached. That
/// is a deliberate trade: refusing to start every server because a headless CI machine has no
/// keychain would make the whole feature untestable, while a server that needs a token and does not
/// get one fails its own handshake with a message from the server itself. The failure is logged
/// either way.
fn secrets_for(
    server: &McpServer,
    credentials: &dyn CredentialStore,
) -> Option<BTreeMap<String, String>> {
    match credentials.get(&secret_scope(server.id), SECRET_KEY) {
        Ok(Some(secret)) => match serde_json::from_str(secret.expose()) {
            Ok(map) => Some(map),
            Err(e) => {
                tracing::warn!(server = %server.name, error = %e, "stored secrets are not readable");
                None
            }
        },
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(server = %server.name, error = %e, "could not read the keychain");
            None
        }
    }
}

fn store_secrets(server_id: Id, secrets: &BTreeMap<String, String>) -> ApiResult<()> {
    let payload = serde_json::to_string(secrets)
        .map_err(|e| ApiError::Internal(format!("could not encode the secrets: {e}")))?;

    KeychainStore::new()
        .set(&secret_scope(server_id), SECRET_KEY, &Secret::new(payload))
        .map_err(|e| ApiError::Internal(format!("could not put the secrets in the keychain: {e}")))
}

fn parse_id(raw: &str) -> ApiResult<Id> {
    raw.parse()
        .map_err(|_| ApiError::BadRequest(format!("'{raw}' is not a valid id")))
}

/// Translate a client error at the boundary, per rule 5.
fn client_error_to_api(error: ClientError) -> ApiError {
    match &error {
        // The user's configuration is wrong, and the message says which field.
        ClientError::Misconfigured { .. } | ClientError::InvalidArguments { .. } => {
            ApiError::BadRequest(error.to_string())
        }
        // Refused on purpose. 409 rather than 403: the state is what makes it refusable, and the
        // fix is to change the state.
        ClientError::NotAllowed { .. } | ClientError::NotStarted { .. } => {
            ApiError::Conflict(error.to_string())
        }
        ClientError::UnknownServer(_) | ClientError::UnknownTool { .. } => {
            ApiError::NotFound(error.to_string())
        }
        // Somebody else's server is not working. Nothing here is broken, so not a 500.
        _ => ApiError::Internal(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------ reading a reply

    #[test]
    fn a_bare_proposal_is_read() {
        let read = read_reply(
            r#"{"server":"linear","tool":"create_issue","arguments":{"title":"x"},"reason":"asked"}"#,
        )
        .expect("reads");

        match read {
            Proposed::One(proposal) => {
                assert_eq!(proposal.server, "linear");
                assert_eq!(proposal.tool, "create_issue");
                assert_eq!(proposal.reason.as_deref(), Some("asked"));
            }
            other => panic!("expected a proposal, got {other:?}"),
        }
    }

    #[test]
    fn a_fenced_proposal_is_read() {
        let read = read_reply(
            "```json\n{\"server\":\"linear\",\"tool\":\"create_issue\",\"arguments\":{}}\n```",
        )
        .expect("reads");
        assert!(matches!(read, Proposed::One(_)));
    }

    /// "None" is a normal answer, not a parse failure.
    #[test]
    fn a_declined_reply_is_read_with_its_reason() {
        let read =
            read_reply(r#"{"action":"none","reason":"no ticket tool is enabled"}"#).expect("reads");
        assert_eq!(
            read,
            Proposed::None {
                reason: Some("no ticket tool is enabled".into())
            }
        );
    }

    /// Models say "none" several ways, and none of them should look like a broken reply.
    #[test]
    fn the_shapes_of_none_are_all_recognised() {
        for reply in [
            r#"{"action":"none"}"#,
            r#"{"action":"NONE"}"#,
            r#"{"none":true}"#,
            r#"{"tool":"none","server":"linear"}"#,
        ] {
            assert!(
                matches!(read_reply(reply), Ok(Proposed::None { .. })),
                "{reply} should read as none"
            );
        }
    }

    #[test]
    fn prose_with_no_json_becomes_an_observation_rather_than_an_error() {
        let problem = read_reply("I would rather not.").expect_err("no proposal");
        assert!(!problem.is_empty());
    }

    #[test]
    fn a_json_object_that_is_not_a_proposal_says_what_was_missing() {
        let problem = read_reply(r#"{"thoughts":"hmm"}"#).expect_err("no proposal");
        assert!(problem.contains("server"), "{problem}");
    }

    #[test]
    fn a_brace_inside_an_argument_does_not_truncate_the_object() {
        let read = read_reply(
            r#"{"server":"gmail","tool":"draft","arguments":{"body":"Use {placeholder}"}}"#,
        )
        .expect("reads");
        match read {
            Proposed::One(proposal) => {
                assert_eq!(proposal.arguments["body"], "Use {placeholder}")
            }
            other => panic!("expected a proposal, got {other:?}"),
        }
    }

    // ------------------------------------------------------------ checking a proposal

    fn tool_def() -> ToolDef {
        ToolDef {
            name: "create_issue".into(),
            description: Some("File one".into()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "title": { "type": "string" } },
                "required": ["title"]
            }),
        }
    }

    fn server_row(name: &str) -> McpServer {
        McpServer {
            id: Id::new(),
            name: name.into(),
            transport: McpTransport::Stdio,
            command: Some("linear-mcp".into()),
            args: vec![],
            url: None,
            enabled: true,
            auto_start: true,
            created_at: chrono::Utc::now(),
        }
    }

    fn proposal_of(server: &str, tool: &str, arguments: Value) -> Proposal {
        Proposal {
            server: server.into(),
            tool: tool.into(),
            arguments,
            reason: None,
        }
    }

    #[test]
    fn a_valid_proposal_for_an_allowed_tool_passes() {
        let available = vec![(server_row("linear"), tool_def())];
        let allowlist = Allowlist::from_pairs([("linear", "create_issue")]);

        assert!(check_proposal(
            &proposal_of("linear", "create_issue", serde_json::json!({"title":"x"})),
            &allowlist,
            &available,
        )
        .is_ok());
    }

    /// A model naming a tool it was not offered is corrected, not obeyed.
    #[test]
    fn a_proposal_for_a_tool_that_is_not_allowed_is_sent_back() {
        let available = vec![(server_row("linear"), tool_def())];
        let allowlist = Allowlist::from_pairs([("linear", "create_issue")]);

        let problem = check_proposal(
            &proposal_of("linear", "delete_issue", serde_json::json!({})),
            &allowlist,
            &available,
        )
        .expect_err("must refuse");

        assert!(problem.contains("delete_issue"), "{problem}");
        assert!(
            problem.contains("list"),
            "the correction has to tell it what to do instead: {problem}"
        );
    }

    #[test]
    fn a_proposal_with_a_missing_required_field_is_sent_back_naming_the_field() {
        let available = vec![(server_row("linear"), tool_def())];
        let allowlist = Allowlist::from_pairs([("linear", "create_issue")]);

        let problem = check_proposal(
            &proposal_of("linear", "create_issue", serde_json::json!({})),
            &allowlist,
            &available,
        )
        .expect_err("must refuse");
        assert!(problem.contains("title"), "{problem}");
    }

    #[test]
    fn a_proposal_naming_an_unknown_server_is_sent_back() {
        let available = vec![(server_row("linear"), tool_def())];
        let allowlist = Allowlist::from_pairs([("linear", "create_issue")]);

        assert!(check_proposal(
            &proposal_of("jira", "create_issue", serde_json::json!({"title":"x"})),
            &allowlist,
            &available,
        )
        .is_err());
    }

    // ------------------------------------------------------------ the prompt

    #[test]
    fn the_prompt_names_every_tool_with_its_schema() {
        let available = vec![(server_row("linear"), tool_def())];
        let prompt = proposal_prompt("File a ticket for the auth regression", &available);

        assert!(prompt.contains("File a ticket for the auth regression"));
        assert!(prompt.contains(r#"server "linear""#), "{prompt}");
        assert!(prompt.contains(r#"tool "create_issue""#), "{prompt}");
        assert!(prompt.contains("required"), "the schema goes in verbatim");
    }

    /// The confirmation is worthless if the arguments were invented, so the prompt says so.
    #[test]
    fn the_prompt_forbids_inventing_identifiers_and_says_a_person_will_read_it() {
        let prompt = proposal_prompt("do a thing", &[(server_row("linear"), tool_def())]);
        assert!(prompt.contains("Do not invent identifiers"), "{prompt}");
        assert!(prompt.contains("A person will read"), "{prompt}");
    }

    // ------------------------------------------------------------ configuration

    #[test]
    fn a_stdio_row_becomes_a_stdio_transport_with_its_secrets_as_environment() {
        let mut server = server_row("linear");
        server.args = vec!["--stdio".into()];
        let secrets = BTreeMap::from([("LINEAR_API_KEY".to_string(), "lin_abc".to_string())]);

        let config = config_of(&server, secrets).expect("builds");
        assert_eq!(config.name, "linear");
        assert!(config.auto_start);

        match config.transport {
            TransportKind::Stdio { command, args, env } => {
                assert_eq!(command, "linear-mcp");
                assert_eq!(args, vec!["--stdio".to_string()]);
                assert_eq!(
                    env.get("LINEAR_API_KEY").map(String::as_str),
                    Some("lin_abc")
                );
            }
            other => panic!("expected stdio, got {other:?}"),
        }
    }

    #[test]
    fn an_http_row_becomes_an_http_transport_with_its_secrets_as_headers() {
        let mut server = server_row("remote");
        server.transport = McpTransport::Http;
        server.command = None;
        server.url = Some("https://example.com/mcp".into());
        let secrets = BTreeMap::from([("authorization".to_string(), "Bearer x".to_string())]);

        let config = config_of(&server, secrets).expect("builds");
        match config.transport {
            TransportKind::Http { url, headers } => {
                assert_eq!(url, "https://example.com/mcp");
                assert_eq!(
                    headers.get("authorization").map(String::as_str),
                    Some("Bearer x")
                );
            }
            other => panic!("expected http, got {other:?}"),
        }
    }

    #[test]
    fn a_stdio_row_with_no_command_cannot_be_built() {
        let mut server = server_row("broken");
        server.command = None;
        assert!(config_of(&server, BTreeMap::new()).is_err());
    }

    #[test]
    fn the_session_key_is_the_row_id_so_a_rename_does_not_orphan_a_process() {
        let server = server_row("linear");
        let config = config_of(&server, BTreeMap::new()).expect("builds");
        assert_eq!(config.id, server.id.to_string());
    }

    /// Two servers must not share a keychain entry.
    #[test]
    fn secret_scopes_are_per_server() {
        let a = Id::new();
        let b = Id::new();
        assert_ne!(secret_scope(a), secret_scope(b));
        assert!(secret_scope(a).starts_with("mcp."));
    }

    /// A CI machine with no keychain must not make the feature untestable, and a real absence and
    /// an unreachable store both mean "no secrets".
    #[test]
    fn secrets_are_absent_rather_than_fatal_when_the_store_cannot_be_read() {
        use notewise_connectors::MemoryStore;

        let server = server_row("linear");
        let store = MemoryStore::new();
        assert!(secrets_for(&server, &store).is_none());

        store
            .set(
                &secret_scope(server.id),
                SECRET_KEY,
                &Secret::new(r#"{"TOKEN":"abc"}"#),
            )
            .expect("stores");
        assert_eq!(secrets_for(&server, &store).expect("reads")["TOKEN"], "abc");

        // Garbage in the store is not a crash either.
        store
            .set(
                &secret_scope(server.id),
                SECRET_KEY,
                &Secret::new("not json"),
            )
            .expect("stores");
        assert!(secrets_for(&server, &store).is_none());
    }

    // ------------------------------------------------------------ error translation

    #[test]
    fn a_refused_tool_is_a_conflict_and_not_a_server_error() {
        let err = client_error_to_api(ClientError::NotAllowed {
            server: "linear".into(),
            tool: "delete_issue".into(),
        });
        assert!(matches!(err, ApiError::Conflict(_)), "{err:?}");
    }

    #[test]
    fn a_broken_third_party_server_is_not_the_callers_fault() {
        let err = client_error_to_api(ClientError::SpawnFailed {
            server: "linear".into(),
            detail: "no such file".into(),
        });
        assert!(matches!(err, ApiError::Internal(_)), "{err:?}");
    }

    #[test]
    fn a_misconfigured_server_is_a_bad_request() {
        let err = client_error_to_api(ClientError::Misconfigured {
            server: "linear".into(),
            detail: "no command".into(),
        });
        assert!(matches!(err, ApiError::BadRequest(_)), "{err:?}");
    }

    /// M8, enforced rather than remembered.
    ///
    /// The autonomous agent's action set must stay exactly what it was: search, read, and write one
    /// note. An unattended loop with arbitrary external tools is the one combination the human
    /// confirmation exists to prevent, and the way it would arrive is somebody adding a case to
    /// `agent.rs`'s dispatch. So the agent's source must not mention this module or the client.
    #[test]
    fn the_autonomous_agent_cannot_reach_an_external_tool() {
        let agent = include_str!("agent.rs");

        for forbidden in [
            "mcp_client",
            "McpClient",
            "ToolExecutionRepository",
            "McpServerRepository",
            "crate::tools",
        ] {
            assert!(
                !agent.contains(forbidden),
                "agent.rs mentions '{forbidden}'. An agent that runs unattended is the last \
                 place to widen a blast radius — external tools need a human, and the agent \
                 does not have one."
            );
        }
    }
    // ------------------------------------------------------------ the whole path

    /// End to end, over HTTP, against a server that is really running.
    ///
    /// The unit tests above prove each piece in isolation, and the one thing they cannot prove is
    /// that the pieces are wired to each other: that a proposal reaches the database, that
    /// confirming it actually sends something, and that nothing is sent before then. This module
    /// does that against a child process, so "it ran" means a real server answered.
    #[cfg(unix)]
    mod end_to_end {
        use super::*;
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use http_body_util::BodyExt;
        use notewise_ai_router::{ChatRequest, Router as AiRouter};
        use notewise_storage::Database;
        use std::collections::VecDeque;
        use std::sync::Mutex as StdMutex;
        use tower::ServiceExt;

        /// A POSIX-shell MCP server publishing one tool.
        const SHELL_SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"shell","version":"0"}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Say it back","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}]}}\n' "$id"
      ;;
    *'"method":"tools/call"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"heard you"}]}}\n' "$id"
      ;;
    *)
      ;;
  esac
done
"#;

        type Script = Arc<StdMutex<VecDeque<String>>>;

        /// A backend that answers from a queue, so a proposal is deterministic.
        #[derive(Debug)]
        struct Scripted {
            replies: Script,
        }

        #[async_trait::async_trait]
        impl notewise_ai_router::AiBackend for Scripted {
            fn model_id(&self) -> &str {
                "scripted"
            }
            fn is_local(&self) -> bool {
                true
            }
            async fn summarize(
                &self,
                _: &notewise_ai_router::TranscriptInput,
            ) -> notewise_ai_router::Result<notewise_ai_router::SummaryOutput> {
                unreachable!("proposing only chats")
            }
            async fn extract_decisions(
                &self,
                _: &notewise_ai_router::TranscriptInput,
            ) -> notewise_ai_router::Result<Vec<notewise_ai_router::ExtractedDecision>>
            {
                unreachable!("proposing only chats")
            }
            async fn extract_action_items(
                &self,
                _: &notewise_ai_router::TranscriptInput,
            ) -> notewise_ai_router::Result<Vec<notewise_ai_router::ExtractedActionItem>>
            {
                unreachable!("proposing only chats")
            }
            async fn chat(
                &self,
                _: &ChatRequest,
            ) -> notewise_ai_router::Result<notewise_ai_router::ChatResponse> {
                let text =
                    self.replies.lock().unwrap().pop_front().unwrap_or_else(|| {
                        r#"{"action":"none","reason":"script exhausted"}"#.into()
                    });
                Ok(notewise_ai_router::ChatResponse {
                    text,
                    model: "scripted".into(),
                })
            }
        }

        fn app_with(replies: &[&str]) -> (AxumRouter<()>, Shared) {
            let script: Script = Arc::new(StdMutex::new(
                replies.iter().map(|r| (*r).to_string()).collect(),
            ));
            let state = Arc::new(AppState::new(
                Database::open_in_memory().expect("in-memory db"),
                AiRouter::with_backend(Box::new(Scripted { replies: script })),
            ));
            (routes().with_state(Arc::clone(&state)), state)
        }

        async fn call(
            app: &AxumRouter<()>,
            request: Request<Body>,
        ) -> (StatusCode, serde_json::Value) {
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

        fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("builds")
        }

        fn put(uri: &str, body: serde_json::Value) -> Request<Body> {
            Request::builder()
                .method("PUT")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("builds")
        }

        fn get(uri: &str) -> Request<Body> {
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("builds")
        }

        /// Add the shell server, enable it, and allow its one tool.
        async fn ready_server(app: &AxumRouter<()>) -> String {
            let (status, server) = call(
                app,
                post(
                    "/v1/mcp/servers",
                    serde_json::json!({
                        "name": "shell",
                        "transport": "stdio",
                        "command": "/bin/sh",
                        "args": ["-c", SHELL_SERVER]
                    }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{server}");

            let id = server["id"].as_str().expect("an id").to_string();
            assert_eq!(server["enabled"], false, "a new server is off");
            assert!(
                server["enabled_tools"]
                    .as_array()
                    .expect("a list")
                    .is_empty(),
                "and none of its tools are allowed"
            );

            let (status, _) = call(
                app,
                put(
                    &format!("/v1/mcp/servers/{id}/enabled"),
                    serde_json::json!({ "enabled": true }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK);

            let (status, body) = call(
                app,
                put(
                    &format!("/v1/mcp/servers/{id}/tools/echo"),
                    serde_json::json!({}),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");

            id
        }

        /// A task, a proposal, a confirmation, and a server that answered.
        #[tokio::test]
        async fn a_task_becomes_a_proposal_and_then_a_call_that_ran() {
            let (app, _state) = app_with(&[
                r#"{"server":"shell","tool":"echo","arguments":{"text":"hello"},"reason":"the task asks for it"}"#,
            ]);
            ready_server(&app).await;

            let (status, tools) = call(&app, get("/v1/mcp/servers")).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(tools[0]["enabled_tools"][0], "echo");

            let (status, proposal) = call(
                &app,
                post(
                    "/v1/mcp/proposals",
                    serde_json::json!({ "text": "say hello" }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{proposal}");
            assert_eq!(proposal["declined"], serde_json::Value::Null, "{proposal}");

            let execution = &proposal["execution"];
            assert_eq!(execution["status"], "proposed");
            assert_eq!(execution["tool_name"], "echo");
            // The arguments a person will read, as fields rather than a string.
            assert_eq!(execution["arguments"]["text"], "hello");
            assert_eq!(
                execution["executed_at"],
                serde_json::Value::Null,
                "proposing must send nothing"
            );

            let id = execution["id"].as_str().expect("an id").to_string();

            let (status, done) = call(
                &app,
                post(
                    &format!("/v1/mcp/executions/{id}/confirm"),
                    serde_json::json!({}),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{done}");
            assert_eq!(done["status"], "succeeded", "{done}");
            assert_eq!(done["result"], "heard you", "the server's own answer");
            assert!(done["executed_at"].is_string());
            assert_eq!(done["outcome_unknown"], false);
        }

        /// The guarantee, at the HTTP boundary: a proposal cannot be sent without a confirmation.
        #[tokio::test]
        async fn a_proposal_cannot_be_executed_without_being_confirmed() {
            let (app, _state) =
                app_with(&[r#"{"server":"shell","tool":"echo","arguments":{"text":"hello"}}"#]);
            ready_server(&app).await;

            let (_, proposal) = call(
                &app,
                post(
                    "/v1/mcp/proposals",
                    serde_json::json!({ "text": "say hello" }),
                ),
            )
            .await;
            let id = proposal["execution"]["id"]
                .as_str()
                .expect("an id")
                .to_string();

            let (status, body) = call(
                &app,
                post(
                    &format!("/v1/mcp/executions/{id}/execute"),
                    serde_json::json!({}),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT, "{body}");

            let (_, listed) = call(&app, get("/v1/mcp/executions")).await;
            assert_eq!(listed[0]["status"], "proposed", "and the row is untouched");
        }

        #[tokio::test]
        async fn a_rejected_proposal_is_the_end_of_it() {
            let (app, _state) =
                app_with(&[r#"{"server":"shell","tool":"echo","arguments":{"text":"hello"}}"#]);
            ready_server(&app).await;

            let (_, proposal) = call(
                &app,
                post(
                    "/v1/mcp/proposals",
                    serde_json::json!({ "text": "say hello" }),
                ),
            )
            .await;
            let id = proposal["execution"]["id"]
                .as_str()
                .expect("an id")
                .to_string();

            let (status, rejected) = call(
                &app,
                post(
                    &format!("/v1/mcp/executions/{id}/reject"),
                    serde_json::json!({}),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(rejected["status"], "rejected");
            assert_eq!(rejected["executed_at"], serde_json::Value::Null);

            let (status, _) = call(
                &app,
                post(
                    &format!("/v1/mcp/executions/{id}/confirm"),
                    serde_json::json!({}),
                ),
            )
            .await;
            assert_ne!(status, StatusCode::OK, "a rejected call cannot be revived");
        }

        /// A model naming a tool it was not offered is corrected once, then the proposal is
        /// declined rather than stored.
        #[tokio::test]
        async fn a_model_that_will_not_stay_inside_the_allowlist_produces_no_proposal() {
            let (app, _state) = app_with(&[
                r#"{"server":"shell","tool":"rm_minus_rf","arguments":{}}"#,
                r#"{"server":"shell","tool":"rm_minus_rf","arguments":{}}"#,
            ]);
            ready_server(&app).await;

            let (status, body) = call(
                &app,
                post(
                    "/v1/mcp/proposals",
                    serde_json::json!({ "text": "delete everything" }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["execution"], serde_json::Value::Null);
            assert!(
                body["declined"]
                    .as_str()
                    .expect("a reason")
                    .contains("rm_minus_rf"),
                "{body}"
            );

            let (_, listed) = call(&app, get("/v1/mcp/executions")).await;
            assert!(
                listed.as_array().expect("a list").is_empty(),
                "nothing unusable is stored: {listed}"
            );
        }

        /// The pending queue is what the interface asks for, and it holds only what is waiting.
        #[tokio::test]
        async fn the_pending_queue_holds_only_unanswered_proposals() {
            let (app, _state) = app_with(&[
                r#"{"server":"shell","tool":"echo","arguments":{"text":"one"}}"#,
                r#"{"server":"shell","tool":"echo","arguments":{"text":"two"}}"#,
            ]);
            ready_server(&app).await;

            let (_, first) = call(
                &app,
                post("/v1/mcp/proposals", serde_json::json!({ "text": "one" })),
            )
            .await;
            call(
                &app,
                post("/v1/mcp/proposals", serde_json::json!({ "text": "two" })),
            )
            .await;

            let id = first["execution"]["id"]
                .as_str()
                .expect("an id")
                .to_string();
            call(
                &app,
                post(
                    &format!("/v1/mcp/executions/{id}/reject"),
                    serde_json::json!({}),
                ),
            )
            .await;

            let (status, pending) = call(&app, get("/v1/mcp/executions?pending=true")).await;
            assert_eq!(status, StatusCode::OK);
            let pending = pending.as_array().expect("a list");
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0]["arguments"]["text"], "two");
        }

        /// A tool enabled on a server that is then turned off reaches nothing.
        #[tokio::test]
        async fn turning_a_server_off_withdraws_its_tools_without_forgetting_them() {
            let (app, _state) = app_with(&[]);
            let id = ready_server(&app).await;

            let (status, _) = call(
                &app,
                put(
                    &format!("/v1/mcp/servers/{id}/enabled"),
                    serde_json::json!({ "enabled": false }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK);

            let (_, body) = call(
                &app,
                post(
                    "/v1/mcp/proposals",
                    serde_json::json!({ "text": "say hello" }),
                ),
            )
            .await;
            assert!(
                body["declined"]
                    .as_str()
                    .expect("a reason")
                    .contains("No external tools are enabled"),
                "{body}"
            );

            let (_, servers) = call(&app, get("/v1/mcp/servers")).await;
            assert_eq!(
                servers[0]["enabled_tools"][0], "echo",
                "the choice survives being turned off"
            );
        }

        /// Discovery reports why a list is empty rather than pretending a server has no tools.
        #[tokio::test]
        async fn a_server_that_cannot_start_says_so_instead_of_looking_empty() {
            let (app, _state) = app_with(&[]);

            let (_, server) = call(
                &app,
                post(
                    "/v1/mcp/servers",
                    serde_json::json!({
                        "name": "missing",
                        "transport": "stdio",
                        "command": "/nonexistent/not-a-real-mcp-server"
                    }),
                ),
            )
            .await;
            let id = server["id"].as_str().expect("an id");

            let (status, body) = call(&app, get(&format!("/v1/mcp/servers/{id}/tools"))).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert!(body["tools"].as_array().expect("a list").is_empty());
            assert!(
                body["error"]
                    .as_str()
                    .expect("a reason")
                    .contains("missing"),
                "{body}"
            );
            assert_eq!(body["running"], false);
        }

        /// Discovery starts the server and marks which tools are already allowed.
        #[tokio::test]
        async fn discovery_lists_a_servers_tools_and_which_are_allowed() {
            let (app, _state) = app_with(&[]);
            let id = ready_server(&app).await;

            let (status, body) = call(&app, get(&format!("/v1/mcp/servers/{id}/tools"))).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["tools"][0]["name"], "echo");
            assert_eq!(body["tools"][0]["enabled"], true);
            assert_eq!(body["tools"][0]["input_schema"]["required"][0], "text");
            assert_eq!(body["running"], true);
        }

        /// A server pinned off is not started by a proposal, and says why when asked.
        #[tokio::test]
        async fn a_server_pinned_off_contributes_nothing_until_it_is_started() {
            let (app, _state) = app_with(&[]);

            let (_, server) = call(
                &app,
                post(
                    "/v1/mcp/servers",
                    serde_json::json!({
                        "name": "pinned",
                        "transport": "stdio",
                        "command": "/bin/sh",
                        "args": ["-c", SHELL_SERVER],
                        "auto_start": false
                    }),
                ),
            )
            .await;
            let id = server["id"].as_str().expect("an id").to_string();

            let (_, body) = call(&app, get(&format!("/v1/mcp/servers/{id}/tools"))).await;
            assert!(
                body["error"]
                    .as_str()
                    .expect("a reason")
                    .contains("not running"),
                "{body}"
            );

            let (status, started) = call(
                &app,
                post(
                    &format!("/v1/mcp/servers/{id}/start"),
                    serde_json::json!({}),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{started}");
            assert_eq!(started["tools"][0]["name"], "echo");
        }

        #[tokio::test]
        async fn a_transport_that_is_not_one_is_refused() {
            let (app, _state) = app_with(&[]);
            let (status, _) = call(
                &app,
                post(
                    "/v1/mcp/servers",
                    serde_json::json!({ "name": "odd", "transport": "carrier-pigeon" }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }

        /// A stdio entry with no command could never run, so it is refused rather than stored.
        #[tokio::test]
        async fn a_stdio_server_with_no_command_is_not_stored() {
            let (app, _state) = app_with(&[]);
            let (status, _) = call(
                &app,
                post(
                    "/v1/mcp/servers",
                    serde_json::json!({ "name": "broken", "transport": "stdio" }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);

            let (_, servers) = call(&app, get("/v1/mcp/servers")).await;
            assert!(servers.as_array().expect("a list").is_empty());
        }

        #[tokio::test]
        async fn a_proposal_with_neither_a_task_nor_an_action_item_is_a_bad_request() {
            let (app, _state) = app_with(&[]);
            let (status, _) = call(&app, post("/v1/mcp/proposals", serde_json::json!({}))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }

        /// Deleting a server stops it and takes its tool choices with it.
        #[tokio::test]
        async fn deleting_a_server_stops_it() {
            let (app, state) = app_with(&[]);
            let id = ready_server(&app).await;

            call(&app, get(&format!("/v1/mcp/servers/{id}/tools"))).await;
            assert!(state.mcp().is_running(&id).await);

            let (status, _) = call(
                &app,
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/v1/mcp/servers/{id}"))
                    .body(Body::empty())
                    .expect("builds"),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(!state.mcp().is_running(&id).await);

            let (_, servers) = call(&app, get("/v1/mcp/servers")).await;
            assert!(servers.as_array().expect("a list").is_empty());
        }
    }
}
