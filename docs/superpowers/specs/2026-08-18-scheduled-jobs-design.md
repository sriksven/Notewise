# Scheduled jobs — design

**Date:** 2026-08-18
**Status:** draft, awaiting review
**Scope:** Spec 7 of the program map. Recurring unattended work over the workspace.

---

## Why this exists

Everything the engine does today is triggered by a person in the app. There is no scheduler
anywhere in the workspace — a grep for one finds only the word in comments.

The work that most wants scheduling is exactly the work nobody does manually: a Friday digest of
the week's decisions, a Monday list of action items that went overdue, a morning brief of today's
meetings from the calendar Spec 1 imports. Each is a question the agent in `agent.rs` can already
answer. None of them gets asked, because asking requires remembering.

## Goals

- Recurring jobs on a cron schedule, using the agent that already exists.
- A durable record of every run: what it did, what it produced, whether it worked.
- Per-job capability scoping, so a job can do less than the user can.
- Notification on completion, reusing what Spec 5 delivers.
- Nothing irreversible happens without a human, even on a schedule.

## Non-goals

- **Executing external MCP tools unattended.** See S1. This is the central constraint.
- **Multi-user or per-user jobs.** Single-user, matching AnythingLLM's own limitation and the
  current state of the product.
- **Running when the app is closed.** No daemon, no launch agent. See S5.
- **Backfilling missed runs.** See S5.
- **A visual cron builder.** A cron string plus a plain-English preview of the next few fire
  times; the builder UI is a frontend concern, not a design decision.
- **Jobs that modify existing workspace data.** Inherited from `agent.rs`'s blast radius.

---

## Decisions

### S1 — A scheduled job may propose external tool calls; it may never execute them

Spec 6's M2 requires a human to confirm every external MCP tool call, every time. A scheduled job
is by definition unattended. These cannot both hold if jobs can execute tools, and M2 is the one
that stays.

So a job run may *produce proposals*. They land in `tool_executions` with status `proposed`, and
they wait. The completion notification says "3 actions proposed for review" and the user confirms
them the same way they confirm any other proposal.

This is the entire reason Spec 6 made the tool allowlist a first-class object rather than a global
switch: a job gets a subset, and that subset governs what it may *propose*.

**Rejected — allowing pre-approved tools on a schedule.** It is what AnythingLLM does and it is
the shortest path to this product acquiring an unattended send path, which nothing in it has by
design. A scheduled job that can act in other people's systems while nobody is watching is a
different product with a different risk profile.

What a job *can* do without confirmation is exactly what `agent.rs` can already do: search, read,
and write one note. Those are reversible and local.

### S2 — Runs are persisted, with their traces

`agent.rs` keeps runs in memory and argues that a trace is only interesting while it is happening
or just after, and that the note it wrote survives anyway.

That argument depends on someone having been present. For a scheduled run, nobody was. The trace is
the only account of what happened at 6am, and "the job failed on Tuesday and I need to know why" is
the normal case, not an edge case.

So `job_runs` holds status, timings, the step trace, and what was produced. This is the same
divergence Spec 6 makes for `tool_executions`, on the same principle: unwitnessed or irreversible
work gets a durable record.

Retention is bounded — the last N runs per job, pruned on write. An unbounded trace table on a
job firing every fifteen minutes is a disk-space bug with a slow fuse.

### S3 — The scheduler is a task inside `api-server`, not a new crate

`api-server` is already the long-running process that owns the tokio runtime, holds
`Arc<dyn AiBackend>`, and serves the agent. A scheduler is a loop that decides when to invoke work
that already lives there.

A separate crate would need the agent, the router, the repositories, and the notification path
handed to it, which is to say it would need `api-server`'s entire state for no isolation benefit.

The scheduling *decision* logic — given cron expressions and a clock, which jobs are due — is a
pure module with an injected clock, testable without waiting for wall time.

### S4 — One run per job at a time; a job still running is skipped, not queued

If a job's previous run has not finished when its next fire time arrives, the new occurrence is
skipped and the skip is recorded.

Queueing means a job that takes longer than its interval accumulates a backlog that never drains,
and each run costs model calls. Skipping is self-correcting and visible in the run history, which is
where a user can see that their schedule is too tight.

