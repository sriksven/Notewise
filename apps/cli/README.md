# CLI

> **Status: implemented.** The engine, linked directly — no desktop app or running server
> required, which is what makes scripting and headless use possible.

```sh
cargo run -p notewise-cli -- status
```

## Commands

| Command | Purpose |
|---|---|
| `status` | Database, schema version, and whether the AI backend is local |
| `meetings [--limit]` | Recent meetings |
| `transcript <id>` | Print a transcript |
| `summarize <id>` | Summarize, persist, and link into the graph |
| `related <id> [--depth]` | Everything connected to a meeting |
| `search <query> [--limit]` | Full-text search |
| `notes [--limit]` | Recent notes |
| `serve [--port]` | Local REST API (loopback only) |
| `mcp` | MCP server on stdio, for agents |

Global: `--db <path>` to pick a database, `--ephemeral` for a throwaway in-memory one.

## Environment

| Variable | Effect |
|---|---|
| `ANTHROPIC_API_KEY` | Use your own key (BYOK) instead of local inference |
| `NOTEWISE_BACKEND=mock` | Deterministic stub — no model needed |
| `NOTEWISE_MODEL` | Override the model name |
| `OLLAMA_HOST` | Point at Ollama on another host |
| `NOTEWISE_DATA_DIR` | Override the data directory |
| `NOTEWISE_LOG` | Log filter, e.g. `debug` |

With nothing set, the backend is **local**.

## One detail if you are scripting `mcp`

Logs go to **stderr**, always. Stdout carries the JSON-RPC stream and nothing else — a stray
line there corrupts the protocol.
