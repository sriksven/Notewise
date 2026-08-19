# Audio retention — design

**Date:** 2026-08-18
**Status:** draft, awaiting review
**Scope:** Spec 11. Keeping recorded audio, and what that unlocks. Deferred out of Spec 3 (D3.4).

---

## Why this exists

Notewise does not keep audio. `recorder` streams capture into transcription and the samples are gone;
there is no `audio_path` on `Meeting` and no audio column in any table. The transcript is the only
artifact.

Two capabilities are blocked by that, and one of them matters more than the obvious one.

**Click-to-seek.** Clicking a transcript line to hear it said. The feature Spec 3 wanted and could not
have.

**Re-transcription.** `d63ce93 feat(transcription): the models that fix a bad transcript` added better
models, and the `eval` crate exists precisely because "a real recording came back with four invented
`Okay.` segments and one speaker split into two." A user who hits that today has no recourse — the audio
that would produce a better transcript no longer exists. Keeping it turns a permanent bad transcript
into a re-run. Diarization improvements have the same property: better speaker separation is worthless
against audio you threw away.

Re-transcription is the stronger argument. Seek is nice; being able to fix a bad transcript is the
difference between a trustworthy record and a lossy one.

## Why this is its own spec

Keeping audio is a decision about storing a recording of other people's voices on disk. This codebase
already treats that class of decision as requiring its own reasoning: the `people` table's `voice_print`
columns exist but are documented as deliberately unwritten, gated on "an explicit consent and
encryption-at-rest decision," and voiceprint storage shipped off by default in `5ab364c`.

Retention length, encryption, what happens on trash, and what happens on export all have to be answered
together, and bundling them into a UI convenience spec would have smuggled a privacy decision in behind
a scrollbar.

## Goals

- Optionally retain meeting audio, off by default.
- Encrypted at rest, using whatever the database already uses.
- A retention policy the user sets, enforced automatically.
- Deletion that actually deletes, including on trash and purge.
- Enable re-transcription and seek without either becoming mandatory.

## Non-goals

- **Uploading audio anywhere.** Retention is local. Hosted STT is a separate opt-in with separate
  consent.
- **Retaining audio for meetings recorded before this ships.** It does not exist.
- **Per-participant consent capture.** See R3 — out of scope and deliberately not pretended to.
- **Audio editing.** Read-only.

---

## Decisions

### R1 — Off by default, and the transcript path is unchanged when off

Retention is a setting, default off. With it off, `recorder` behaves exactly as today: samples stream
through and are discarded.

Every privacy-sensitive default in this repo points the same way, and this is the most sensitive of
them. A user who upgrades must not discover later that the app started keeping recordings.

### R2 — Audio is stored as files with a database pointer, not as blobs

Files live under the app data directory in a per-meeting layout; `meetings` gains a nullable
`audio_path` and `audio_bytes`.

An hour of audio is tens of megabytes. Putting that in SQLite bloats the database file, makes every
backup copy it, and slows `VACUUM`. Files also let deletion be a filesystem operation that can be
verified.

The pointer being nullable is what makes retention optional per meeting rather than global — a user can
enable retention later without every earlier meeting appearing broken.

### R3 — Encryption uses the database's existing at-rest mechanism, and consent is not claimed

Audio files are encrypted with the same scheme and key material the database already uses for
encryption at rest. A separate mechanism would mean a second key to manage and a second thing to get
wrong.

On consent, this spec is explicit about what it does *not* do: it does not obtain consent from meeting
participants, and it does not pretend that a setting the user toggled constitutes it. What it provides
is that retention is off unless chosen, visible when on, bounded by a policy, and deletable. Whether
recording and retaining a given meeting is lawful or appropriate is the user's responsibility, and the
UI should say that plainly rather than implying the software has handled it.

Claiming otherwise would be worse than saying nothing.

### R4 — Retention is time-bounded by default, and the default is short

Policies: **off**, **until the meeting is deleted**, or **N days** with a default of 30.

An unbounded default would silently consume tens of gigabytes a year, and the value of audio decays
fast — re-transcription and seek are both overwhelmingly used soon after a meeting. A short default
gets the benefit and limits the exposure.

Enforcement is a pass, not a timer per file: a sweep deletes audio past its policy and clears the
pointer. It runs on the same schedule mechanism Spec 7 provides, and on demand.

### R5 — Deleting a meeting deletes its audio, and trash is not deletion

Trashing a meeting does **not** delete its audio, because trash is recoverable and a restore with no
audio would be a silent partial recovery. Purging does delete it.

`meetings` already has `deleted_at` for trash, and `purge`/`empty_trash` already exist for notes with
the same distinction. Audio follows the model already in the schema.

Deletion is unlinking the file and clearing the pointer, in that order, so a crash between them leaves
a pointer to a missing file — which reads as "no audio" — rather than a file with no pointer, which is
an orphan nothing will ever clean up.

### R6 — Re-transcription creates a new transcript; it does not mutate the old one

Re-running transcription on retained audio produces new segments and, per Spec 3's D3.1 reasoning about
summaries, does not destroy what was there. The user compares and chooses.

