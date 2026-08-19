# Rule-based model routing — design

**Date:** 2026-08-18
**Status:** draft, awaiting review
**Scope:** Spec 2 of the program map. Per-request backend selection inside `ai-router`.

---

## Why this exists

`Router` today wraps exactly one backend chosen by one `RouterConfig`. Every model call in the
product goes to that backend: a two-word meeting title and a ninety-minute transcript summary
hit the same endpoint at the same price.

That forces a bad choice on the user. Pick Ollama and summaries are as good as the local model
is. Pick Anthropic and every trivial call — a title, a speaker label, a clarifying question
that may never be shown — costs money and leaves the machine. Neither is what anyone wants,
and the product currently has no way to express the obvious answer: cheap and local for the
small things, good and remote for the hard ones.

The seam to fix it already exists. `Router` implements `AiBackend` and callers hold
`Arc<dyn AiBackend>`, so a router that dispatches per request is a drop-in replacement that no
call site has to know about.

## Goals

- Route each model call to a backend chosen by rules, not by one global setting.
- Keep every existing caller unchanged. No signature churn above `ai-router`, and no change to
  `AppState`'s `Arc<Router>`.
- Make the common case require no configuration and no LLM call to decide.
- Preserve the redaction guarantee per destination, not per app.
- Fail toward the configured default rather than toward an error.

## Non-goals

- **Routing embeddings.** `embed.rs` is deliberately local-only for a stated reason — indexing
  a workspace means sending everything, which the user did not consent to by picking a cloud
  summarizer. Routing must not become the loophole that undoes that.
- **Cost accounting or budgets.** Knowing what a call cost is a separate feature.
- **Automatic model discovery or benchmarking.** Rules are declared, not learned.
- **Streaming or partial-response routing.** Whole call, one backend.
- **Per-workspace or per-user rules.** Single-user, one rule set, until multi-user exists.

---

## Decisions

### R1 — The trait method is the primary routing signal

`AiBackend` already segments work: `summarize`, `extract_decisions`, `extract_action_items`,
`chat`. That distinction is free, exact, available before any tokens are examined, and it maps
almost perfectly onto the cheap/expensive split the feature exists to exploit.

So a `TaskKind` enum derived from the method being called is the first-class rule input:
`Summarize`, `ExtractDecisions`, `ExtractActionItems`, `Chat`, and `Clarify` — the last because
`clarify::suggest_questions` goes through `chat` but has completely different economics: it
fires repeatedly during a live meeting and most of its output is discarded.

**Rejected — routing on message content by default.** AnythingLLM's Model Router leads with
keyword and LLM-classified rules because it has one entry point (a chat box) and nothing else
to go on. Notewise has typed call sites. Using them is both cheaper and more accurate.

### R2 — Rules are evaluated locally; no LLM decides the route by default

Every rule predicate is computable without a model call: task kind, estimated input tokens,
whether the transcript exceeds a length, time of day, and whether the local backend currently
probes healthy.

An LLM-classified rule is deliberately omitted from this spec. It would mean a model call to
decide which model to call, doubling latency on the fast path to route work that `TaskKind`
already separates. If a real need appears it can be added as another predicate later.

### R3 — Route targets are full `RouterConfig`s, not model names

A route must carry its own api key, endpoint, model, **and redaction policy**. `RouterConfig`
already holds exactly that set, and `Router::effective_redaction` already forces `Off` for
local backends.

Carrying only a model name would silently apply the default backend's redaction policy to a
different destination — a masking rule chosen for one provider applied to another, which is the
kind of privacy bug that produces no error and no log line.

Each configured route therefore builds a real `Router`, constructed once and reused.

### R4 — Routing goes *inside* `Router`; there is no wrapper type

**Corrected during planning.** An earlier draft of this spec proposed a `PolicyRouter` wrapping
several `Router`s, on the claim that `api-server` holds `Arc<dyn AiBackend>` so a wrapper would be
a drop-in. It does not: `AppState.ai` is `RwLock<Arc<Router>>`, the concrete type. A wrapper would
have forced either a field-type change that breaks the `Router`-only methods, or a parallel type
that every call site has to learn about.

