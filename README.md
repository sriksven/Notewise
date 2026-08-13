<div align="center">

# Notewise

**Local-first meeting intelligence and workspace platform.**

Meetings are captured and understood on your machine, then become notes, tickets,
drafted emails, and notifications — all linked in one queryable graph.

</div>

---

## What this is

Most meeting tools record you, summarize you, and hand back a transcript. Notewise treats
the meeting as the *start* of the work, not the end of it:

```
Meeting happens → transcribed & understood → becomes:
  ├── a Notes page      (searchable, linkable, editable)
  ├── Tickets           (native, or pushed to your tracker)
  ├── Draft emails      (follow-ups, addressed to real attendees)
  └── Notifications     (when something actually needs attention)
```

The connective tissue is an **object graph** — meetings, notes, decisions, tickets, and
emails reference each other as typed edges, so "show me everything related to this
meeting" is one traversal rather than a hand-written join per feature.

## Local or cloud, your choice

This is enforced architecturally, not promised in marketing copy. Every feature that
touches a model calls the `ai-router` trait. Nothing calls a model provider directly.
Swapping local Ollama for your own API key for hosted inference is a config change.

| Backend | Runs | Needs |
|---|---|---|
| Ollama | Your machine | A running daemon |
| LM Studio / Unsloth | Your machine | A running server |
| Mock | In-process | Nothing |
| Anthropic | Cloud | `ANTHROPIC_API_KEY` |
| Google Gemini | Cloud | `GEMINI_API_KEY` |
| Groq | Cloud | `GROQ_API_KEY` |
| OpenRouter | Cloud | `OPENROUTER_API_KEY` |
| Any OpenAI-compatible endpoint | Wherever you point it | `NOTEWISE_ENDPOINT` |

Selection defaults to **local**, and `notewise status` tells you which is active and whether
transcripts leave the machine.

Sync is **off** unless you turn it on. `sync-client` is a separate crate specifically so a
local-only build never compiles it in.

## Licensing — read this before contributing

This is an **open-core** project, and we would rather be blunt about the split than have
you discover it later:

| Path | License |
|---|---|
| `core/`, `apps/` | **MIT** — see [LICENSE](LICENSE) |
| `cloud/` | **BSL 1.1**, converts to Apache-2.0 after 4 years — see [LICENSE-CLOUD.md](LICENSE-CLOUD.md) |

Everything you need to run Notewise entirely on your own machine is MIT. The hosted
multi-tenant services are BSL.

## Status

Early. The repository structure covers the full roadmap, but code lands phase by phase.

| Component | State |
|---|---|
| `storage`, `graph`, `ai-router` | Implemented, tested |
| `api-server`, `mcp-server`, `cli` | Implemented, tested |
| Markdown export | Implemented |
| `audio-capture` (mixing, conversion), `transcription` | Interfaces + partial implementation |
| `diarization`, `sync-client`, `ffi` | Interfaces defined |
| `apps/` (except CLI), `cloud/` | Scaffolded, awaiting their phase |

See [ROADMAP.md](ROADMAP.md) for what lands when, and each directory's `README.md` for
its specific state. Directories that are scaffolds say so at the top.

## Quick start

```sh
# Requires Rust 1.82+
cargo build --workspace
cargo test --workspace

# Run the CLI
cargo run -p notewise-cli -- --help
```

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md) — how the pieces fit and why the boundaries sit where they do
- [ROADMAP.md](ROADMAP.md) — phased build order
- [CLAUDE.md](CLAUDE.md) — orientation for AI coding agents working in this repo
- [docs/](docs/) — build instructions, API reference, MCP tool list
- [CONTRIBUTING.md](CONTRIBUTING.md) — conventions, commit format, review process
- [SECURITY.md](SECURITY.md) — vulnerability disclosure