Silently replacing a transcript would discard human corrections — exactly the edits Spec 3's
`set_segment_text` exists to allow. A user who fixed twelve names and then re-transcribed would lose all
twelve with no warning.

### R7 — Export never includes audio unless explicitly requested

The vault sink and every other export path continue to write text only. Audio is included only by an
explicit export action that names it.

The vault mirrors to a user-chosen folder that may be a synced drive. Silently placing encrypted-at-rest
audio into Dropbox, decrypted, because a user enabled retention for seek, is precisely the kind of
surprise this whole spec exists to avoid.

---

## Architecture

| Location | Contents | New? |
|---|---|---|
| `recorder/src/` | Optional tee of captured samples to an encrypted file | edit |
| `storage/src/migrations.rs` | `meetings.audio_path`, `meetings.audio_bytes`, retention setting | edit |
| `storage/src/repositories/meeting.rs` | `set_audio`, `clear_audio`, `audio_due_for_deletion` | edit |
| `api-server/src/audio.rs` | Range-serving for playback, re-transcribe trigger, sweep | new |
| `apps/desktop/src/views/RecordView.tsx` | Seek from a segment, re-transcribe action | edit |

No new crate. The encryption primitive is whatever `storage` already uses; `recorder` gains a writer,
not a scheme.

Playback is served by `api-server` over loopback with HTTP range requests, so the desktop webview can
seek without loading an hour of audio into memory. `api-server` already binds loopback-only and refuses
anything else.

## Data flow

```
record (retention on)
  └─> capture ──┬──► transcription  (unchanged)
                └──► encrypted writer ──► <data>/audio/<meeting_id>.enc
  └─> on stop: set_audio(path, bytes)

seek
  └─> segment.start_ms ──► GET /v1/meetings/:id/audio  (Range) ──► decrypt-on-read ──► play

re-transcribe
  └─> decrypt to a temp file ──► transcription with the chosen model
  └─> new segments written alongside; existing segments untouched (R6)
  └─> temp file removed

sweep (scheduled + on demand)
  └─> audio_due_for_deletion(policy, now)
  └─> unlink file, then clear_audio()   (R5 ordering)
```

## Error handling

| Condition | Handling |
|---|---|
| Disk full mid-recording | Stop retaining, keep transcribing, tell the user. Never fail the recording |
| Encryption key unavailable | Retention disabled for the session; transcription unaffected |
| Pointer set but file missing | Treated as no audio; pointer cleared on next sweep |
| Decrypt fails on playback | Reported; the transcript is unaffected |
| Re-transcription fails | Existing transcript untouched |
| Sweep cannot delete a file | Pointer left set, retried next sweep; never cleared without deletion |

The first row is the important one. Audio retention is an enhancement to a recording; a full disk must
degrade it, never lose the meeting.

## Testing

In CI, with synthetic audio — `audio-capture` already provides `SyntheticSource` and `FileSource` that
work everywhere:

- Retention off: no file written, pointer stays null, transcript identical.
- Retention on: file written, encrypted (ciphertext does not contain known plaintext), pointer set.
- Round-trip decrypt of a written file.
- Range requests return correct byte ranges, including the final partial range.
- Sweep: deletes past-policy audio and clears pointers; `until deleted` policy retains; a file it
  cannot unlink keeps its pointer.
- Trash retains audio; restore still has it; purge deletes it; `empty_trash` deletes all of it.
- Ordering: a simulated crash between unlink and clear leaves a pointer to a missing file, and the next
  sweep resolves it.
- Re-transcription adds segments without modifying or deleting existing ones, including human-edited
  ones.
- Disk-full during write degrades to no retention with the transcript intact.
- Export paths contain no audio unless explicitly requested.

`#[ignore]`d with reasons: anything requiring real microphone hardware, and GPU-backed re-transcription.

## What this delivers

1. Optional, off-by-default audio retention encrypted with the existing at-rest mechanism.
2. Three retention policies with a 30-day default, enforced by a sweep.
3. Deletion semantics that respect trash-versus-purge and cannot orphan files.
4. Click-to-seek, unblocking Spec 3's D3.4.
5. Re-transcription that adds rather than replaces, preserving human corrections.
6. Export that never leaks audio implicitly.

## Risks and open questions

- **This is the highest-consequence storage decision in the program.** A bug that leaves decrypted
  audio on disk, or that fails to delete on purge, is a serious privacy failure rather than a bug.
- **R3 declines to solve consent.** That is honest but leaves users in jurisdictions with all-party
  consent requirements to handle it themselves, with only a UI warning.
- **Disk usage** at 30 days of daily meetings is substantial and users will be surprised by it; the
  settings UI needs to show current usage, not just the policy.
- **Encryption key handling** is inherited rather than designed here, so this spec is only as strong as
  the existing at-rest mechanism — which should be reviewed before implementation rather than assumed.
- **Re-transcription doubles storage of segments** for a meeting and there is no pruning policy for
  superseded transcripts, the same open question Spec 3 leaves for summaries.
