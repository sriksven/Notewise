# Ask knows the app: workspace facts, settings and the guide

**Date:** 2026-10-31
**Status:** draft, awaiting review
**Scope:** workspace-wide Ask (`POST /v1/ask`) and the desktop surfaces that show it.

---

## Why this exists

Ask answers only from meetings, notes and tickets. Anything about the app itself fails:

- **"How to change models?"** is refused with "Nothing in this workspace matches that question".
  Retrieval searches transcripts for "change" and "models", finds nothing, and `ask_workspace`
  refuses before the model is called. That refusal is correct for what Ask can see today; the
  problem is what it can see.
- **"How many recordings do I have?"** fails the same way. The number is not written in any
  transcript, so no amount of search finds it.
- **"What's in the trash?"**, **"What are my open tasks?"**, **"Which model am I using?"**,
  **"What version is this?"**, **"What does the Search setting do?"** all fail for one of those
  two reasons.

A user asking "everything you have" means the app too: what is in it, how it is set up, and
how to use it.

## Goals

- Ask answers questions about the workspace's contents in aggregate: counts, recent recordings,
  open tasks, trash.
- Ask answers questions about settings: what each one does, where it is, and what it is set to
  right now.
- Ask answers questions covered by Help and About: how to record, where data lives, shortcuts,
  version, what works and what does not.
- Every such answer is cited like any other, and the citation opens the page it describes.
- Questions about meeting content behave exactly as they do today.

## Non-goals

- **Changing settings from Ask.** Ask explains and links; it never flips a setting. Decided with
  the user: "Explain + link".
- **Making Help render from the guide.** Help keeps its own text for now. See D7.
- **Semantic matching for the guide.** Word matching with authored keywords first; embeddings
  only if measurement on `llama3.1:8b` shows misses. See D3.
- **Note- and meeting-scoped chat.** Asking one note or one meeting stays about that note or
  meeting.
- **Held-back features.** Tickets, People and Automation are hidden in this release
  (`DEFERRED` in `apps/desktop/src/lib/scope.ts`). The guide does not describe them and the facts
  do not count them.
- **"What's new".** Release notes stay on the Help page. Not worth a second copy now.

---

## Decisions

### D1: App knowledge sits beside the workspace, never in it

Guide sections and facts are not stored in the database, not in the FTS index, not embedded,
not exported and not synced. They are built in memory for each question.

Putting them in the index would make them part of the user's material: they would turn up in
search results, in exports, in the agent's `search` action, and on a second machine through
sync. They are about the app, not the user's material, and they belong to the engine build that
is running.

### D2: The guide is markdown bundled into the engine

New directory `core/crates/api-server/guide/`, compiled in with `include_str!`. One file per
area:

| File | Covers |
|---|---|
| `pages.md` | Each sidebar page: Home, Record, Library, Notes, My Tasks, Ask, Agent, Connectors, Trash |
| `settings.md` | Each Settings page and the settings on it |
| `help.md` | The Help documentation topics, shortcuts and support |
| `about.md` | What Notewise is, where data lives, how updates work, what works and what does not |

Each `## ` heading is one section. The lines straight under it are metadata, then the body:

```markdown
## Settings: Models
page: settings/models
keywords: model, models, llm, ai, provider, ollama, anthropic, groq, api key, routing, switch, change
live: models

Which AI model writes summaries and answers questions, and where it runs. ...
```

- `page` is the desktop hash route without `#/`. Required.
- `keywords` are the words a user would use that the body might not: synonyms, plurals, the
  names people give things. Required.
