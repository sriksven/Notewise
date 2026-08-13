# Core engine

See [ARCHITECTURE.md](../../ARCHITECTURE.md) for the overview. This page covers what a
contributor needs before changing engine code.

## The dependency rule

Dependencies point **downward only**.

```
surfaces:  api-server   mcp-server   cli   ffi
                  \         |        /     /
                   +--------+-------+-----+
                            |
                          graph          ai-router
                            |
                         storage
```

- `graph` may depend on `storage`. `storage` must **never** depend on `graph`.
- Surfaces may depend on anything beneath them, **never on each other**.
- Nothing above `ai-router` may name a model provider type.

The payoff is concrete: a local-only build can exclude `sync-client` from compilation
entirely, and swapping a transcription engine does not ripple upward.

## Where the invariants live

| Rule | Enforced by |
|---|---|
| SQL only in `storage` | `Database::conn` is `pub(crate)` |
| No provider calls outside `ai-router` | Review + the `AiBackend` trait being the only entry point |
| Cascade deletes work | `PRAGMA foreign_keys=ON`, set per connection |
| Email cannot auto-send | `EmailDraft` state machine has no method skipping `Approved` |
| API stays on loopback | `Server::bind` returns `NotLoopback` |
| MCP surface stays read-only | A test asserting no tool name starts with a mutating verb |

## Ownership versus association

The distinction that shapes the schema:

- **Ownership is a foreign key.** A transcript segment belongs to exactly one meeting.
  Deleting the meeting cascades.
- **Association is a graph edge.** A note references a meeting. Many-to-many, heterogeneous,
  traversable to arbitrary depth.

Getting this wrong means a schema migration every time the product grows a link type. If you
are about to add a nullable foreign key to express "may reference", add an edge instead.

## The edge table has no foreign keys

It cannot: it references heterogeneous kinds, so SQLite cannot cascade for it. **Deleting an
entity therefore requires calling `Graph::detach`** — the entity tables cascade, the edges do
not. This is the easiest thing to forget when adding a new entity type.

## Testing

`storage` and `graph` test against in-memory SQLite, so tests need no fixtures and run in
milliseconds. `ai-router` tests use `MockBackend`. Capture tests use synthetic buffers.

Anything needing a GPU, a signed bundle, a TCC grant, or a paid API key is `#[ignore]`d
**with a reason string** — a green CI run must never imply unverified things work.
