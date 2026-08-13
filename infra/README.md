# Infrastructure

> **Status: scaffold.** Phase 4 — see [ROADMAP.md](../ROADMAP.md).

Deployment tooling for `cloud/` and for the self-hosted enterprise option.

| Path | Purpose |
|---|---|
| `docker/` | Container images for cloud services |
| `k8s/` | Manifests / Helm charts |
| `terraform/` | Cloud resource definitions |

Self-hosted deployment is an enterprise requirement, so these must produce something a
customer can run in their own environment — not only something that runs in ours.
