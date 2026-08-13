# Integrations

> **Status: scaffold.** BSL 1.1 — see [LICENSE-CLOUD.md](../../LICENSE-CLOUD.md).

Outbound connections to third-party services. Each needs an OAuth app and credentials.

- `pm/` — Linear, Jira, Asana, GitHub Issues. **One-way push first.**
- `comms/` — Gmail, Outlook, Slack. Email is **draft-and-confirm**, never auto-send.
- `crm/` — Salesforce, HubSpot.
