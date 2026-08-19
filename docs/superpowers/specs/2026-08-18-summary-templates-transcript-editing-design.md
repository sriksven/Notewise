# Summary templates, transcript editing, and meeting titles — design

**Date:** 2026-08-18
**Status:** draft, awaiting review
**Scope:** Spec 3 of the program map. The meeting-surface gaps that need no new native code.

---

## Why this exists

Three specific things a user cannot currently do.

**Change how a meeting is summarised.** One prompt produces every summary. A sales call and an
architecture review get the same treatment, and a user who wants "decisions and owners only"
has no way to ask.

**Fix a wrong transcript.** `MeetingRepository` can set a segment's speaker
(`set_segment_speaker`) and rename a speaker across a meeting (`rename_speaker`), but there is
no method that changes a segment's *text*. A mis-transcribed name is permanent, and it is
permanent in the search index too. `d63ce93 feat(transcription): the models that fix a bad
transcript` improved the input; nothing lets a human fix the output.

**Rename a meeting.** `Meeting.title` is set at `create` and never again. There is no
`set_title`, and nothing generates a title from content — so a recording started from a hotkey
is titled whatever the caller guessed before the meeting happened.

## Goals

- Named, editable summary templates, with the template that produced a summary recorded.
- Regenerate a summary without destroying the previous one.
- Edit transcript segment text, with the search and semantic indexes staying correct.
- Set a meeting title, and generate one from the transcript when the user wants that.
- Name the diarization choices the engine already supports so a user can pick one.

## Non-goals

- **Click-to-seek from a transcript segment.** See D3.4 — this needs audio retention, which is
  a privacy decision, not a UI feature. Separate spec.
- **Real-time transcription.** The pipeline question is separate from the editing question.
- **Editing segment timings or splitting/merging segments.** Text only.
- **Collaborative or multi-user editing.** Single writer.
- **Template sharing or a template marketplace.** Local templates only.

---

## Decisions

### D3.1 — Summaries stay append-only; regenerate creates a new row

`SummaryRepository` already has `create`, `list_for_meeting`, `latest_for_meeting`, and
`delete`, with no `update`. Regenerating therefore means creating another summary and letting
`latest_for_meeting` win.

This is already the right shape and needs no change. A summary records which model produced it
so it can be audited later; overwriting it in place would destroy exactly the history that field
exists to preserve. A user who regenerates with a different template and prefers the old one
can still see it.

`delete` remains available for pruning. The UI shows the latest and offers previous versions
rather than presenting a list nobody asked for.

### D3.2 — Templates are rows; the template that ran is recorded on the summary

Two additions:

```sql
CREATE TABLE summary_templates (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL UNIQUE,
    prompt      TEXT NOT NULL,
    is_builtin  INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

ALTER TABLE summaries ADD COLUMN template_id TEXT REFERENCES summary_templates(id);
```

A table rather than a settings JSON blob, unlike the routing rules in Spec 2, because these
*are* queried individually, referenced by foreign key from `summaries`, and edited one at a
time. The opposite call to R8, for the opposite reasons.

`template_id` is nullable and `ON DELETE` is left as the default restrict-free reference: a
summary produced before templates existed has none, and deleting a template must not orphan or
erase the record of what produced a summary. Built-ins are seeded rows with `is_builtin = 1`,
not hardcoded constants, so the user can copy and modify one.

Three built-ins seeded by the migration, matching the shapes that actually differ: **General**,
**Sales call**, **Engineering review**.

**Schema version:** assigned at implementation time. This spec assumes it lands after Spec 1's
v11; if Spec 3 is implemented first it takes v11 and Spec 1 takes v12. Nothing here depends on
Spec 1.

### D3.3 — Editing a segment invalidates its embedding, and the FTS index takes care of itself

The v9 migration added `segments_au AFTER UPDATE ON transcript_segments`, so an `UPDATE` to
`text` re-indexes FTS automatically. Transcript editing needs no trigger work — this was already
solved, for the speaker column, and it generalises.

The semantic index does not take care of itself. `indexing.rs` decides staleness by comparing an
entity's `updated_at` against the newest chunk stored for it, and `transcript_segments` has no
`updated_at` column. An edited segment would keep its old vector indefinitely, so search would
find the text the user just corrected.

