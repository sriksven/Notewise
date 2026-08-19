# Tiering and entitlements — design

**Date:** 2026-08-18
**Status:** draft, awaiting review
**Scope:** Spec 12. What is free, what is paid, and how that is enforced without breaking
local-first.

---

## Why this exists

The roadmap names a Pro tier in Phase 2 and a Team tier in Phase 3 and says nothing about what
goes in either. Meanwhile every spec in this program adds capability, and each one arrives with an
implicit question nobody has answered: is this free?

Answering it late is expensive. A feature built without knowing produces an entitlement check
retrofitted into ten call sites, or worse, a capability shipped free that the business later needs
to claw back — which is the one product change users genuinely resent.

## The competitive data point

AnythingLLM's Pro tier is instructive precisely because of how little it contains: quota removal on
three ambient OS-level conveniences, and watermark-free document export. The meeting assistant,
every LLM provider, agents, MCP, scheduled jobs, and self-hosting are all free with no usage
limits.

They charge for *ambient convenience*, not for the core product and not for access to your own
data. That is the right instinct and it is worth copying, because the alternative — metering the
core — is what makes users of a local-first tool feel cheated.

## Goals

- A stated line between free and paid that every future spec can be checked against.
- Enforcement that never prevents local features from working, ever, including offline.
- One place the check lives, so it is not scattered.
- Alignment with the licensing boundary the repo already has.

## Non-goals

- **Pricing.** Numbers are a business decision, not a design one.
- **Payment processing.** `cloud/billing` is a scaffold and stays one.
- **Trials, coupons, or promotional logic.** Later, and not architectural.
- **Per-seat management.** Team tier, Phase 3, separate spec.

---

## Decisions

### T1 — The free/paid line is the MIT/BSL line that already exists

`core/` and `apps/` are MIT. `cloud/` is BSL 1.1. `CLAUDE.md` already treats moving code between
them as a relicensing event requiring deliberate thought.

That boundary is already exactly where the monetization boundary belongs: **anything that runs on
the user's machine is free; anything that runs on our infrastructure is paid.** No new taxonomy is
needed, and the rule is checkable by looking at which directory the code is in.

This is not a coincidence so much as a consequence — the things that cost us money to operate are
the things that run on our servers.

It also settles the specs in this program cleanly:

| Capability | Directory | Tier |
|---|---|---|
| Recording, transcription, diarization, speaker naming | `core/` | Free |
| Summaries, templates, transcript editing, titles (Spec 3) | `core/` | Free |
| Lexical and semantic search, grounded answers, the agent | `core/` | Free |
| Notes, tickets, vault sink, webhook sink | `core/` | Free |
| Calendar and mail connectors (Specs 1) | `core/` | Free |
| Model routing (Spec 2) | `core/` | Free |
| Auto-join detection (Spec 5) | `core/` | Free |
| MCP server and MCP client (Spec 6) | `core/` | Free |
| Scheduled jobs (Spec 7) | `core/` | Free |
| Memory (Spec 8) | `core/` | Free |
| Document import, vault divergence (Spec 10) | `core/` | Free |
| Audio retention (Spec 11) | `core/` | Free |
| Desktop assistant and dictation (Spec 9) | `core/` + shell | Free — see T2 |
| Cloud sync | `cloud/sync-service` | **Pro** |
| Hosted STT and LLM inference | `cloud/hosted-inference` | **Pro** |
| Team workspace, comments, external access | `cloud/` | **Team** |
| SSO, audit logs, admin policy | `cloud/admin-api` | **Enterprise** |

The user's own API key going to Anthropic is free: that call costs us nothing, and metering someone
else's key would be indefensible.

### T2 — No metered quotas on local compute

AnythingLLM meters its Magic features by daily usage. Notewise does not, for one reason: their
transcription and completion can route through their infrastructure, and ours cannot — Spec 9's
dictation runs on the user's own CPU with a model on the user's own disk.

Charging per use of someone's own hardware is a position that cannot be defended in a support
thread. If Spec 9 ships, it ships free.

**Rejected — quota-limiting local features to drive conversions.** It is the obvious revenue lever
and it directly contradicts the product's central claim. A local-first tool that counts your
transcriptions is not local-first in any sense the user cares about.

### T3 — Entitlement checks gate cloud calls only, and never local ones

The check sits at the boundary where a request would leave the machine: `sync-client` before it
syncs, the hosted-inference backend before it calls. Nowhere else.

No local feature has an entitlement check anywhere in its path. Not disabled-when-unlicensed, not
degraded, not even read. This is a structural rule, not a policy — a local feature that *reads*
entitlement state is one refactor away from depending on it.

Consequence: an expired subscription means sync stops and hosted inference stops. Recording,
transcription, summaries, search, and every connector keep working forever. The user's data stays
fully usable and fully exportable via the vault sink.

### T4 — Entitlement is a cached grant with a long offline grace, and it fails open

