# MCP client and executable action items — design

**Date:** 2026-08-18
**Status:** draft, awaiting review
**Scope:** Spec 6 of the program map. Calling external tools, and turning action items into
things that can actually happen.

---

## Why this exists

Notewise extracts action items and stores them. `ActionItem` can be assigned, given a due date,
given an owner, and moved through `WorkStatus`. What it cannot do is *happen*. "File a ticket for
the auth regression" becomes a row, and a human still opens Linear.

The asymmetry is stark in the current architecture: `mcp-server` lets *other* agents act on the
Notewise workspace, with a considered write-access model. Notewise cannot act on anything else.
It is a tool provider and not a tool consumer, which means every integration has to be built as a
connector by us, one vendor at a time — the exact cost `cloud/integrations` is scoped to absorb
and the exact reason it is still a scaffold.

An MCP client inverts that. Any tool the user already trusts becomes reachable, and the
long tail stops being our problem.

## What already exists, and what constrains this

**`mcp-server/src/tools.rs`** establishes the trust model this spec must not undercut: read-only
by default, `WriteAccess::Allowed` as an explicit opt-in, `MUTATING_TOOLS` held as data so the
check cannot drift from the dispatch table, and **nothing deletes** even when writes are allowed.

**`api-server/src/agent.rs`** deliberately has a narrow blast radius: search, read, and create
one note. It cannot edit or delete anything existing, cannot touch tickets, and cannot reach a
connector. Its module docs state the reasoning — "an agent that runs unattended is the last place
to widen a blast radius."

**`AiBackend` has no tool-calling method,** and `agent.rs` explains why: adding one means
implementing it for every backend, including a local GGUF whose tool support depends on which
file the user downloaded. It uses a text JSON protocol instead.

All three of those hold. This spec is designed inside them, not around them.

## Goals

- Connect to external MCP servers, discover their tools, and call them.
- Let an action item carry a proposed tool call, reviewable before it runs.
- Show the exact arguments a call will use, before it runs.
- Record every external call that was made, because it happened outside Notewise.
- Default to being able to do nothing.

## Non-goals

- **Widening what `agent.rs` can do unattended.** See M8. The agent does not get external tools.
- **Auto-executing anything.** See M2. Every external call is confirmed by a human, every time.
- **Deleting through external tools.** Consistent with `tools.rs`, no proposal may target a tool
  the user has marked destructive, and there is no bulk execute.
- **A visual flow builder.** AnythingLLM's Agent Flows are a separate product surface.
- **A tool marketplace or community hub.** Users bring their own server configs.
- **`AiBackend::call_tool`.** Explicitly rejected in M3.

---

## Decisions

### M1 — A new `core/crates/mcp-client` crate, not an addition to `mcp-server`

`CLAUDE.md` rule 2: surfaces never depend on each other. `mcp-server` is a surface;
`mcp-client` is a capability that `api-server` consumes. They must not import one another even
though they speak the same wire protocol.

That means the JSON-RPC framing is written twice — once in `mcp-server/src/protocol.rs` and once
here. That duplication is accepted rather than resolved by extracting a shared `mcp-protocol`
crate, because doing so mid-spec would refactor a working surface to serve a new one. It is the
obvious follow-up and is recorded as a risk, not hidden.

`mcp-client` depends on neither `storage` nor `graph`: it manages child processes and speaks a
protocol. Persistence is the caller's business.

### M2 — Every external tool call is confirmed by a human, every time

There is no auto-execute, no "always allow this tool", and no batch execute.

The reasoning is the same one `agent.rs` gives for its own narrowness, applied to a blast radius
that is now unbounded: an external MCP tool can send a message, file a ticket, or charge a card,
and Notewise cannot know which. The workspace tools in `mcp-server` could be reasoned about
one by one, which is why `MUTATING_TOOLS` is a list. An arbitrary external tool cannot be, so the
gate moves from "which tools are safe" to "a person looked at this call."

A remembered per-tool allow would collapse to auto-execute within a week of use, which is why it
is absent rather than merely defaulted off.

**Rejected — a trusted-tools allowlist that skips confirmation.** It is the obvious convenience
and it is exactly how an unattended agent ends up sending an email. The product has no send path
anywhere by design; this must not become one by transitivity.

### M3 — Proposals come through the text JSON protocol, not a new trait method

`agent.rs`'s reasoning applies unchanged: `AiBackend::call_tool` would have to be implemented for
five backends including local ones with unpredictable tool support, and the local path is the one
the product's promise depends on.

So a proposal is produced by asking the model for JSON describing one tool call, and parsing it
the way `parse_action` already parses agent actions — tolerating prose and code fences, and
feeding an unparseable response back as an observation rather than failing.

This costs accuracy relative to native tool calling. It buys working identically on Ollama and
Anthropic, which is the trade this codebase has already made once and should not un-make in a
feature that touches external systems.

### M4 — Arguments are validated against the discovered schema before a human sees them