Rather than add a column and teach the indexing pass a fourth staleness rule,
`set_segment_text` calls `EmbeddingRepository::delete_for_entity("transcript_segment", id)` in
the same transaction. A missing vector is already a state the indexing pass handles — it is what
a never-indexed entity looks like — so the next pass rebuilds it and nothing new has to be
understood.

**Rejected — adding `updated_at` to `transcript_segments`.** It is the more general fix and
would be right if segments were edited often. They are not, the column would be written on every
one of thousands of inserts per meeting to serve a rare update, and the delete-and-rebuild path
reuses machinery that already exists and is already tested.

### D3.4 — Click-to-seek is excluded, because audio is not retained

There is no `audio_path` on `Meeting`, no audio column in any table, and nothing in `recorder`
writes a file: capture streams to transcription and the samples are gone. Jumping from a
transcript line to the moment it was said has nothing to jump into.

Making it work means keeping raw meeting audio on disk, which is a decision about a recording of
other people's voices — the same class of decision the `people` table's `voice_print` columns are
already gated on, with an explicit note that populating them waits for a consent and
encryption-at-rest decision. Retention length, encryption, and what happens on trash and on
export all have to be answered together.

That is a spec, not a checkbox, and bundling it here would smuggle a privacy decision in behind
a convenience feature.

### D3.5 — Title generation is an `ai-router` module, not a prompt in `api-server`

`ai-router` already holds `email.rs` and `clarify.rs` — prompt-and-orchestration modules that
own a task's wording and its failure modes. A new `title.rs` follows them.

Putting the prompt in `api-server` would work, and would put the first model prompt outside the
crate whose entire purpose is to be the only place model interaction lives. The rule in
`CLAUDE.md` is about provider SDKs, so this would not break it — it would just start the drift
it exists to prevent.

Titles are generated from the first few minutes of transcript rather than the whole thing:
a title is established early, and sending ninety minutes to name a meeting is the exact waste
Spec 2's routing exists to eliminate. This call is `TaskKind::Chat`-shaped and lands on the cheap
route.

Generation is explicit — a button, and optionally an automatic pass when a recording ends and
its title is still the placeholder the caller supplied. Never overwrites a title a human typed.

### D3.6 — Diarization tiers name what exists rather than adding modes

The engine already supports no diarization, diarization, and diarization plus acoustic
separation (`500df1a`, off by default). Three behaviours with no user-facing names.

This spec names them **Off**, **Basic**, and **Full**, and surfaces the cost of each — Full's
extra processing time and its preference for a GPU — at the point of choosing. No new
diarization capability, one new setting and the honest labelling of an existing trade-off.

The default stays where it is. A tier list is not an invitation to change a default that was
chosen deliberately.

---

## Architecture

| Location | Contents | New? |
|---|---|---|
| `storage/src/migrations.rs` | `summary_templates`, `summaries.template_id`, built-in seeds | edit |
| `storage/src/repositories/summary.rs` | Template CRUD; `template_id` on create | edit |
| `storage/src/repositories/meeting.rs` | `set_title`, `set_segment_text` | edit |
| `ai-router/src/title.rs` | Title generation prompt and parsing | new |
| `api-server/src/routes.rs` | Template CRUD, regenerate, retitle, segment edit | edit |
| `apps/desktop/src/views/SummaryView.tsx` | Template picker, regenerate, version history | edit |
| `apps/desktop/src/views/RecordView.tsx` | Inline segment editing | edit |

No new crate. No native code. Nothing here needs a GPU, a key, or a signed bundle, so all of it
is verifiable in CI.

### Repository additions

```rust
impl MeetingRepository<'_> {
    pub fn set_title(&self, id: Id, title: &str) -> Result<Meeting>;
    /// Updates text, re-indexes FTS via the existing trigger, and drops the
    /// segment's embedding so the next indexing pass rebuilds it.
    pub fn set_segment_text(&self, segment_id: Id, text: &str) -> Result<TranscriptSegment>;
}

impl SummaryRepository<'_> {
    pub fn create_template(&self, new: NewSummaryTemplate) -> Result<SummaryTemplate>;
    pub fn update_template(&self, id: Id, name: &str, prompt: &str) -> Result<SummaryTemplate>;
    pub fn delete_template(&self, id: Id) -> Result<()>;   // refuses is_builtin
    pub fn templates(&self) -> Result<Vec<SummaryTemplate>>;
    pub fn template(&self, id: Id) -> Result<SummaryTemplate>;
}
```

