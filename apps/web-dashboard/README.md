# Web dashboard

> **Status: scaffold.** No implementation yet — this directory exists so the architecture does not need reshaping when Phase 3 arrives. See [ROADMAP.md](../../ROADMAP.md).

Team workspace, admin, onboarding, and billing UI. **Cloud-tier only** — it talks to
[`cloud/`](../../cloud/) services, never directly to a user's local engine.

Next.js. Note the licensing boundary: this app serves the hosted product, so treat anything
it depends on from `cloud/` as BSL, not MIT.
