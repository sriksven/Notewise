# Examples

Runnable examples. Each works against a real engine with no model installed.

| File | What it shows |
|---|---|
| `rest-walkthrough.sh` | The full loop: meeting → transcript → summary → graph → search |
| `mcp-session.jsonl` | An MCP session as a client would send it |
| `agent-context.md` | How an agent should gather context (and what to avoid) |

## Run the REST walkthrough

```sh
./examples/rest-walkthrough.sh
```

Starts a server on a throwaway in-memory database with the mock AI backend, drives every
endpoint, and shuts down. Needs `curl`, `python3`, and a built binary.