`delete_template` refuses to delete a built-in rather than allowing a user to empty the list and
reach a state where summarising is impossible. Copy-then-edit is the supported path.

## Data flow

```
regenerate summary
  └─> read chosen template prompt
  └─> AiBackend::summarize with that prompt   (routed per Spec 2)
  └─> SummaryRepository::create { text, model, template_id }
  └─> extract decisions / action items as today, linked to the new summary
  └─> latest_for_meeting now returns the new one; previous rows remain

edit a segment
  └─> set_segment_text in one transaction:
        ├─> UPDATE transcript_segments        (segments_au re-indexes FTS)
        └─> delete_for_entity("transcript_segment", id)
  └─> next indexing pass re-embeds it

retitle
  └─> explicit: set_title
  └─> generated: title::generate(first N minutes) -> set_title
        └─> only when the current title is still the placeholder
```

Decisions and action items attach to the new summary, and the existing schema already allows
this: `NewDecision.summary_id` is `Option<Id>` and the v6 migration deliberately stopped
cascading them from `summaries` so re-summarising does not silently delete a user's work items.
Regenerate therefore adds without destroying, which is the behaviour that migration was written
to make possible.

## Error handling

No new error variants. Every failure here is an existing one:

| Condition | Handling |
|---|---|
| Template name collides | `StorageError` from the `UNIQUE` constraint, surfaced as 409 |
| Deleting a built-in | Refused in the repository, 400 at the boundary |
| Title generation fails | Leave the title unchanged. A meeting with a placeholder title is fine; a meeting with an error where its title should be is not |
| Model unavailable during regenerate | Existing `AiError` path; the previous summary is still there |
| Segment edit on a trashed meeting | Refused, consistent with the rest of the trash behaviour |

Title generation failing silently is deliberate. It is a convenience on a background path, and
an error toast for a cosmetic feature trains users to dismiss toasts.

## Testing

All in CI, no model and no hardware:

- Template CRUD, unique-name violation, built-in deletion refused, built-ins present after
  migration.
- Regenerate: creates a second summary, `latest_for_meeting` returns it, the first still
  readable, `template_id` recorded, prior decisions and action items survive.
- `set_segment_text`: text changes; FTS finds the new text and no longer finds the old (this
  exercises the v9 `segments_au` trigger); the segment's embedding row is gone; a second
  indexing pass would rebuild it.
- `set_title` round-trip; generated title does not overwrite a human-set title; generation
  failure leaves the title untouched.
- Diarization tier setting round-trips and maps to the existing engine behaviours.
- Migration forward test including the seeded built-ins and the nullable `template_id` on
  pre-existing summaries.

`MockBackend` covers title generation and regeneration deterministically.

## What this delivers

1. `summary_templates` with three built-ins, full CRUD, and the template recorded on every
   summary it produces.
2. Regenerate, non-destructively, preserving prior summaries and prior work items.
3. `set_segment_text` with both indexes staying correct.
4. `set_title` plus generated titles that never overwrite a human's.
5. Named diarization tiers over existing engine behaviour.

## Risks and open questions

- **Prompt injection through templates** is a smaller version of the problem `email.rs`
  documents. A user-authored template is trusted input in a way a transcript is not, but a
  template that says "ignore the transcript and write a resignation letter" will do that. Local,
  user-authored, single-user — acceptable, worth stating.
- **Version history has no pruning policy.** Someone who regenerates fifty times keeps fifty
  summaries. `delete` exists; nothing calls it automatically.
- **Title generation on the cheap route** may produce weak titles precisely because it is routed
  to the small model. If that is bad in practice the fix is a route, not a code change.
- **Editing text does not re-run extraction.** A corrected transcript does not update the
  decisions and action items derived from the uncorrected one. Regenerating does. Whether an
  edit should prompt for that is unresolved.