So `Router` gains the policy itself:

```rust
pub struct Router {
    backend: Box<dyn AiBackend>,   // the default route
    redaction: RedactionPolicy,
    kind: BackendKind,
    routes: Vec<Route>,            // ordered; first match wins; empty = today's behaviour
}

pub struct Route {
    pub name: String,
    pub when: Vec<Predicate>,      // all must hold
    pub target: Box<dyn AiBackend>,
    pub kind: BackendKind,
    pub redaction: RedactionPolicy,
}
```

An empty `routes` is exactly the current behaviour, so every existing construction path and all
174 existing tests keep passing unchanged. No call site in `api-server` is touched.

### R4a — The four whole-router questions get honest answers under a policy

`Router` answers four things that have no single truth once several backends are reachable. Each
gets the conservative answer, because each drives either a privacy claim or a UI label:

| Method | Under a policy | Why |
|---|---|---|
| `is_local()` | true only if the default **and every route** is local | It drives a claim the product treats as verifiable. A policy where anything may leave the machine is not local. |
| `effective_redaction()` | the **strictest** policy across default and routes | It is asked without a call context, so it must not under-report masking for the destination that needs it most. |
| `kind()` | the default route's kind | Used for a settings label. Ambiguous by nature; the default is the honest single answer and the explain endpoint gives the full picture. |
| `model_id()` | see R5 | |

### R5 — `model_id()` reports what actually ran, per call

`AiBackend::model_id` returns `&str` and is called to record which model produced an output, so
it can be audited or regenerated. With routing, the honest answer differs per request, and a
`&str` return cannot express that.

`SummaryOutput` already carries a `model` field, which is the value that actually gets stored,
so per-call attribution is preserved through the existing path. `PolicyRouter::model_id()`
returns a stable description of the *policy* — `"policy:3-routes"` — because a single string
cannot honestly summarise several backends, and inventing one that names only the default would
be a lie recorded next to every artifact.

Nothing that persists a model name reads it from `model_id()` where a per-call value is
available. This is the one place the abstraction leaks, and it leaks toward truth.

### R6 — `is_local()` is true only if every reachable route is local

`is_local` drives a UI claim the product treats as verifiable rather than trusted. A policy
where any route may leave the machine is not local, even if most calls stay.

Reporting otherwise would put "local only" in front of a user whose summaries are going to
Anthropic, which is the single worst thing this crate could get wrong.

### R7 — A failing route falls back to the default, once

If a route's backend fails with a retryable error, `PolicyRouter` retries the call on `default`
and records that it did. If `default` fails, the error surfaces.

One fallback hop, not a cascade: a chain of failing backends turns one slow call into four, and
the user is better served by an error than by a ninety-second wait.

A route whose backend is *unreachable* (local daemon stopped) is skipped at selection time via
a cached probe result rather than discovered by failing — probing on every call would add a
round trip to every request.

### R8 — Rules live in settings as one JSON document

`SettingsRepository` is a key-value store of strings. The rule set is stored under a single key
as serialized JSON rather than decomposed into rows.

Rules are read as a set, written as a set, and never queried individually — ordering *is* the
semantics, and ordering in a key-value store means an index column and a rewrite on every
reorder. A table would buy nothing and cost a migration.

Api keys inside route configs are the exception: those go to `CredentialStore`, keyed per route
name, exactly as the single-backend path already does. The JSON holds a reference, never a key.

---

## Architecture

```
api-server holds Arc<Router>        ── unchanged
        │
        ▼
  Router  (existing type, now policy-aware)
        ├─ select(TaskKind, &input)  ── local predicates only
        │     └─ first matching Route, else the default backend
        ▼
  Box<dyn AiBackend>  (Ollama | Anthropic | …)
```

| Location | Contents | New? |
|---|---|---|
| `ai-router/src/policy.rs` | `Route`, `Predicate`, `TaskKind`, selection, rule (de)serialization | new |
| `ai-router/src/lib.rs` | Re-exports | edit |
| `api-server/src/state.rs` | Load the stored rule set when building the `Router` | edit |
| `api-server/src/routes.rs` | CRUD for the rule set, plus a dry-run explain endpoint | edit |

