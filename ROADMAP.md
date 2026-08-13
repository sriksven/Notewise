# Roadmap

Phases are **additive and gated**. The point of the gating is to avoid shipping a
half-working version of four product categories at once.

---

## Phase 0 — Meeting core

**Goal:** audio in, transcript out, stored locally, on one platform.

| Area | Work |
|---|---|
| `core/crates/storage` | SQLite schema, migrations, repositories, encryption at rest |
| `core/crates/graph` | Typed nodes and edges, traversal |
| `core/crates/ai-router` | Trait + mock/Ollama/BYOK backends |
| `core/crates/audio-capture` | Capture interface, mic capture, per-OS backends |
| `core/crates/transcription` | Whisper.cpp integration, model registry |
| `apps/desktop` | macOS + Windows shell |

> `graph` and `ai-router` are pulled into Phase 0 rather than Phase 1. Retrofitting a graph
> onto populated tables is a migration; building on it is a foundation. See
> [ARCHITECTURE.md](ARCHITECTURE.md) for the full reasoning.

## Phase 1 — Local product complete

Diarization. Chat-with-meetings. Decisions and action items as first-class objects.
Block-based notes editor — every meeting auto-generates an **editable** page, not a
read-only export. Native lightweight tickets, no external tracker required. Local REST API
and MCP server. Linux support. **Open-source release of `core/` + `apps/desktop`.**

The bet: notes and native tickets before any external integration, so the free local
product is genuinely useful with nothing connected.

## Phase 2 — Cloud, mobile, external integrations

Opt-in cloud sync. Hosted STT/LLM for users without local compute. iOS and Android.
Calendar integration. Ticket push to Linear/Jira/Asana/GitHub Issues — **one-way first**.
Email draft generation with tone variants; Gmail/Outlook connection for direct send.
Pro tier.

> Email send is the highest trust-risk feature in the entire plan. Draft-and-confirm is the
> default and does not become auto-send-by-default, even as a power-user option, without
> very deliberate UX friction. A single wrong auto-send is a reputational disaster for a user.

## Phase 3 — Notifications, team workspace, wearables

Shared team workspace, comments, external/client access. Full notification system — due
dates, mentions, decisions, catch-up alerts — over push/Slack/digest. Cross-linking graph
fully live across meetings, notes, tickets, and email. watchOS/Wear OS companions. Browser
extension. Team tier.

> Notifications come last of the workspace features because they depend on there already
> being notes, tickets, and teammates worth being notified about. **Default to digest mode.**
> Notification fatigue kills adoption faster than almost anything else here.

## Phase 4 — Enterprise, analytics, bot infrastructure

Self-hosted deployment, SSO, audit logs. Optional calendar-triggered bot recording.
Sales/coaching analytics. Org-wide admin control over notification policy, retention, and
integration permissions.

---

## Folder → phase map

| Phase | Directories that get real code |
|---|---|
| 0 | `core/crates/{storage,graph,ai-router,audio-capture,transcription}`, `apps/desktop` |
| 1 | `core/crates/{diarization,api-server,mcp-server}`, `apps/desktop` (Linux) |
| 2 | `cloud/{sync-service,hosted-inference}`, `core/crates/sync-client`, `apps/mobile/*` |
| 3 | `cloud/{notification-service,integrations}`, `apps/{wearable,browser-extension,web-dashboard}` |
| 4 | `infra/`, `cloud/{bot-service,admin-api,billing}` |

Directories outside the current phase exist but stay empty. The structure is present so the
architecture never needs a rewrite; that is not license to write Phase 3 code during Phase 0.

---

## The main risk

This is four product categories — meeting capture, notes/wiki, project management, email —
not one. Scope creep is the primary threat to shipping any of them well. Each phase above
is deliberately gated. **Resist the urge to parallelize them.**