Concurrency across *different* jobs is bounded by a small limit so several heavy jobs firing at
midnight do not saturate a local model.

### S5 — Missed occurrences are not backfilled

The app is not a daemon. If it was closed from Friday to Monday, Monday's launch does not fire three
days of jobs.

Backfilling produces the worst first impression this feature could have: opening the app after a
holiday and receiving a burst of stale digests, each costing model time, describing a week the user
already lived through. At most, a job marked `catch_up` runs its single most recent missed occurrence
once, and that flag defaults off.

The run history records the gap so it is visible rather than silently absent.

### S6 — Job output lands as a note, which is a thing that already exists

A completed run writes a note, using the path `agent.rs` already uses. No new "job results
workspace", no new entity type.

AnythingLLM auto-creates a dedicated workspace for job results. Notewise already has notes, which
are searchable, indexable by `indexing.rs`, editable in the block editor, and exportable through the
vault sink. Inventing a parallel container would put job output outside every one of those.

The note is linked to the run, so the run history points at what it produced and the note points
back at how it was made.

### S7 — Cron expressions, parsed by a dependency, validated at save time

Standard five-field cron, plus a validated preview of the next several fire times shown before the
job is saved. An invalid expression is rejected at save, not discovered at 3am.

A cron parsing crate is a new workspace dependency. Writing one is a bad use of effort and a good
source of off-by-one bugs around DST.

Times are interpreted in the machine's local timezone, because "every Friday at 5pm" means the
user's Friday. This is stored explicitly alongside the expression so a DST transition or a
relocation is interpretable rather than mysterious.

---

## Architecture

```
api-server
  ├── scheduler task (tokio)
  │     └── tick every 30s: due_jobs(now, &jobs)   ← pure, injected clock
  │           └── for each due job, if not already running:
  │                 ├── job_runs row: 'running'
  │                 ├── run the agent with the job prompt
  │                 │     ├── allowed: search / read / write one note
  │                 │     └── allowed: PROPOSE tool calls within the job's allowlist
  │                 ├── job_runs: 'completed' | 'failed' | 'timed_out'
  │                 └── NotificationRepository::create(Desktop)
  └── routes: job CRUD, enable/disable, run now, run history, trace
```

| Location | Contents | New? |
|---|---|---|
| `api-server/src/jobs/schedule.rs` | Pure due-time logic, injected clock | new |
| `api-server/src/jobs/runner.rs` | Run execution, trace capture, pruning | new |
| `api-server/src/jobs/routes.rs` | CRUD, run-now, history, trace | new |
| `storage/src/migrations.rs` | `jobs`, `job_runs`, `job_allowed_tools` | edit |
| `storage/src/repositories/job.rs` | `JobRepository` | new |
| `apps/desktop/src/views/` | Job list, editor with fire-time preview, run history | new views |

No new crate; one new third-party dependency for cron parsing.

### Data model

```sql
CREATE TABLE jobs (
    id           TEXT PRIMARY KEY NOT NULL,
    name         TEXT NOT NULL UNIQUE,
    prompt       TEXT NOT NULL,
    cron         TEXT NOT NULL,
    timezone     TEXT NOT NULL,
    enabled      INTEGER NOT NULL DEFAULT 1,
    catch_up     INTEGER NOT NULL DEFAULT 0,
    timeout_secs INTEGER NOT NULL DEFAULT 600,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);

CREATE TABLE job_allowed_tools (
    job_id     TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    server_id  TEXT NOT NULL,
    tool_name  TEXT NOT NULL,
    PRIMARY KEY (job_id, server_id, tool_name)
);

CREATE TABLE job_runs (
    id           TEXT PRIMARY KEY NOT NULL,
    job_id       TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    status       TEXT NOT NULL,   -- 'running'|'completed'|'failed'|'timed_out'|'skipped'
    trace        TEXT,            -- JSON array of steps
    note_id      TEXT REFERENCES notes(id) ON DELETE SET NULL,
    proposals    INTEGER NOT NULL DEFAULT 0,
    error        TEXT,
    started_at   TEXT NOT NULL,
    finished_at  TEXT
);

CREATE INDEX idx_job_runs_job ON job_runs(job_id, started_at DESC);
```

