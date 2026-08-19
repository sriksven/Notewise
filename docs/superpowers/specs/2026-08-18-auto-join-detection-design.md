# Auto-join detection — design

**Date:** 2026-08-18
**Status:** draft, awaiting review
**Scope:** Spec 5 of the program map. Noticing that a meeting started, and offering to record it.

---

## Why this exists

Every other feature in this product is downstream of someone remembering to press record. That
is the single largest source of lost value in the whole system, and it is not a feature gap —
it is a human one, which is why it has to be solved by the software noticing instead.

The cost of missing is total. A meeting that was not recorded has no transcript, no summary, no
decisions, no action items, and no calendar link. Every capability described in the other eight
specs produces nothing for it.

## What is already available

Almost everything needed is already built, which is why this spec is small.

**The browser extension already knows.** Its content scripts run on `meet.google.com`,
`*.zoom.us/wc/*`, `teams.microsoft.com`, and `teams.live.com`, and it already posts to the local
`api-server` — it currently sends speaker identity. A "meeting page became active" message is a
new message type on a channel that exists, is tested, and needs no new permission from anyone.

**The calendar will know** once Spec 1 lands: an event with a `join_url` whose `starts_at` is
now.

**Notification storage exists.** `NotificationRepository` has `create`, `pending_on(channel)`,
and `mark_delivered`, and `NotificationChannel::Desktop` is already a variant. Nothing has ever
delivered one. This is its first consumer.

## Goals

- Notice a meeting starting, from signals that require no new OS permission.
- Offer to record rather than recording automatically.
- Never notify twice for the same meeting.
- Degrade to silence, not to noise, when signals are absent or contradictory.
- Leave a clean seam for a native detector later without redesigning anything.

## Non-goals

- **Native process, window, or audio-session detection.** See D5.2. This is where a native
  Zoom or Teams *desktop* client would be caught, and it is deliberately not in the first cut.
- **Recording automatically without confirmation.** See D5.3.
- **Detecting who else is in the meeting.** The extension already does that separately.
- **Bot-join for meetings the user is not in.** Phase 4, `cloud/bot-service`.
- **Notification delivery for anything other than this.** A general notification system is
  Phase 3; this delivers one kind of notification for one purpose.

---

## Decisions

### D5.1 — Detection is a trait with several sources, fused by one state machine

```rust
pub struct JoinSignal {
    pub source: SignalSource,       // Extension | Calendar | Native
    pub platform: Option<Platform>, // Meet | Zoom | Teams | Unknown
    pub confidence: Confidence,     // Weak | Strong
    pub external_ref: Option<String>, // meeting URL or calendar external_id
    pub observed_at: DateTime<Utc>,
}

pub trait JoinDetector: Send + Sync + Debug {
    fn source(&self) -> SignalSource;
}
```

Signals arrive from different places at different times and are individually unreliable. A tab
being open is not a meeting; a calendar event is not attendance. Fusing them in one place means
the rules about *when to speak* live in one testable object rather than being spread across the
producers.

This mirrors what `clarify.rs` already does for live suggestions, and for the same reason stated
there: "almost all of the difficulty is in *when* to speak rather than *what* to say." That
module's structure — a pure session object holding cooldown, staleness, and dedup, separate from
the thing that produces candidates — is the pattern this follows, because it was the right answer
to the same problem.

### D5.2 — No native detection in the first cut

A native detector would catch the desktop Zoom and Teams clients, which the extension cannot see.
The permissionless way to do it is audio-session inspection — on macOS, whether an input device is
running somewhere; on Windows, enumerating audio sessions — neither of which needs a TCC grant or
Accessibility.

It is still excluded here, for three reasons. It requires `unsafe` FFI and therefore a new
quarantine crate under the pattern `macos-permissions` establishes. It cannot be verified in CI,
so it would arrive `#[ignore]`d. And it is the least certain signal of the three: a microphone in
use means a call, a voice memo, or a browser tab playing something.

The extension plus the calendar covers browser-based Meet, browser Zoom, browser Teams, and
anything on the user's calendar regardless of client. That is most meetings, for no permission and
no native code. `JoinDetector` is where the native source slots in once the fusion logic has been
proven against real use.

**Rejected — leading with the native detector.** It is the most general signal and the most
expensive one, and building it first would mean the debounce rules are first exercised by the
noisiest source with no way to test them.

### D5.3 — Notify and offer; never record unprompted

Detection starts nothing. It creates a `Notification` on `NotificationChannel::Desktop`; the
desktop shell delivers it; clicking it starts recording.

Recording unprompted would mean this product silently captures audio of other people because it
guessed a meeting began. A false positive would be a recording nobody knew about, which is a
different category of failure from a missed meeting — one is lost value, the other is a breach of
the thing the product is selling. The asymmetry decides it.

The prompt also carries the calendar title and attendees when Spec 1 has them, so the recording
starts already labelled rather than as "Meeting 2026-08-18".

### D5.4 — One notification per meeting occurrence, enforced by a dedup key

A `dedup_key` is derived from the strongest available identity: the calendar event's external id,
else the normalized meeting URL, else `platform + a coarse time bucket`. A notification is
suppressed if one with the same key was created within the session window.

Repeats are the failure that kills this feature fastest. A user who gets three notifications for
one standup turns it off, and it never gets turned back on — the same reasoning `clarify.rs`
gives for deduplicating on normalized text.

Rejoining after a genuine drop is the cost: within the window it is silent. That is the right
side to err on.

### D5.5 — A weak signal alone waits; two weak signals or one strong signal fire

| Signals | Action |
|---|---|
| Calendar event now, nothing else | Wait out a short grace period, then notify (Weak) |
| Extension reports an active meeting page | Notify (Strong) |
| Calendar event **and** extension | Notify immediately, with full calendar context |
| Extension page open, no audio, no calendar | Notify — a meeting page is deliberate |
| Calendar event that was declined | Never notify |

