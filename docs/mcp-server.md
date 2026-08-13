# MCP server

Exposes your workspace to agents (Claude Code, Cursor, and other MCP clients) over JSON-RPC
2.0 on stdio.

## Every tool is read-only

An agent can search and traverse your workspace. It cannot create, edit, or delete anything.

This is deliberate. Write access for an unattended agent is a much larger trust decision than
read access, and it should not arrive as a side effect of connecting an MCP client. A test in
the crate asserts the surface stays read-only, so adding a mutating tool has to be a
deliberate change rather than an accident.

## Configuration

```json
{
  "mcpServers": {
    "notewise": {
      "command": "notewise",
      "args": ["mcp"]
    }
  }
}
```

Point `command` at the built binary (`target/debug/notewise`) if it is not on your `PATH`.
Add `--ephemeral` to run against a throwaway in-memory database.

## Tools

| Tool | Purpose |
|---|---|
| `list_meetings` | Recent meetings: id, title, start time, whether still recording |
| `get_transcript` | One meeting's full transcript, speaker-prefixed where known |
| `get_summary` | Latest summary plus its decisions and action items |
| `search` | Full-text search across notes, tickets, and transcripts |
| `find_related` | Everything connected to an entity, by graph traversal |
| `list_notes` | Recent notes |

### `find_related` is the one worth knowing

The others are ordinary lookups. `find_related` walks the object graph, so an agent can gather
the context around a meeting — the notes referencing it, the summary derived from it, the
tickets that came out of it — in one call instead of searching blindly.

```json
{
  "name": "find_related",
  "arguments": { "kind": "meeting", "id": "5f2ade65-...", "depth": 2 }
}
```

Returns each connected node with its `distance` in hops and the `via` edge kind, so an agent
can tell a summary derived from the meeting apart from a note that merely mentions it.

Valid `kind` values: `workspace`, `project`, `meeting`, `transcript_segment`, `summary`,
`decision`, `action_item`, `note`, `ticket`, `email_draft`, `notification`.

## Behaviour worth relying on

- **Unsummarized meetings are not an error.** `get_summary` returns `summary: null` with a
  note saying so, which is more useful to an agent than an error it has to interpret.
- **Bad ids are `invalid_params` (-32602), not internal errors.** An agent given that code
  can correct itself rather than retrying.
- **`depth` is clamped, not rejected.** An over-eager agent asking for depth 99 gets the
  maximum rather than a failure.
- **Search input is treated as a literal phrase.** Punctuation cannot produce a syntax error.

## Verifying it works

```sh
printf '{"jsonrpc":"2.0","id":1,"method":"tools/list"}\n' | notewise mcp
```
