#!/usr/bin/env bash
# Download a Whisper model into the local model store.
#
# The CLI does this on demand; this script exists for prefetching in CI or an installer,
# where waiting for a 148MB download at first use is a poor experience.
set -euo pipefail

MODEL="${1:-base.en}"
BASE_URL="https://huggingface.co/ggerganov/whisper.cpp/resolve/main"

# The same order the engine resolves it in (`notewise_storage::data_dir`, then `models/`):
# NOTEWISE_MODEL_DIR, then NOTEWISE_DATA_DIR, then the platform's data directory. A model
# prefetched anywhere else is a model the engine downloads again — and CI, which this exists
# for, is exactly where NOTEWISE_DATA_DIR tends to be set.
if [ -n "${NOTEWISE_DATA_DIR:-}" ]; then
  DEFAULT_DIR="$NOTEWISE_DATA_DIR/models"
else
  case "$(uname -s)" in
    Darwin) DEFAULT_DIR="$HOME/Library/Application Support/notewise/models" ;;
    Linux)  DEFAULT_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/notewise/models" ;;
    # Git Bash, MSYS and Cygwin on Windows, where the engine uses %APPDATA%.
    *)      DEFAULT_DIR="${APPDATA:-$HOME/AppData/Roaming}/notewise/models" ;;
  esac
fi
DIR="${NOTEWISE_MODEL_DIR:-$DEFAULT_DIR}"

mkdir -p "$DIR"
TARGET="$DIR/ggml-${MODEL}.bin"

if [ -f "$TARGET" ]; then
  echo "already present: $TARGET"
  exit 0
fi

echo "downloading $MODEL to $DIR"

# Download to a temporary path and rename on success. An interrupted download must not
# leave a partial file that looks installed and fails later with an opaque load error.
TEMP="$TARGET.partial"
trap 'rm -f "$TEMP"' EXIT

curl --fail --location --progress-bar --output "$TEMP" "$BASE_URL/ggml-${MODEL}.bin"
mv "$TEMP" "$TARGET"
trap - EXIT

echo "done: $TARGET"