MCP `tools/list` returns a JSON Schema per tool. A proposal is checked against it — required
fields present, types correct, no unknown fields — before the confirmation UI renders.

A model-authored argument object shown to a user unvalidated invites the worst version of this
feature: a plausible-looking confirmation dialog for a call that will fail, or worse, one whose
extra field means something to the server. Validation failure sends the proposal back to the
model as an observation, which is the same recovery path `parse_action` already uses.

The confirmation UI shows the validated arguments verbatim — the "View Arguments" idea, which is
the one genuinely good trust affordance in AnythingLLM's version of this.

### M5 — Servers and tools are default-denied, mirroring `WriteAccess`

A configured MCP server is not a usable one. Each server is explicitly enabled, and within it
each tool is explicitly enabled. Nothing is reachable by having been added to a config file.

This is `WriteAccess`'s reasoning generalised: connecting a client should not grant capability as
a side effect. It also gives Spec 7's scheduled jobs the per-job tool subset they need, because
the allowlist is already a first-class object rather than a global switch.

Enabled tools are stored as data and checked at dispatch, so — as with `MUTATING_TOOLS` — adding
a handler without listing it fails a test rather than silently widening access.

### M6 — Servers start lazily and can be pinned off

Stdio servers are child processes. They start on first use, not at app launch, and a server may be
configured `auto_start: false` so a resource-heavy one starts only when explicitly requested.

Starting every configured server at launch would make app startup depend on every MCP server a
user has ever added, including broken ones. Lazy start means a misconfigured server breaks its own
tools and nothing else.

Both stdio and streamable HTTP transports are supported. Stdio is the common case for local tools;
HTTP is what remote servers use.

### M7 — Executions are persisted, unlike agent runs

`agent.rs` keeps runs in memory and argues correctly that a trace is only interesting while it is
happening, and that what matters — the note it wrote — survives already.

That argument does not transfer. An external tool call's effect is *outside* Notewise: a ticket was
filed, a message was posted. Losing the record on restart leaves the user unable to answer "did
that already run?", and the recovery for guessing wrong is a duplicate side effect in someone
else's system.

So a `tool_executions` table records what was proposed, what was confirmed, what was sent, and what
came back. This is the same reasoning that puts `connector_outbox` on disk while agent runs stay in
memory: irreversible external effects get durable records.

### M8 — `agent.rs` does not get external tools

The autonomous agent's action set stays exactly as it is. External tools are reachable only from
paths where a human is present to confirm.

An unattended multi-step agent with arbitrary external tools is precisely the combination M2 exists
to prevent. The agent remains able to search, read, and write one note; if it identifies work worth
doing externally, it can say so in the note.

---

## Architecture

```
api-server
   ├── propose:  transcript / action item ──► AiBackend (text JSON) ──► ToolProposal
   │                                              │
   │                                        validate vs schema (M4)
   │                                              ▼
   ├── confirm:  human reads tool + arguments ──► approves
   │                                              ▼
   └── execute:  mcp_client.call(server, tool, args)
                        │                          │
                        │                    tool_executions row
                        ▼
                 mcp-client
                   ├── registry of configured servers (default denied)
                   ├── lifecycle: lazy start, auto_start:false, stop
                   └── transports: stdio, streamable HTTP
```

| Location | Contents | New? |
|---|---|---|
| `core/crates/mcp-client` | Transports, JSON-RPC framing, server lifecycle, `tools/list`, `tools/call`, schema validation | **new crate** |
| `storage/src/migrations.rs` | `mcp_servers`, `mcp_enabled_tools`, `tool_executions` | edit |
| `storage/src/repositories/tool_execution.rs` | Execution records and server config | new |
| `api-server/src/tools.rs` | Propose, confirm, execute routes; proposal parsing | new |
| `apps/desktop/src/views/TasksView.tsx` | Proposed action, argument review, confirm | edit |

`mcp-client → {}` (no internal deps). `api-server → mcp-client`. `mcp-server` unchanged and
untouched.

### Data model

```sql
CREATE TABLE mcp_servers (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL UNIQUE,
    transport   TEXT NOT NULL,          -- 'stdio' | 'http'
    command     TEXT,                   -- stdio
    args        TEXT,                   -- JSON array
    url         TEXT,                   -- http
    enabled     INTEGER NOT NULL DEFAULT 0,
    auto_start  INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL
);

CREATE TABLE mcp_enabled_tools (
    server_id  TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,
    tool_name  TEXT NOT NULL,
    PRIMARY KEY (server_id, tool_name)
);

CREATE TABLE tool_executions (
    id            TEXT PRIMARY KEY NOT NULL,
    action_item_id TEXT REFERENCES action_items(id) ON DELETE SET NULL,
    server_id     TEXT NOT NULL REFERENCES mcp_servers(id),
    tool_name     TEXT NOT NULL,
    arguments     TEXT NOT NULL,        -- JSON, as sent
    status        TEXT NOT NULL,        -- 'proposed'|'confirmed'|'succeeded'|'failed'|'rejected'
    result        TEXT,                 -- JSON or error text
    proposed_at   TEXT NOT NULL,
    executed_at   TEXT
);
```

