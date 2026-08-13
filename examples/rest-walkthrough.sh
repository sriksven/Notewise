#!/usr/bin/env bash
# End-to-end walkthrough of the local REST API.
#
# Runs against an in-memory database with the mock AI backend, so it needs no model
# download and leaves nothing behind.
set -euo pipefail

PORT="${PORT:-47899}"
BASE="http://127.0.0.1:$PORT"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug/notewise"

if [ ! -x "$BIN" ]; then
  echo "building the cli first..."
  (cd "$ROOT" && cargo build -q -p notewise-cli)
fi

jsonf() { python3 -m json.tool; }
field() { python3 -c "import sys,json;print(json.load(sys.stdin)$1)"; }

echo "starting the engine on $BASE (in-memory, mock backend)"
NOTEWISE_BACKEND=mock "$BIN" --ephemeral serve --port "$PORT" >/dev/null 2>&1 &
SERVER=$!
trap 'kill $SERVER 2>/dev/null || true' EXIT

# The server binds before it is ready to answer; poll rather than sleeping a fixed time.
for _ in $(seq 1 40); do
  curl -sf "$BASE/health" >/dev/null 2>&1 && break
  sleep 0.25
done

echo
echo "── health ──────────────────────────────────────────"
# ai_local tells a client whether transcripts leave the machine.
curl -s "$BASE/health" | jsonf

echo
echo "── create a meeting ────────────────────────────────"
MEETING=$(curl -s -X POST "$BASE/v1/meetings" \
  -H 'content-type: application/json' \
  -d '{"title":"Infra sync","source":"combined"}')
echo "$MEETING" | jsonf
ID=$(echo "$MEETING" | field "['id']")

echo
echo "── append transcript segments (batched) ────────────"
curl -s -X POST "$BASE/v1/meetings/$ID/transcript" \
  -H 'content-type: application/json' \
  -d '[
        {"text":"We agreed to migrate to Postgres.","start_ms":0,"end_ms":4000,"speaker":"Alex"},
        {"text":"Jordan will draft the plan by Friday.","start_ms":4200,"end_ms":8000,"speaker":"Sam"}
      ]' | jsonf

echo
echo "── summarize (persists + links into the graph) ─────"
curl -s -X POST "$BASE/v1/meetings/$ID/summarize" | jsonf

echo
echo "── graph traversal from the meeting ────────────────"
# The summary is reachable at one hop via a derived_from edge.
curl -s "$BASE/v1/meetings/$ID/related?depth=2" | jsonf

echo
echo "── a note that references the meeting ──────────────"
curl -s -X POST "$BASE/v1/notes" \
  -H 'content-type: application/json' \
  -d "{\"title\":\"Migration follow-up\",
       \"body\":\"Postgres migration owner: Jordan.\",
       \"references_meeting\":\"$ID\"}" | jsonf

echo
echo "── the note is now reachable from the meeting ──────"
curl -s "$BASE/v1/meetings/$ID/related?depth=2" | jsonf

echo
echo "── full-text search ────────────────────────────────"
curl -s "$BASE/v1/search?q=Postgres" | jsonf

echo
echo "── error shape: unknown id is 404 with a code ──────"
curl -s "$BASE/v1/meetings/00000000-0000-4000-8000-000000000000" | jsonf

echo
echo "── error shape: malformed id is 400, not 500 ───────"
curl -s "$BASE/v1/meetings/not-a-uuid" | jsonf

echo
echo "done."