The client holds a signed entitlement with an expiry, refreshed opportunistically when online. If
refresh fails, the cached grant continues to be honoured for a long grace period.

A local-first product must work on a plane, in a basement, and during our outage. An entitlement
system that fails closed turns our infrastructure problem into the user's lost afternoon —
including for the paid features they are current on, which is the worst possible failure.

Fail-open has an obvious abuse story: someone stays offline to keep using Pro past expiry. That is
an acceptable loss. The alternative punishes every honest user for the behaviour of a few, and the
features at stake require our servers anyway — sync with nothing to sync to is not much of a theft.

### T5 — Free is a product, not a funnel

The roadmap already commits to this: "the free local product is genuinely useful with nothing
connected." T1 makes that structural rather than aspirational.

Stated as a rule for future specs: **a free feature is never degraded to create demand for a paid
one.** If a paid tier needs a reason to exist, it earns it by doing something the local machine
cannot — syncing across devices, running a model the laptop cannot hold, sharing with a team.

### T6 — Entitlement state lives in one small module, and its shape is boring

A single module in `sync-client` owns fetching, caching, verifying, and answering. It exposes one
question — is this capability granted — over a closed enum of paid capabilities.

An enum rather than strings so a typo is a compile error, and a closed set so the list of things
that can be gated is auditable in one place. If a capability is not in the enum, it cannot be
gated, which is the property T3 depends on.

---

## Architecture

```
  local features ────────────────────────────────► no check, ever  (T3)

  sync-client ──► Entitlements::granted(Sync)? ──► sync or don't
  hosted backend ─► Entitlements::granted(HostedInference)? ──► call or refuse

  Entitlements
    ├─ cached signed grant + expiry (on disk)
    ├─ opportunistic refresh when online
    └─ long offline grace; fails open  (T4)
```

| Location | Contents | New? |
|---|---|---|
| `core/crates/sync-client/src/entitlements.rs` | Grant cache, verification, `granted()` | new |
| `cloud/admin-api` or a small endpoint | Issues signed grants | new (BSL) |
| `apps/desktop/src/views/SettingsView.tsx` | Plan state, what is and is not active | edit |

`sync-client` is already the crate that owns talking to our infrastructure, so entitlement lives
there rather than in a new crate. Nothing in `storage`, `graph`, `ai-router`, `connectors`,
`recorder`, `transcription`, or `diarization` imports it — and that absence is worth a test.

```rust
pub enum PaidCapability {
    Sync,
    HostedInference,
    TeamWorkspace,
}

impl Entitlements {
    pub fn granted(&self, capability: PaidCapability) -> bool;
    pub fn state(&self) -> EntitlementState; // Active | Grace{until} | Expired | Unlicensed
}
```

`state()` exists so the UI can be honest about grace rather than showing "Active" until the moment
everything stops.

## Error handling

| Condition | Handling |
|---|---|
| Refresh fails, cached grant valid | Honour the cache. Not an error |
| Refresh fails, inside grace | Honour it; UI shows grace and when it ends |
| Grace expired | Paid capabilities refuse with a clear reason; local unaffected |
| Grant signature invalid | Treat as unlicensed; never as active |
| Clock moved backwards | Use the later of cached-issue-time and system time so a clock change cannot extend grace indefinitely |
| No grant at all | Unlicensed. The free product works |

Invalid signature failing closed is the one place T4's fail-open does not apply: a forged grant is
not an outage.

## Testing

- `granted()` for each capability across Active, Grace, Expired, and Unlicensed.
- Refresh failure inside grace honours the cache; past grace refuses.
- Invalid signature is unlicensed regardless of expiry.
- Backwards clock cannot extend grace.
- **Structural test:** no crate other than `sync-client` references `Entitlements` or
  `PaidCapability`. This is the T3 guarantee and it is the most important test here, because it is
  the one that decays silently as the codebase grows.
- A fully unlicensed build exercises recording, transcription, summarising, searching, and a vault
  push end to end, proving the free product does not depend on entitlement state.

No network needed; grants are constructed in tests.

## What this delivers

1. A stated free/paid line that coincides with the existing MIT/BSL boundary.
2. A closed enum of paid capabilities, gated only at the network boundary.
3. Cached grants with long offline grace, failing open on our outages and closed on forgery.
4. Honest plan state in the UI, including grace.
5. A structural test that keeps local features free by construction.

## Risks and open questions

- **The paid tier may be too thin.** Sync and hosted inference are the whole offer, and both are
  things a technical user can route around — which is the cost of T1 and T5 being honest.
- **Fail-open is exploitable** by staying offline. Accepted in T4.
- **Grant issuance is unspecified** here; it lives in `cloud/` under BSL and needs its own design
  alongside billing.
- **Team tier is named but not designed.** Phase 3, and the multi-user model it needs does not
  exist yet.
- **Spec 9 being free** removes the revenue argument for building it, which sharpens rather than
  resolves the opportunity-cost question that spec already raises about itself.