The grace period on a calendar-only signal exists because calendars are full of meetings nobody
attends. Waiting a couple of minutes and then asking converts "your calendar says something" into
"something appears to be happening."

Declined events are excluded using the `response_status` Spec 1 stores on
`calendar_attendees` — notifying about a meeting the user explicitly declined is the most
annoying possible false positive.

### D5.6 — Delivery is the desktop shell's job; the engine only enqueues

`api-server` writes the `Notification` row. The Tauri shell polls `pending_on(Desktop)`, delivers
via the OS notification API, and calls `mark_delivered`.

The engine must not depend on a GUI toolkit — `apps/desktop/src-tauri` is excluded from the
workspace on purpose so engine CI never pulls a webview. Enqueue-and-drain keeps that boundary
intact and matches how `connector_outbox` already separates deciding from delivering.

---

## Architecture

```
browser extension ──POST /v1/signals/join──┐
                                            │
calendar (Spec 1) ──local poll─────────────┤
                                            ▼
                                   JoinTracker  (pure state machine)
                                     ├─ dedup by key
                                     ├─ grace period for weak signals
                                     ├─ cooldown
                                     └─ decide: notify or stay silent
                                            │
                                            ▼
                              NotificationRepository::create(Desktop)
                                            │
                              Tauri shell drains pending_on(Desktop)
                                            │
                                   user clicks → start recording
                                     (title + attendees prefilled)
```

| Location | Contents | New? |
|---|---|---|
| `api-server/src/join.rs` | `JoinTracker`, signal intake route, calendar poll | new |
| `apps/browser-extension/src/` | Emit a join/leave signal on page activity | edit |
| `apps/desktop/src-tauri` | Drain `pending_on(Desktop)`, deliver, `mark_delivered` | edit |
| `apps/desktop/src/views/RecordView.tsx` | Start prefilled from a notification | edit |

No new crate, no migration, no native code. `JoinTracker` is pure and lives with the surface that
owns the routes, next to how `clarify::ClarifierSession` sits with its own logic.

`event_at(db, now)` from Spec 1 supplies the calendar signal and the prefill, which is why that
function exists in that spec.

## Data flow

```
extension: page becomes an active meeting  -> POST /v1/signals/join { platform, url }
calendar:  poll every 60s -> event_at(now + grace) with a join_url and not declined

JoinTracker::observe(signal) -> Decision
   ├─ Suppress   (dedup hit, cooldown, or declined event)
   ├─ Wait       (weak signal inside its grace period)
   └─ Notify { title, attendees, dedup_key, external_ref }
        └─> NotificationRepository::create(channel = Desktop)

shell tick -> pending_on(Desktop) -> OS notification -> mark_delivered
click -> POST /v1/recording/start { title, calendar_external_id }
      -> on the resulting meeting, the SyncedTo edge to the calendar event is written
```

The recording that results is linked to the calendar event immediately rather than waiting for
Spec 1's reconciliation pass to infer it from time overlap. A link that is *known* should never be
guessed, and this is the one path where it is known.

## Error handling

No new error variants. Every failure degrades to silence:

| Condition | Handling |
|---|---|
| Extension unreachable or not installed | Calendar signal only. Normal, not an error |
| Calendar not connected | Extension signal only |
| Neither available | No detection. The manual record button is unchanged |
| Notification creation fails | Logged; no retry. A missed prompt is recoverable by pressing record |
| Shell not running | Rows stay pending; stale ones are dropped rather than delivered late |

Stale pending notifications are dropped on drain if older than the session window. A notification
saying a meeting is starting, delivered forty minutes later, is worse than none.

## Testing

`JoinTracker` is pure, so all of the behaviour that can be wrong is testable with no browser, no
calendar account, and no OS notification:

- Strong signal notifies; weak signal waits then notifies; weak signal that disappears inside the
  grace period never notifies.
- Two weak signals corroborate and notify immediately.
- Dedup: same key twice inside the window notifies once; different keys both notify; the same
  meeting rejoined after the window notifies again.
- Declined calendar event never notifies, even with an extension signal.
- Cooldown respected across distinct meetings.
- Dedup key derivation: calendar id preferred over URL, URL normalized (query strings and
  trailing slashes ignored), coarse time bucket as the last resort.
- Prefill carries calendar title and attendees when present, placeholder when absent.
- Stale pending notification dropped rather than delivered.

Marked `#[ignore]` with a reason: anything requiring a real OS notification to appear, and the
extension's DOM selectors, which its README already documents as untestable.

## What this delivers

1. `JoinTracker` — signal fusion with dedup, grace, and cooldown, fully unit-tested.
2. A join-signal route and the extension change that feeds it, on an existing channel.
3. Calendar-driven detection using Spec 1's `event_at`, with declined events excluded.
4. The first actual delivery of `NotificationChannel::Desktop`.
5. One-click record with title and attendees prefilled, and the calendar link written directly
   rather than inferred.

## Risks and open questions

- **Native desktop Zoom and Teams are not covered.** For a user who never takes meetings in a
  browser and keeps no calendar, this feature does nothing. That is the accepted cost of D5.2, and
  it is the most likely reason someone finds this underwhelming.
- **The grace period and cooldown are guesses.** They are two constants and should be revisited
  against real use, like Spec 1's overlap threshold.
- **The extension must be installed and the desktop app running** for the strong signal to exist,
  which is the same constraint `CLAUDE.md` already notes about the extension generally.
- **Notification permission** is an OS-level grant on both platforms. A user who denied it gets
  silent detection and no indication why, and the settings UI should say so.
