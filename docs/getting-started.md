# Getting started

## Requirements

Rust 1.82 or newer. Nothing else for the engine — SQLite is bundled, so there is no system
database to install.

## Build and verify

```sh
cargo build --workspace
cargo test --workspace
```

## Run the CLI

```sh
cargo run -p notewise-cli -- status
```

`status` reports where your database lives, its schema version, and — importantly — whether
the configured AI backend keeps data on your machine:

```
database       ~/Library/Application Support/notewise/notewise.db
schema version 3
meetings       0
ai backend     llama3.1
ai location    local — transcripts stay on this machine
```

## Choosing an AI backend

The backend is chosen from your environment, and **defaults to local**. Configuring nothing
means nothing leaves your machine.

| Setup | Result |
|---|---|
| Nothing set | Local inference via Ollama at `localhost:11434` |
| `ANTHROPIC_API_KEY` set | Your own Anthropic key (BYOK) |
| `NOTEWISE_BACKEND=mock` | Deterministic stub — no model needed |
| `OLLAMA_HOST` set | Ollama on another host |
| `NOTEWISE_MODEL` set | Override the model name |

To try the workspace with no model installed at all:

```sh
NOTEWISE_BACKEND=mock cargo run -p notewise-cli -- status
```

## Download a transcription model

```sh
./scripts/download-models.sh base.en
```

`base.en` (~148 MB) balances accuracy against a download you will actually wait for. Larger
models are meaningfully better but start at half a gigabyte — see
[`ModelRegistry`](../core/crates/transcription/src/models.rs) for the full list and the RAM
each needs.

## Run the local API

```sh
cargo run -p notewise-cli -- serve
```

Serves on `http://127.0.0.1:47821`. **Loopback only** — the server refuses to bind a
non-loopback address, because it is unauthenticated by design and assumes the trust boundary
is the machine edge. See [api-reference/rest.md](api-reference/rest.md).

## Connect an agent

```sh
cargo run -p notewise-cli -- mcp
```

Speaks MCP over stdio. See [mcp-server.md](mcp-server.md) for client configuration and the
tool list.

## Where things are

| Path | What |
|---|---|
| [`core/crates/`](../core/crates/) | The engine |
| [`apps/cli/`](../apps/cli/) | This CLI |
| [`ARCHITECTURE.md`](../ARCHITECTURE.md) | How the pieces fit and why |
| [`ROADMAP.md`](../ROADMAP.md) | What is built and what is not |
