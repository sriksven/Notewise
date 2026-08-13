# Security Policy

## Reporting a vulnerability

**Do not open a public issue for a security vulnerability.**

Report privately through GitHub's ["Report a vulnerability"](https://github.com/notewise/notewise/security/advisories/new)
flow, which creates a private advisory.

Please include: what the vulnerability allows, steps to reproduce, affected versions and
platforms, and any proof-of-concept you have.

## What to expect

| | |
|---|---|
| Acknowledgement | Within 3 business days |
| Initial assessment | Within 10 business days |
| Fix target | Severity-dependent; critical issues take priority over roadmap work |

We will keep you updated, credit you in the advisory unless you prefer otherwise, and
coordinate disclosure timing with you.

## Scope

This product handles recordings of private conversations. We take the following especially
seriously:

- Anything that exfiltrates meeting audio, transcripts, or notes off-device
- Anything that causes data to reach the cloud when a user has **not** opted into sync
- Weaknesses in encryption at rest for the local database
- Local API or MCP server exposure beyond localhost, or missing authentication on them
- Privilege escalation through audio capture permissions
- Credential leakage for connected integrations (Gmail, Linear, Jira, Slack)
- Anything causing an email to send without explicit user confirmation

## Out of scope

- Vulnerabilities in dependencies with no exploitable path in Notewise (report upstream)
- Attacks requiring physical access to an unlocked machine
- Social engineering
- Missing hardening headers with no demonstrated impact
- Automated scanner output without a working proof of concept

## Design commitments

These are product decisions with security consequences, stated so you can hold us to them:

- **Sync is opt-in.** A default install sends no meeting content anywhere.
- **`api-server` binds to localhost only.**
- **Email is draft-and-confirm.** No path auto-sends without explicit user approval.
- **The local database is encrypted at rest.**

If you find behavior contradicting any of these, treat it as a vulnerability and report it.
