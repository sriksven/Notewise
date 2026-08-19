# Memory and personalization — design

**Date:** 2026-08-18
**Status:** draft, awaiting review
**Scope:** Spec 8 of the program map. Durable facts that make answers better, extracted from
meetings.

---

## Why this exists

Every model call in Notewise starts from nothing. The user's role, the vocabulary their team uses,
which project a recurring meeting belongs to, how they like summaries structured — none of it
survives from one call to the next. A user who told the app last month that "the platform team"
means four specific people has to tell it again, implicitly, every time.

Meetings are an unusually good source for this. AnythingLLM extracts memories from chat logs;
a meeting transcript contains an hour of a person's actual working context, stated naturally,
weekly. The raw material here is better than the raw material the idea was built on.

That is also exactly why this spec is mostly about restraint.

## Goals

- A small set of durable facts about the user, injected into prompts that benefit.
- Automatic extraction that is off until the user turns it on.
- Manual add, edit, delete, and re-scope, with the automatic path never required.
- Hard caps, so the feature cannot degrade every prompt by growing without bound.
- An extraction boundary that keeps third parties out of it.

## Non-goals

- **Facts about other people.** See P2. This is the decision that shapes the whole spec.
- **Voiceprints or biometric identity.** Already gated elsewhere on an explicit consent decision;
  nothing here touches it.
- **Replacing retrieval.** `retrieval.rs` finds relevant material per question. Memory is a handful
  of standing facts, not a search index, and it must not become a second one.
- **Cross-device memory sync.** Local only until sync exists.
- **Per-person memory profiles.** One user, one set.

---

## Decisions

### P1 — Off by default, and the manual path works without ever turning it on

Automatic extraction is disabled on a fresh install. Manual memories work regardless.

This matches how every privacy-sensitive capability in this codebase already ships: voiceprint
storage is off by default (`5ab364c`), acoustic separation is off by default (`500df1a`), and the
`people` table's voice columns are documented as deliberately unwritten pending consent. A feature
that reads every transcript to build a durable profile belongs in that category, and defaulting it
on would be inconsistent with a product whose central claim is that the user decides what happens
to their recordings.

### P2 — Memories are about the user, never about third parties

An extracted memory may record the user's own role, preferences, projects, vocabulary, and working
patterns. It may not record claims about other people.

This is the load-bearing decision. A transcript contains sentences like "Dana's performance review
is not going well" or "Sam is out for surgery in March." Those are facts, they are extractable, and
storing them as durable context injected into future prompts would be indefensible: the person they
describe never consented, cannot see them, cannot correct them, and would be described that way in
every future summary the model writes.

Attendee *identity* — that a person exists, their name and email — is already handled by the
`people` table via Spec 1's calendar import, which is a directory rather than a dossier. The
distinction this decision draws is between knowing who someone is and holding opinions about them.

The extraction prompt forbids third-party claims explicitly, and the reflector rejects candidates
naming a person other than the user. That is mitigation, not a guarantee — which is why P3 also
makes every memory visible and deletable.

**Rejected — extracting team facts because they are useful.** They are useful. "Priya owns billing"
would genuinely improve action-item assignment. It is also one sentence away from "Priya is
struggling with billing," and no prompt reliably separates those. Assignment already has a better
mechanism in `set_action_item_person`.

### P3 — Every memory is visible, editable, and deletable, always

No hidden memories, no derived state the user cannot inspect. The list shows every memory, its
scope, and when it was added.

A durable fact silently influencing every future answer, that the user cannot see, is the failure
mode that makes personalization feel like surveillance. It is also the only practical remedy when
P2's prompt-level guard misses something.

### P4 — Two scopes with hard caps: global and project

Global memories apply everywhere and are capped at 5. Project memories apply to meetings in that
project and are capped at 20 per project. The `Workspace → Project → Meeting` hierarchy already
exists, and `Meeting.project_id` is already the link.

Caps are hard limits, not soft warnings. Memory is injected into the system prompt of calls that
already carry retrieved material and a transcript; an unbounded list crowds out the actual content
and makes every answer slightly worse in a way that is nearly impossible to attribute. Reaching the
cap forces a choice, and forcing a choice is the point.