- `live` names one or more live values, comma-separated, appended to the body at ask time (D4).
  Optional. The Recording page's section, for example, carries `live: recording, retention,
  detection`.

The engine, not the desktop, owns this so that every surface that calls `/v1/ask` gets it, and
so the text ships with the build it describes. It is MIT like the rest of `core/`.

Settings links target the nine pages from `feat/settings-categories` (appearance, recording,
transcription, models, summaries, assistant, tools, search, speakers). This work rebases onto
that branch before the guide is written.

Guide text follows the repo's prose rules, including no em dashes (`chore/no-em-dashes`).

### D3: One word-matching scorer for guide sections and facts

A section is scored against the question's terms (`retrieval::terms`, so the same stopwords):

- a term matching a word in the **title** scores 3, in **keywords** 2, in the **body** 1. The
  part of a title before a colon ("Settings: ") is not scored;
- two words match when they are the same once a common ending is taken off, leaving at least 4
  letters ("light"/"lighter", "record"/"recordings", "deleting"/"deleted", "notes"/"note").
  Plain prefix matching was tried first and had "mode" find "model", sending every dark-mode
  question to the Models page;
- each term counts once per section, at its best field;
- everyday words (meeting, note, task, recording, summary, settings, change, open, use, ...)
  add to the score but never make a section a candidate, unless the question has no other words.
  "What did Dana say in the meeting about pricing?" is about pricing; "where are my notes?" is
  about the Notes page.

A section is a candidate when some other word matches its title or keywords. Candidates are
ranked by score, and those under two thirds of the best are dropped, at most 3.

Word matching rather than embeddings because the guide is small, hand-written and ours. Missing
synonyms are fixed by adding a keyword, which is deterministic and testable; an embedding miss
is not. It also adds no model call and works with Ollama stopped.

A content question such as "what did we decide about pricing?" matches no section, so it costs
nothing and changes nothing.

### D4: Facts are computed when asked, never cached

**Live values** inside guide sections, named by `live:`, computed only for the sections a question
found. The Models section gains a line like
"Right now: llama3.1:8b through Ollama, on this machine." These come from the same engine state
the Settings page reads:

| `live` | From |
|---|---|
| `models` | `state.ai()`: resolved model label, backend, local or not; routing rule count |
| `appearance` | stored preferences (`/v1/preferences`), read best-effort: mode and accent if present |
| `transcription` | the downloaded transcription models and the recommended one (there is no per-recording choice to report) |
| `recording` | microphone and system audio permission state (as `/v1/setup` reports it) |
| `retention` | the audio retention policy |
| `detection` | meeting detection on or off |
| `search` | whether semantic search is on, and how much is indexed |
| `speakers` | the speaker separation model |
| `about` | version, schema version, whether this build can record, whether AI is local |

The workspace facts are guide sections too, in `workspace.md`, whose body is one sentence and a
live value. One mechanism rather than two:

| Section | `page` | Content |
|---|---|---|
| Your workspace in numbers | `library` | recordings, notes, open and done tasks, meetings and notes in the trash, oldest and newest recording |
| Recent recordings | `library` | the 15 most recent: title, date, length |
| Open tasks | `tasks` | up to 20: what, owner, due date, which meeting |
| In the trash | `trash` | counts, and up to 20 titles with when each was deleted |

Their keywords are about counting and time ("how many, count, total, recent, yesterday, due"),
not the names of things, so "how many recordings do I have" finds the numbers without every
question that mentions a meeting doing so. The date-based ones say what today is: without it,
`llama3.1:8b` answered "what did I record yesterday?" with a recording from two days before.

The counts and lists are new repository methods in `storage` (SQL lives only there): counts of
live and trashed meetings and notes, counts of action items by status, and open action items
across meetings with their meeting title. "Recordings" means meetings that are not in the trash,
which is what Library shows.

Never cached: a cached count is wrong the moment something is recorded or deleted, and the
queries are cheap.

### D5: App sections go through the same path as everything else

In `ask_workspace`:

1. Retrieve workspace passages as today (`gather_hybrid`).
2. Score app sections (guide and facts) against the same retrieval query; take up to 3.
3. Combine: workspace passages first, then app sections. App sections are capped at 1,500
   characters each, and together with workspace passages stay inside the existing
   `MAX_PASSAGES` and `MAX_CONTEXT_CHARS`. When over budget, workspace passages lose their tail
   first: an app section was matched by the question's own words, which is stronger evidence
   than being the eighth-best transcript.
4. `keep_relevant` judges the combined list, unchanged except for its prompt saying passages come
   "from their workspace and from Notewise's own guide".
5. Refusal, citations and `considered` work exactly as now over the combined list.

`GROUNDING_RULES` already says "answer only from the material above". Guide and facts are
material, so "how many recordings" is answered from the numbers section and not from the model's
idea of how many recordings people usually have.

### D6: A new citation kind, `app`, that opens a page

`Passage` currently has `kind` and `id: Id`. App sections have no id; they have a page. `Passage`
gets a target that is either an entity id or a page route, and citations serialise as:

```json
{ "n": 3, "kind": "meeting", "id": "0192f4a1-7c3e-7d10-9a55-2f1e8b6c4d20", "title": "Pricing review" }
{ "n": 4, "kind": "app", "page": "settings/models", "title": "Settings: Models" }
```

Existing kinds keep their exact shape, so nothing that reads them breaks.

On the desktop, `Citation` becomes a union, and `citationRoute` turns an `app` citation into a
route with the existing `parseRoute`. The chip reads "Settings: Models", "Trash" and so on, and
clicking it opens that page. The Ask panel already stays open while you navigate, so the answer
and the page sit side by side.

### D7: Help keeps its own text; a test catches the worst drift

Help, About and Settings each say things in their own components. The guide is a second copy
and can drift. Making Help render from the guide (option B in the discussion) removes that, at
the cost of rewriting Help's in-text links now. Not this time.

What is guarded:

- **Engine test:** every section parses, has a `page` and `keywords`, and every `live` name is one
  the engine provides.
- **Desktop test:** reads the guide files and checks that every `page` parses to the route it
  names rather than falling back to Home, and that every Settings category id has a section.

That catches a renamed or added Settings page. It cannot catch a sentence that has gone stale,
and the spec does not pretend otherwise.

### D8: Failures cost the facts, never the answer

- Building a live value or fact section fails (database error, a backend that does not answer):
  logged at `debug`, that value or section is left out, the question is still answered.
- No section matches: today's behaviour, unchanged.
- No new model calls are added.

### D9: The desktop says what Ask now covers

- The Ask intro, in both `AskPanel` and `AskView`, now opens "Answered only from your own
  meetings and notes", followed by "never from what the model happens to know". It becomes
  "Answered only from your own meetings and notes and from Notewise's own settings and guide,
  never from what the model happens to know." The sentence about references stays.
- `SUGGESTIONS` swaps "Who has raised concerns about the timeline?" for "How many recordings do
  I have?", so one of the three shows that Ask covers the app. The other two are about content
  and stay.

---

## Testing

**Engine (mock AI router, in-memory database):**

- Guide parsing: every section has a page and keywords; `live` names resolve; prefix matching
  behaves as in D3, including that "on" does not match "only".
- "How many recordings do I have?" with three meetings seeded and one trashed: an `app` citation
  with page `library`, and the numbers section says 2 recordings and 1 in the trash.
- "How to change models?": an `app` citation with page `settings/models`, and the section carries
  the live model line.
- "What's in the trash?": page `trash`, with the trashed titles.
- "What did we decide about pricing?" with a pricing meeting: no `app` citations.
- A fact that fails to build: the answer still comes back, without it.
- Budget: with eight long workspace passages and a matching app section, the app section is
  kept and the total stays inside `MAX_CONTEXT_CHARS`.
- Existing `ask.rs` and `retrieval.rs` tests pass unchanged; existing citation shapes are
  byte-for-byte the same.

**Desktop:**

- `citationRoute` for an `app` citation returns the parsed route.
- The guide drift test from D7.

**Measured, by hand, on `llama3.1:8b` in the built app** (not claimable from CI):

- "how to change models?" (the screenshot that started this)
- "how many recordings do I have?", "what's in the trash?", "what are my open tasks?"
- "what version is this?", "is my microphone allowed?", "how do I make it light mode?"
- "what did we decide about pricing?" against a real meeting, to confirm content answers
  are unchanged.

Results go in the PR description, misses included. A miss that a keyword fixes gets the keyword;
a pattern of misses is the evidence for adding semantic matching.

## Files

| File | Change |
|---|---|
| `core/crates/api-server/guide/*.md` | new, the guide |
| `core/crates/api-server/src/guide.rs` | new: parse sections, score, render with live values |
| `core/crates/api-server/src/facts.rs` | new: live values and fact sections |
| `core/crates/api-server/src/retrieval.rs` | `Passage` target, `app` citations, judge wording |
| `core/crates/api-server/src/ask.rs` | combine app sections in `ask_workspace` |
| `core/crates/storage/src/repositories/{meeting,note,summary}.rs` | counts and open action items |
| `apps/desktop/src/lib/api.ts` | `Citation` union |
| `apps/desktop/src/lib/ask.ts` | `citationRoute` for `app`, suggestions |
| `apps/desktop/src/components/AskPanel.tsx`, `views/AskView.tsx` | intro text |
| `apps/desktop/src/lib/guide.test.ts` | new, the drift test |