No new crate. No schema migration.

### Predicates

```rust
pub enum Predicate {
    Task(Vec<TaskKind>),
    InputTokensOver(usize),
    InputTokensUnder(usize),
    TranscriptMinutesOver(u32),
    LocalBackendHealthy(bool),
    HourOfDayBetween(u8, u8),
    TitleOrPromptContains(Vec<String>),
}
```

Token counts are estimated by character count over a divisor, not tokenized. An exact count
needs a tokenizer per model family, and the predicate exists to separate "a title" from "a
transcript" — a decision no plausible tokenizer disagreement changes.

### Default policy shipped out of the box

Two routes, expressing the split the feature exists for:

1. `Summarize` **or** `InputTokensOver(8000)` → the user's configured quality backend.
2. everything else → local.

If the user has configured no cloud backend, both collapse to local and the policy is a no-op.
A fresh install therefore behaves exactly as today, and no meeting content leaves the machine
until the user configures a backend that does.

---

## Data flow

```
AiBackend::summarize(input)
  └─> PolicyRouter::select(TaskKind::Summarize, input)
        ├─> for each route in order: all predicates hold?
        │     └─> skip routes whose backend last probed unhealthy
        ├─> matched -> that Router
        └─> none    -> default Router
  └─> forward the call
        ├─> Ok  -> return; SummaryOutput.model already names the real model
        └─> retryable Err and route != default -> retry once on default
```

Probe results are cached with a short TTL and refreshed out of band, so selection never blocks
on a network call.

## Error handling

`AiError` needs no new variants. Routing introduces no failure mode of its own: a route with an
unbuildable config fails at construction, which is startup, where `from_config` already reports
missing keys and endpoints.

An invalid stored rule set is a startup condition, not a runtime one, and is handled by falling
back to the default single-backend behaviour with a warning rather than refusing to start. An
app that will not launch because a routing rule is malformed has turned an optimisation into an
outage — the same reasoning `indexing.rs` applies to a missing embedder.

## Testing

All of it runs in CI with no model, using `MockBackend` behind several `Router`s:

- Selection: first-match-wins ordering, all-predicates-must-hold conjunction, fallthrough to
  default, empty rule set behaves as the bare router (the existing suite proves this).
- `effective_redaction()` returns the strictest across routes; `is_local()` false if any route is
  remote.
- Every predicate, at and either side of its boundary.
- `is_local()` false when any route is remote, true when all are local.
- Unhealthy route skipped at selection without a call attempt.
- Retryable failure on a route retries exactly once on default; non-retryable does not; a
  failure on default surfaces.
- Redaction: a call routed to a remote target applies *that* target's policy, and a local
  target applies `Off`. This is the privacy-critical test.
- Malformed stored JSON degrades to single-backend rather than failing startup.
- Round-trip serialization of the rule set.

Nothing here needs an API key or a GPU, so nothing here is `#[ignore]`d.

## What this delivers

1. Per-request selection inside the existing `Router`, with an empty rule set preserving
   today's behaviour exactly.
2. Seven local predicates and a `TaskKind` derived from the trait method.
3. A shipped default policy that routes summaries to quality and everything else to local, and
   that is a no-op until a cloud backend is configured.
4. Settings CRUD plus an explain endpoint that answers "where would this call go, and why".
5. Fallback-once semantics and probe-based route skipping.

## Risks and open questions

- **`model_id()` is a genuine abstraction leak.** R5 handles it honestly but does not remove
  it. Any future caller that persists `model_id()` instead of the per-output `model` field will
  record something misleading, and nothing in the type system prevents that.
- **Estimated token counts** will misclassify occasionally near a boundary. The consequence is a
  call going to the wrong tier, not a wrong answer.
- **The explain endpoint is load-bearing for trust.** Without it, "why did that cost money" is
  unanswerable, and users will disable routing rather than debug it.
- **Route-scoped credentials** multiply the number of keys in the keychain. The UI must make it
  obvious which route a key belongs to or key management becomes the feature's worst edge.