Injection sends all global memories plus the most relevant project memories, ranked by the same
cosine similarity `embed.rs` already provides, with a fixed ceiling.

### P5 — Observer and reflector are two passes, and the reflector is pure

**Observer** reads recent, unprocessed meetings and proposes at most three candidate facts per run.
**Reflector** compares candidates against existing memories, decides scope, rejects duplicates and
third-party claims, and consolidates rather than accumulating.

Splitting them means the reflector's rules — dedup, scope choice, cap enforcement, third-party
rejection — are a pure function over candidates and existing memories, testable exhaustively with no
model. That is where the decisions that can be wrong live, and it follows the same split
`clarify.rs` uses: the model call in one place, the judgement about whether to act in another.

### P6 — Extraction runs as a scheduled job, gated on idleness

Extraction is a Spec 7 job on a default schedule of every few hours, and it does nothing unless
there are enough unprocessed meetings and the app has been idle for a while.

Building a second scheduler for this would duplicate Spec 7 exactly. The idle gate exists because
extraction competes with the user for a local model — running it during a live meeting would slow
transcription to save a fact nobody asked for.

If Spec 7 is not implemented, extraction runs on demand from settings, and the automatic path
simply does not exist yet. This spec does not require Spec 7 to be useful.

### P7 — Memory never leaves the machine except with the prompt it is attached to

Memories are stored locally and are subject to the same `RedactionPolicy` as anything else in a
prompt going to a remote backend. A memory whose text matches a `redact::Category` — an api key, a
card number, a phone number — is rejected at write time rather than redacted at send time.

Storing a secret as a durable fact and then relying on redaction to mask it on every future call is
one missed code path away from leaking it. Refusing to store it has no such failure mode.

---

## Architecture

```
scheduled job (Spec 7) or manual trigger
  └─> Observer: recent unprocessed meetings ──AiBackend──> ≤3 candidates
        └─> Reflector (pure): dedup, scope, third-party reject, cap enforce
              └─> MemoryRepository: insert / update / consolidate
                     └─> mark meetings processed

prompt assembly (ask.rs, summarize, agent)
  └─> all global memories + top-k project memories by cosine
        └─> "Things I know about you" block in the system prompt
```

| Location | Contents | New? |
|---|---|---|
| `ai-router/src/memory.rs` | Observer prompt, candidate parsing, reflector (pure) | new |
| `storage/src/migrations.rs` | `memories`, `memory_extraction_state` | edit |
| `storage/src/repositories/memory.rs` | `MemoryRepository` | new |
| `api-server/src/memory.rs` | CRUD, manual extract trigger, prompt assembly helper | new |
| `api-server/src/ask.rs`, `agent.rs` | Include the memory block | edit |
| `apps/desktop/src/views/` | Memory list with scope, edit, delete, cap indicator | new view |

No new crate.

### Data model

```sql
CREATE TABLE memories (
    id          TEXT PRIMARY KEY NOT NULL,
    scope       TEXT NOT NULL,          -- 'global' | 'project'
    project_id  TEXT REFERENCES projects(id) ON DELETE CASCADE,
    text        TEXT NOT NULL,
    origin      TEXT NOT NULL,          -- 'manual' | 'extracted'
    source_meeting_id TEXT REFERENCES meetings(id) ON DELETE SET NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    CHECK ((scope = 'project') = (project_id IS NOT NULL))
);

CREATE TABLE memory_extraction_state (
    meeting_id   TEXT PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE,
    processed_at TEXT NOT NULL
);
```

The `CHECK` makes the scope/project pairing a schema invariant rather than a convention — a global
memory with a project id, or a project memory without one, cannot exist.

`source_meeting_id` is `ON DELETE SET NULL`: a memory outlives the meeting that produced it, but the
provenance is worth keeping while the meeting exists. It is what lets the UI answer "why does it
think that."

`memory_extraction_state` is a separate table rather than a column on `meetings` because it is
derived state about a background pass, and `meetings` should not grow a column every time something
processes it — the same reasoning the v8 migration applies to embeddings being derived data.

