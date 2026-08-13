# Sync protocol

> **Status:** the conflict-resolution logic is implemented and tested in
> [`core/crates/sync-client`](../../core/crates/sync-client/). There is **no transport yet** —
> that needs a running sync service, which is Phase 2.

## Sync is opt-in and inert by default

`sync-client` is a separate crate specifically so a local-only build never compiles it in.
"Your meetings stay on your machine" should be true of the binary, not just of its
configuration.

## Version vectors, not timestamps

Each record carries a version: one counter per device that has written it.

A last-write-wins timestamp cannot distinguish the two cases that matter:

| Situation | Timestamp says | Version vector says |
|---|---|---|
| Phone edited after syncing the laptop's change | Whichever clock is ahead | Phone **descends** — take it |
| Phone and laptop both edited independently | Whichever clock is ahead | **Diverged** — a real conflict |

The second row is the bug: with skewed clocks, last-write-wins silently discards one user's
edit and reports success. Comparing vectors answers the question a merge actually needs — did
one side descend from the other, or did they diverge?

## Resolution

| Policy | Behaviour | Can lose work |
|---|---|---|
| `KeepBoth` | **Default.** Keep both as separate records | No |
| `AskUser` | Surface it, change nothing | No |
| `PreferLocal` | Discard the remote edit | **Yes** |
| `PreferRemote` | Discard the local edit | **Yes** |

The default cannot lose work. A visible duplicate is recoverable in seconds; a silently
dropped edit is not recoverable at all, and the user may never know it happened.

The destructive policies report `discarded_an_edit: true` so the UI can tell the user. And
`ConflictPolicy::can_lose_data()` exists so the setting can be labelled honestly before
someone selects it.

## Resolving a conflict has to actually resolve it

A merged version descends from **both** sides. Without that, the next sync rediscovers the
same conflict and asks the user again, forever. There is a test for exactly this.
