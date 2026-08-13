# Gathering context as an agent

Notes on using the MCP tools well. The tools are documented in
[../docs/mcp-server.md](../docs/mcp-server.md); this is about which to reach for.

## Prefer traversal over search

Searching for a meeting's context finds things that merely *mention* the same words.
`find_related` finds things actually *connected* to it, and tells you how.

```json
{ "name": "find_related", "arguments": { "kind": "meeting", "id": "...", "depth": 2 } }
```

Each result carries `distance` (hops) and `via` (the edge kind), so you can distinguish:

- `via: "derived_from"` — the summary this meeting produced. Authoritative.
- `via: "references"` — a note someone wrote pointing at it. Related, but someone's opinion.
- `via: "became_ticket"` — work that came out of it.

That distinction is not recoverable from a text search.

## Depth 2 is usually right

Depth 1 gives immediate neighbours — the summary, notes referencing the meeting. Depth 2
reaches one step further: the decisions and action items inside that summary, the tickets
those became. Beyond that, results get loosely relevant fast.

`depth` is clamped rather than rejected, so asking for more is not an error — it is just noise.

## Read the summary before the transcript

`get_summary` returns the model's summary plus extracted decisions and action items.
`get_transcript` returns everything anyone said. For most questions the summary answers it in
a fraction of the tokens.

Reach for the transcript when you need exact wording, who said something, or when the summary
is absent.

## Nulls are answers, not failures

`get_summary` on an unsummarized meeting returns `summary: null` with a note saying so. That
is the state of the world, not an error to retry.

## The surface is read-only

There is no tool to create, edit, or delete anything. If a task needs a change made, report
what should change and let the user act — do not look for a write tool, there isn't one.
