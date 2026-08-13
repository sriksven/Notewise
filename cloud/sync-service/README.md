# Sync service

> **Status: scaffold.** BSL 1.1 — see [LICENSE-CLOUD.md](../../LICENSE-CLOUD.md).

Accepts opt-in sync from clients and resolves conflicts.

The hard part is already built and tested: `core/crates/sync-client` implements version
vectors and conflict resolution. This service is the transport around it. Reuse that logic
rather than reimplementing merge rules server-side — two implementations of a merge rule
will diverge, and the symptom is a user losing an edit.