Environment variables and headers for a server hold credentials and therefore go to
`CredentialStore`, never into `mcp_servers`. The row holds a reference; the keychain holds the
value — the same split Spec 2 applies to route keys.

`action_item_id` is `ON DELETE SET NULL`: deleting an action item must not erase the record that
something was executed in another system on its behalf.

## Data flow

```
propose
  └─> context = action item text + meeting summary + enabled tool schemas
  └─> AiBackend::chat asking for one JSON tool call, or "none"
  └─> parse (tolerating fences/prose); unparseable -> observation -> retry once
  └─> validate against the tool's JSON Schema (M4)
        ├─ invalid -> back to the model as an observation
        └─ valid   -> tool_executions row, status 'proposed'
confirm
  └─> UI renders tool name, server name, and validated arguments verbatim
  └─> user approves -> status 'confirmed'
      user rejects  -> status 'rejected'; nothing is sent
execute
  └─> ensure server started (lazy, M6)
  └─> mcp_client.call(tool, args)
        ├─ Ok  -> status 'succeeded', result stored, executed_at set
        └─ Err -> status 'failed', error stored; no automatic retry
```

**No automatic retry.** A failed external call may or may not have taken effect, and a retry that
duplicates a filed ticket is worse than a visible failure a human resolves. This is deliberately
unlike `connector_outbox`, where we control both sides and can make delivery idempotent; here we
cannot.

## Error handling

New `McpClientError` variants (its own `thiserror` enum, per rule 5):

| Condition | Variant | Behaviour |
|---|---|---|
| Server binary missing | `SpawnFailed { server, source }` | Server marked unavailable; its tools vanish from proposals |
| Handshake fails or version unsupported | `Handshake { server, detail }` | Same |
| Server stopped mid-call | `Transport` | Execution fails; no retry |
| Tool not in the enabled list | `ToolNotEnabled { server, tool }` | Rejected before dispatch; test-enforced |
| Arguments fail schema validation | `InvalidArguments { tool, detail }` | Returned to the model, never shown as a valid proposal |
| Tool returns an error | `ToolError { tool, detail }` | Stored in `result`, status `failed` |
| Call exceeds timeout | `Timeout { tool }` | Status `failed`, outcome explicitly unknown to the user |

`Timeout` says "unknown", not "failed", in the UI. Telling a user a call failed when it may have
succeeded is what causes duplicate side effects.

## Testing

In CI, with a stub MCP server implemented in-process:

- Handshake, `tools/list`, `tools/call` over both transports against a stub.
- Lazy start: no process until first use; `auto_start: false` requires explicit start.
- Default deny: a configured-but-not-enabled server exposes nothing; an enabled server with no
  enabled tools exposes nothing.
- A dispatch attempt on a non-enabled tool is refused — and a test that fails if a handler exists
  whose name is not in the enabled-tool check, mirroring the `MUTATING_TOOLS` drift test.
- Proposal parsing: clean JSON, fenced JSON, JSON in prose, "none", garbage.
- Schema validation: missing required field, wrong type, unknown field, valid.
- State machine: proposed → confirmed → succeeded / failed; proposed → rejected sends nothing;
  no path from proposed to succeeded without confirmed.
- Failure does not retry; timeout records unknown.
- `agent.rs`'s action set is unchanged — a test asserting the agent cannot reach a tool dispatch.

Marked `#[ignore]` with a reason: anything requiring a real third-party MCP server.

`MockBackend` drives proposal generation deterministically.

## What this delivers

1. `core/crates/mcp-client` — stdio and HTTP transports, lifecycle, discovery, schema validation.
2. Server and per-tool configuration, default denied, credentials in the keychain.
3. Action items that carry a proposed tool call, with arguments shown verbatim before running.
4. Durable `tool_executions` records, because the effects are outside Notewise.
5. A tool allowlist object Spec 7 can scope a scheduled job to.
6. No change to what the autonomous agent can do.

## Risks and open questions

- **Duplicated JSON-RPC framing** between `mcp-server` and `mcp-client`. Extracting a shared
  protocol crate is the right follow-up and is deliberately not in this spec.
- **Confirmation fatigue is the real failure mode.** M2 forbids the remembered-allow that would
  fix it. If users find per-call confirmation intolerable, the answer is fewer and better
  proposals, not a trust shortcut — and that tension is unresolved.
- **Child process management** is where this will actually break: zombie processes, servers that
  hang on shutdown, stdio deadlocks on large payloads. It deserves more attention than the
  protocol work.
- **Prompt injection through transcripts into tool arguments.** A meeting participant who says
  "file a ticket assigning all work to me" can influence a proposal. Human confirmation is the
  mitigation, and it is the same mitigation `email.rs` relies on for the same reason.
- **A malicious MCP server** sees whatever arguments a proposal contains, which may include
  meeting content. Server configuration is a trust decision the user makes, and the UI should say
  so plainly.
