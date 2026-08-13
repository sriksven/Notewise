# Shared contracts

Cross-cutting definitions, not runtime code.

| Path | Purpose | State |
|---|---|---|
| `proto/` | Wire-format definitions shared by the engine and cloud services | Scaffold (Phase 2) |
| `types/` | TypeScript and Rust types generated from `proto/` | Scaffold (Phase 2) |
| `design-tokens/` | Colors, spacing, and typography shared across desktop, web, and mobile | Scaffold (Phase 1) |

Generated code belongs in `types/`; nothing here is hand-edited once generation exists.
