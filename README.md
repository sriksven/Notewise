<div align="center">

# Notewise

**Local-first meeting intelligence and workspace platform.**

Meetings are captured and understood on your machine, then become notes, tickets,
drafted emails, and notifications, all linked in one queryable graph.

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

The connective tissue is an **object graph**: meetings, notes, decisions, tickets, and
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

## Licensing: read this before contributing

This is an **open-core** project, and we would rather be blunt about the split than have
you discover it later:

| Path | License |
|---|---|
| `core/`, `apps/` | **MIT** (see [LICENSE](LICENSE)) |
| `cloud/` | **BSL 1.1**, converts to Apache-2.0 after 4 years (see [LICENSE-CLOUD.md](LICENSE-CLOUD.md)) |

Everything you need to run Notewise entirely on your own machine is MIT. The hosted
multi-tenant services are BSL.

## Status

Early. The repository structure covers the full roadmap, but code lands phase by phase.

| Component | State |
|---|---|
| `storage`, `graph`, `ai-router` | Implemented, tested |
| `api-server`, `mcp-server`, `cli` | Implemented, tested |
| Markdown export | Implemented |
| `recorder` | **Pipeline implemented**: capture → mix → transcribe → diarize → store |
| `transcription` | **Whisper implemented** (`--features whisper-metal`), Parakeet pending |
| `audio-capture` | **Microphone and macOS system audio implemented** (`--features os-capture`); system audio not yet verified in a release build |
| `diarization`, `sync-client`, `ffi` | Interfaces defined |
| `apps/desktop` | **Runs**: React UI served by the engine, in a Tauri window; records from the app |
| `apps/mobile`, `cloud/` | Scaffolded, awaiting their phase |

See `docs/roadmap.md` for what lands when, and each directory's `README.md` for
its specific state. Directories that are scaffolds say so at the top.

## Install

Each tagged version attaches installers to
[GitHub Releases](https://github.com/sriksven/Notewise/releases).

| Platform | File | First launch |
|---|---|---|
| macOS, Apple silicon | `.dmg` | **Open Anyway**, once per download (see below) |
| Windows 10 and 11, 64-bit | `_x64-setup.exe` | Unsigned: SmartScreen warns, choose *More info → Run anyway* |
| Linux | Not released yet | The engine and CLI build from source |

### macOS: the first launch

Notewise is signed with the project's own certificate rather than an Apple Developer ID, so
macOS cannot confirm who made it and stops the first launch:

1. Drag Notewise from the `.dmg` into Applications, and open it from there.
2. macOS says it could not verify that Notewise is free of malware. Choose **Done**.
3. Open **System Settings → Privacy & Security**, scroll down to **Security**, and choose
   **Open Anyway** beside the line about Notewise. It stays there for about an hour after the
   blocked attempt. Confirm with your login password.

You do that once for each download. Permissions you grant (Microphone, Screen & System Audio
Recording, Accessibility, Input Monitoring) carry over to later versions, because every release
is signed with the same certificate. One thing does not: after an update, macOS may ask whether
Notewise can use what it saved in your keychain (connector sign-ins). Choose **Always Allow**.
Without an Apple Developer ID, macOS ties those items to each version.

To check that a copy is the released one:

```sh
codesign -d -r- /Applications/Notewise.app
```

The requirement it prints should end in `H"…"` with the fingerprint pinned as
`MACOS_CERTIFICATE_SHA1` in [`release.yml`](.github/workflows/release.yml).

### Windows: the first launch

The installer is not signed, so Windows cannot say who made it. Your browser may call the
download uncommon; keep it. Run the `_x64-setup.exe`, and if **Windows protected your PC**
appears, choose **More info**, then **Run anyway**. It installs for your account only, with no
administrator prompt.

On Windows, recording is from the microphone only — system audio is not built there yet —
transcription runs on the processor, and the desktop assistant is not available.

## Quick start

```sh
# Requires Rust 1.87+
cargo build --workspace
cargo test --workspace
```

### Record a meeting

Needs a C++ toolchain (cmake + clang) for whisper.cpp. The model downloads on first use.

```sh
cargo run -p notewise-cli --features full -- devices
cargo run -p notewise-cli --features full -- record --seconds 60
cargo run -p notewise-cli --features full -- import meeting.wav
```

On an Apple M4 with `base.en`, transcription runs at **37.6x realtime on Metal**.

### Run the desktop app

```sh
cd apps/desktop && npm install && npm run build && cd -
cargo run --manifest-path apps/desktop/src-tauri/Cargo.toml --features bundle
```

`--features bundle` is what makes the record button real: it compiles in microphone capture and
Metal-accelerated Whisper. Without it the app still browses, searches, summarizes, and exports,
and it says so in About rather than offering a button that does nothing.

The window points at the engine's own loopback port. That is deliberate: serving the UI from the
engine keeps them same-origin, so the unauthenticated local API needs no CORS. With permissive
CORS, any page you visited could read your meetings.

### Recording over HTTP

Capture is server-side, so the desktop app and the CLI see the same recording:

```sh
curl -X POST   127.0.0.1:47821/v1/recording -d '{"title":"Standup"}'
curl           127.0.0.1:47821/v1/recording          # what is running
curl -X DELETE 127.0.0.1:47821/v1/recording          # stop, flush, diarize
```

`GET /health` reports `can_record`, so a client can tell "not recording" from "cannot record".

## Documentation

- [CLAUDE.md](CLAUDE.md): orientation for AI coding agents working in this repo
- [CONTRIBUTING.md](.github/CONTRIBUTING.md): conventions, commit format, review process
- [SECURITY.md](.github/SECURITY.md): vulnerability disclosure
- `docs/`: architecture, roadmap, build walkthroughs, REST reference. Local only and
  not published; anything load-bearing lives in doc comments beside the code.
