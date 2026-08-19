# Program map — AnythingLLM parity review

**Date:** 2026-08-18
**Status:** index. Read this before any of the specs it lists.

Derived from a feature review of [AnythingLLM](https://github.com/Mintplex-Labs/anything-llm) —
its meeting assistant, scheduled jobs, Pro tier, agent skills, MCP support, model router, and
memory system — assessed against what Notewise already has.

---

## First: what the review got wrong

An initial gap analysis based on directory listings judged several capabilities missing that are
in fact implemented. Reading `core/crates/api-server` and `core/crates/ai-router` corrected it.
Recorded here because the same mistake is easy to repeat.

**Already built, and not specced:**

| Capability | Where | Note |
|---|---|---|
| Semantic + lexical search with RRF fusion | `api-server/src/retrieval.rs` | Hybrid, reasoned; not a gap |
| Background embedding index that self-refreshes | `api-server/src/indexing.rs`, `3e4344d` | Degrades to lexical by design |
| Grounded Q&A with citations | `api-server/src/ask.rs` | |
| Multi-step autonomous agent | `api-server/src/agent.rs` | Deliberately narrow blast radius |
| Clarifying questions during a live meeting | `ai-router/src/clarify.rs` | Better than AnythingLLM's Agent Surveys — real-time, with cooldown, staleness, dedup |
| Follow-up email drafting with tone variants | `ai-router/src/email.rs` | Drafts only; no send path exists anywhere |
| Email draft state machine | `storage` `EmailDraftRepository` | `Draft → Approved → Sent`, no step-skipping |
| MCP server with a write-access model | `mcp-server/src/tools.rs` | `WriteAccess`, `MUTATING_TOOLS`, nothing deletes |
| Local-only embedder | `ai-router/src/embed.rs` | Deliberately never hosted |
| Speaker naming and merging | `3b20f40` | AnythingLLM identifies but does not rename |
| Redaction before egress | `ai-router/src/redact.rs` | |

Notewise is ahead of AnythingLLM on clarifying questions, speaker naming, redaction, and the
strictness of its write and send boundaries.

## Specs written

Dependency-ordered. Each is an independent design cycle.

| # | Spec | Depends on | Size |
|---|---|---|---|
| 1 | [Calendar and mail source connectors](2026-08-18-calendar-mail-source-connectors-design.md) | — | Large |
| 2 | [Rule-based model routing](2026-08-18-model-routing-rules-design.md) | — | Small-med |
| 3 | [Summary templates, transcript editing, titles](2026-08-18-summary-templates-transcript-editing-design.md) | — | Medium |
| 5 | [Auto-join detection](2026-08-18-auto-join-detection-design.md) | 1 (calendar signal) | Medium |
| 6 | [MCP client and executable action items](2026-08-18-mcp-client-executable-actions-design.md) | — | Large |
| 7 | [Scheduled jobs](2026-08-18-scheduled-jobs-design.md) | 6 (tool allowlist) | Med-large |
| 8 | [Memory and personalization](2026-08-18-memory-personalization-design.md) | 7 (optional) | Medium |
| 9 | [Desktop assistant foundation](2026-08-18-desktop-assistant-foundation-design.md) | — | Very large |
| 10 | [Document import and vault divergence](2026-08-18-document-import-and-vault-divergence-design.md) | 1 (`Importer`) | Small-med |
| 11 | [Audio retention](2026-08-18-audio-retention-design.md) | — | Medium |
| 12 | [Tiering and entitlements](2026-08-18-tiering-and-entitlements-design.md) | — | Small |

Spec 4 was semantic search. It is already implemented; no spec exists.

Spec 11 was split out of Spec 3 mid-design: click-to-seek needs retained audio, which is a privacy
decision and not a UI feature.

## Cross-spec constraints

Three places where specs constrain each other, worth knowing before implementing any of them.

**Spec 6's M2 versus Spec 7.** Every external tool call requires human confirmation. A scheduled
job is unattended. Resolution: jobs may *propose* tool calls, never execute them (Spec 7's S1),
and proposals land in the same review queue as human-initiated ones.

**Spec 1's D10 versus the existing email machinery.** Generation and the draft state machine
already exist. Spec 1 adds only the last hop — a provider-side draft — and must not call
`mark_sent`, because creating a Gmail draft is not sending.

**Spec 12's T3 versus everything.** No local feature may read entitlement state. A structural
test enforces it, because this is the guarantee that decays silently as the codebase grows.

## Recurring decisions

Patterns that recur because the codebase already established them:

- **Quarantine `unsafe`** in one small auditable crate (`macos-permissions`'s stated rationale).
  Specs 1 and 5 avoid native code entirely as a result; Spec 9 accepts one new such crate.
- **Degrade, never fail** (`indexing.rs`). Applied in Specs 2, 5, 8, 10.
- **Off by default for anything privacy-sensitive** (`5ab364c`, `500df1a`, the `voice_print`
  columns). Applied in Specs 8 and 11.
- **Durable records for irreversible or unwitnessed effects**, in-memory for the rest — the
  `connector_outbox`-versus-`agent.rs` split. Applied in Specs 6 and 7.
- **Model call in one place, judgement about whether to act in another** (`clarify.rs`). Applied
  in Specs 5, 6, 8.

## Deliberately not adopted from AnythingLLM

- Vector-DB and embedder breadth. `embed.rs` is local-only on purpose.
- Image generation, embeddable chat widgets, Community Hub / marketplace.
- Agent Flows' visual builder.
- Metered quotas on local compute (Spec 12's T2 rejects it explicitly).
- Bring-your-own-credentials as the *only* option for Microsoft (Spec 1's D8 ships a first-party
  app registration instead).
- Auto-executing tools on a schedule (Spec 7's S1).
- Backfilling missed scheduled runs (Spec 7's S5).

## Phase note

Specs 1, 2, 3, 5, 10, and 11 are Phase 2-adjacent. Specs 6, 7, and 8 reach into Phase 3. Spec 9 is
Phase 3-4 and is a different product from meeting intelligence; its own spec recommends building
only the foundation and dictation, then reassessing.

`docs/roadmap.md`'s gating rule stands: the structure existing "is not license to write Phase 3
code during Phase 0." These specs describe the design, not permission to build it now.
