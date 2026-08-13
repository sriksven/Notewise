# Hosted inference

> **Status: scaffold.** BSL 1.1 — see [LICENSE-CLOUD.md](../../LICENSE-CLOUD.md).

STT and LLM endpoints for users without local compute.

Implements the same operations as `core/crates/ai-router`'s trait, so switching a user
between local and hosted changes a config value and nothing else.