## Data flow

```
extract
  ├─> meetings with no memory_extraction_state row, ended, above a minimum length
  ├─> gate: enough unprocessed meetings? app idle long enough?  (P6)
  ├─> Observer: AiBackend::chat -> ≤3 candidate facts as JSON
  ├─> parse tolerantly (fences/prose), like parse_action
  └─> Reflector (pure) over (candidates, existing memories):
        ├─ reject: names a third party            (P2)
        ├─ reject: matches a redact::Category     (P7)
        ├─ reject: duplicate of an existing memory
        ├─ consolidate: supersedes an existing one -> update in place
        ├─ decide scope: project if the fact is project-bound, else global
        └─ enforce caps: at cap, keep the existing unless the candidate supersedes it
  └─> write accepted memories; mark every read meeting processed

inject
  └─> global memories (all, ≤5) + project memories ranked by cosine, capped
  └─> rendered as a labelled block in the system prompt
```

Meetings are marked processed whether or not anything was extracted, so a meeting containing
nothing worth remembering is not re-read every run.

## Error handling

No new error variants. Every failure path leaves the feature absent rather than broken:

| Condition | Handling |
|---|---|
| Model unavailable during extraction | Run does nothing; meetings stay unprocessed; retried next run |
| Observer output unparseable | Discarded after one retry; meetings still marked processed |
| Candidate rejected by the reflector | Recorded in the run trace, not surfaced as an error |
| Cap reached | Candidate dropped unless it supersedes; visible in the trace |
| Embedder unavailable for ranking | Fall back to most-recent-first ordering |
| `CHECK` violated | `StorageError`; indicates a code bug, not user input |

Falling back to recency when the embedder is down mirrors `indexing.rs`: every path degrades to
something that works less well, never to an error.

## Testing

The reflector is pure, so nearly everything that can be wrong is testable with no model:

- Third-party rejection: candidates naming someone other than the user are rejected; a candidate
  about the user that merely mentions a colleague's name is judged on its claim, not on the presence
  of a name.
- `redact::Category` rejection for each category.
- Deduplication on near-identical candidates; consolidation replacing a superseded memory in place
  rather than adding a second.
- Cap enforcement at 5 global and 20 per project, including the supersede-at-cap path.
- Scope decision: project-bound fact scoped to the project, general fact scoped global.
- The `CHECK` constraint rejects both invalid scope/project combinations.
- Injection: all global plus capped project memories; ordering by cosine; recency fallback with no
  embedder.
- Meetings marked processed even when nothing was extracted.
- Idle and volume gates: no run when either fails.
- Extraction off by default on a fresh database.

`MockBackend` drives the observer deterministically. Nothing here is `#[ignore]`d.

## What this delivers

1. `memories` with global and project scope, hard caps, and a schema-level scope invariant.
2. Manual CRUD that works whether or not extraction is enabled.
3. Observer plus a pure, exhaustively tested reflector.
4. Third-party and secret rejection as tested rules, not prompt hope.
5. Prompt injection with cosine ranking and a recency fallback.
6. Extraction as a Spec 7 job, idle-gated, off by default.

## Risks and open questions

- **P2 is enforced by a prompt plus a heuristic reflector check, and both can miss.** A memory that
  slips through is visible and deletable by P3, which is the backstop, not a fix. This is the
  single most important risk in this spec.
- **"Is this about the user?" is genuinely ambiguous** for facts like "I run the platform standup" —
  which is about the user and implies things about others. The reflector will get some of these
  wrong in both directions.
- **Caps will frustrate.** Five global memories is very few, and the alternative is degrading every
  prompt. Whether these numbers are right is unmeasured.
- **Memory changes answers without being visible in them.** A user who cannot reproduce an answer may
  not think to check the memory list. Surfacing which memories were injected into a given answer is
  unresolved and probably necessary.
- **Extraction cost** is a model call over multiple transcripts on a schedule; Spec 2's routing should
  keep it local, but nothing enforces that a user has not routed everything to a paid backend.
