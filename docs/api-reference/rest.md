# REST API

Served by `notewise serve` on `http://127.0.0.1:47821`.

## Loopback only

The server **refuses to bind a non-loopback address**:

```
$ notewise serve --port 8080   # after editing the bind address to 0.0.0.0
refusing to bind 0.0.0.0:8080: the API server serves unauthenticated access to the
user's meetings and must stay on loopback
```

The API is unauthenticated by design — it assumes the trust boundary is the machine edge.
Binding it to `0.0.0.0` would publish your meetings to your whole network, so the check is
in the type system rather than in documentation.

## Endpoints

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/health` | Status, schema version, and whether the AI backend is local |
| `GET` | `/v1/meetings?limit=` | Recent meetings |
| `POST` | `/v1/meetings` | Create a meeting |
| `GET` | `/v1/meetings/:id` | One meeting |
| `POST` | `/v1/meetings/:id/end` | Mark a meeting finished |
| `GET` | `/v1/meetings/:id/transcript` | Transcript segments, chronological |
| `POST` | `/v1/meetings/:id/transcript` | Append segments (batched) |
| `POST` | `/v1/meetings/:id/summarize` | Summarize, persist, and link in the graph |
| `GET` | `/v1/meetings/:id/related?depth=` | Graph traversal from the meeting |
| `GET` | `/v1/meetings/:id/export?variant=` | Markdown export (`full`, `brief`, `transcript`) |
| `GET` | `/v1/notes?limit=` | Recent notes |
| `POST` | `/v1/notes` | Create a note |
| `GET` | `/v1/tickets` | Open tickets |
| `GET` | `/v1/search?q=&limit=` | Full-text search |

## `/health` tells you where your data goes

```json
{ "status": "ok", "schema_version": 3, "ai_local": true, "ai_model": "llama3.1" }
```

`ai_local` is surfaced so a client can show the user whether transcripts are leaving the
machine, rather than asking them to trust a settings screen.

## Appending transcript segments

Batched, because transcription emits segments in bursts and one request per segment would
dominate the recording path:

```sh
curl -X POST http://127.0.0.1:47821/v1/meetings/$ID/transcript \
  -H 'content-type: application/json' \
  -d '[{"text":"We agreed to ship Friday.","start_ms":0,"end_ms":4000,"speaker":"Alex"}]'
```

A segment whose `end_ms` precedes its `start_ms` is rejected — that is corrupt timing, and
storing it would misalign everything after it.

## Errors

Every error carries a stable `code` alongside a human-readable `error`. Branch on `code`.

```json
{ "error": "Meeting not found: 5f2ade65-...", "code": "not_found" }
```

| Status | `code` | Meaning |
|---|---|---|
| 400 | `bad_request` | Malformed id, bad segment timing, missing field |
| 400 | `depth_too_large` | Traversal depth above the maximum |
| 404 | `not_found` | No such record |
| 422 | `invalid_state` | Stored data failed validation |
| 422 | `model_refused` | The model declined — **not a server fault** |
| 424 | `ai_not_configured` | No API key for the selected backend |
| 429 | `rate_limited` | Upstream model rate limit |
| 502 | `ai_backend_error` | The local engine is fine; the model provider is not |
| 503 | `schema_too_new` | Database written by a newer Notewise |

The 422-vs-502 distinction matters: a model declining to answer is a normal outcome and the
engine did its job, so it is not reported as a server error.