`job_allowed_tools` intentionally duplicates the shape of Spec 6's `mcp_enabled_tools` rather than
referencing it: a job's allowance must be a subset the user granted *to that job*, and expressing it
as a filter over the global set would let enabling a tool globally silently widen every job.

`note_id` is `ON DELETE SET NULL` so deleting a note does not erase the run that made it.

## Data flow

```
tick (30s)
  └─> due_jobs(now, jobs, last_run_times)      ← pure
        └─> skip if a run is already 'running'  (S4)
        └─> skip if missed and !catch_up        (S5)
  └─> per due job:
        ├─> job_runs 'running'
        ├─> agent loop with the job prompt, capped by timeout_secs
        │     ├─> search / read / write one note      (executed)
        │     └─> propose tool calls in the allowlist (NOT executed — S1)
        ├─> prune runs beyond the retention limit
        ├─> job_runs 'completed' with trace, note_id, proposal count
        └─> Notification(Desktop): "<job> finished — 1 note, 3 actions to review"
```

Proposals created by a job are ordinary Spec 6 `tool_executions` rows with status `proposed`. They
are indistinguishable from ones a human requested, which is deliberate — there is one review queue,
not two.

## Error handling

| Condition | Handling |
|---|---|
| Invalid cron at save | Rejected with the parse error; never stored |
| Model unavailable at fire time | Run `failed`, error recorded, notification says so |
| Agent exceeds `timeout_secs` | Run `timed_out`; partial trace kept |
| Job still running at next fire | New occurrence `skipped`, recorded |
| App closed over a fire time | No run; the gap is visible in history |
| Tool proposal outside the job's allowlist | Dropped and recorded in the trace; not proposed |
| Trace exceeds a size cap | Truncated with a marker rather than refusing to store the run |

A failing job does not disable itself. Auto-disabling means a transient Ollama restart silently
stops a weekly digest, and the user finds out a month later.

## Testing

The scheduling decision is pure, so the parts that can be wrong are testable without wall-clock
waits:

- `due_jobs` with an injected clock: not yet due, exactly due, overdue, disabled, already running,
  missed-with-catch-up, missed-without-catch-up.
- DST transitions in a local timezone — the spring-forward hour that does not exist and the
  fall-back hour that happens twice.
- Cron validation accepts standard expressions and rejects malformed ones at save.
- Concurrency: same job never runs twice; different jobs bounded by the limit.
- Run lifecycle for each terminal status, including timeout keeping a partial trace.
- Retention pruning keeps exactly N runs and prunes oldest first.
- Trace truncation at the cap still stores the run.
- **Tool boundary:** a job whose agent proposes a tool outside its allowlist produces no proposal;
  a job never produces a `confirmed` or `succeeded` execution. This is the S1 guarantee and gets
  the most explicit test in the spec.
- Notification enqueued on completion with the right counts.

`MockBackend` drives runs deterministically. Nothing here is `#[ignore]`d.

## What this delivers

1. Cron-scheduled jobs with validated expressions and a fire-time preview.
2. Durable run history with traces, bounded retention, and every terminal status.
3. Per-job tool allowlists that scope what a job may propose.
4. Completion notifications through the channel Spec 5 delivers.
5. Output as ordinary notes, searchable and indexable like everything else.
6. A hard guarantee, tested, that no scheduled run executes an external tool.

## Risks and open questions

- **The app must be open.** For a user who quits it nightly, a 6am job never runs. A login item or
  background helper is the fix and is a separate decision with its own trust implications.
- **Cost of unattended model calls.** A job firing every fifteen minutes against a cloud backend
  spends money while nobody watches. Spec 2's routing helps; a per-job budget does not exist.
- **S1 may frustrate.** Users who want "file these tickets every Friday" get proposals, not filed
  tickets. That is the correct trade and it will read as a limitation.
- **Trace size** on a long agent run is the most likely operational surprise; the cap is a guess.
- **A job prompt is user-authored and unvalidated,** which makes it the same trusted-input question
  Spec 3 raises about summary templates.
