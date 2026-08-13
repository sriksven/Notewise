# Cloud services

> **Status: scaffold.** Nothing here is implemented. These services begin in Phase 2 — see
> `docs/roadmap.md`.

> ⚠️ **Licensed under [BSL 1.1](../LICENSE-CLOUD.md), not MIT.** Moving code from here into
> `core/` or `apps/` relicenses it. Do not do that incidentally.

Everything in this directory is **opt-in**. A default Notewise install talks to none of it,
and that has to stay true — it is the product's central claim.

| Service | Purpose | Phase |
|---|---|---|
| `sync-service/` | Accepts opt-in sync; resolves conflicts using `core/crates/sync-client` | 2 |
| `hosted-inference/` | STT/LLM for users without local compute | 2 |
| `integrations/pm/` | Linear, Jira, Asana, GitHub Issues | 2 |
| `integrations/comms/` | Gmail, Outlook, Slack | 2 |
| `integrations/crm/` | Salesforce, HubSpot | 3 |
| `notification-service/` | Push, Slack, email digests | 3 |
| `bot-service/` | Calendar-triggered bot recording | 4 |
| `billing/` | Subscriptions, usage metering | 4 |
| `admin-api/` | Team/org management, SSO, audit logs | 4 |

## Two things already decided

**Ticket sync is one-way first.** Two-way sync with an external tracker is a known-hard
conflict problem. Phase 2 pushes only; two-way waits until that is stable.

**Email is draft-and-confirm, permanently.** No path auto-sends without explicit user
approval — the `EmailDraft` state machine in `core/crates/storage` enforces `Draft → Approved
→ Sent` and has no method that skips a step. A single wrong auto-send is a reputational
disaster for a user, so this is not a setting to be relaxed later.

## Why these are empty

Each needs credentials that do not exist in a build environment — Stripe for billing, an IdP
for SSO, OAuth apps for Gmail/Linear/Jira. The interfaces they consume (`sync-client`'s
conflict resolution, `storage`'s repositories) are implemented and tested in `core/`.
