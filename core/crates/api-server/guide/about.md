Guide sections for the About page: what Notewise is, where data lives, updates, and what works.

## About Notewise
page: about
keywords: about, version, build, release, engine, schema, notewise, local first, licence, license, mit, open source, info, information
live: about

Notewise is local-first meeting intelligence. It records or imports meetings, transcribes them on this machine, and turns them into summaries, decisions, action items and notes that you can ask questions of.

About, at the bottom of the sidebar, shows the version and a short panel:

- Engine: the engine version and its database schema version.
- AI backend: the model that answers.
- Recording: whether this build can capture from the microphone.
- Transcripts: processed on this machine, or sent to the configured provider.

The engine and apps are MIT licensed. Hosted cloud services are under a separate source-available licence. Everything needed to run Notewise entirely on your own machine is MIT.

## Where your data lives
page: help/docs
keywords: data, privacy, private, storage, stored, store, database, sqlite, location, offline, upload, uploaded, secure, security, keychain, confidential

Everything is kept in a SQLite database file on this machine. Transcription always runs locally. Whether summaries and answers stay local depends on the AI backend you picked under Settings, then Models: a local one keeps them here, and a hosted one sends the transcript to that provider. The Local or Cloud badge in the top bar, and the About page, show which is active.

Audio is deleted once it is transcribed, unless you choose to keep it under Settings, then Recording. Search by meaning, if you turn it on, also runs locally.

API keys are kept in your operating system's keychain and are never shown again. A key pasted in Settings goes only to the engine on this machine, and from there only to its provider.

## About: Updates
page: about
keywords: update, updates, updating, upgrade, upgrading, newer, latest, auto, automatic, check, install, migrate, migration, download

Nothing in Notewise checks for updates and nothing installs them. Upgrading means downloading a newer release yourself, or pulling and rebuilding if you build from source.

The database migrates itself forward when a newer build opens it, and refuses to open a file written by a newer build than the one running.

Every release is signed with the same certificate, so the macOS permissions you granted carry over to an update.

## About: What works and what does not
page: about
keywords: features, feature, capabilities, supported, limitations, limits, missing, roadmap, works, working, sync, gpu, apple silicon, mcp, coming

About lists what works today:

- Microphone recording, on a build that includes capture.
- Local transcription with Whisper, GPU-accelerated on Apple silicon.
- Speaker separation, inferred from pauses rather than voices. Separating by voice is a separate opt-in under Settings, then Speakers.
- Summaries, decisions and action items, Markdown export, and full-text search across the workspace.
- Notes that attach to meetings and can be asked questions, and an agent that researches across the workspace and writes a note.
- Read-only access for other AI agents over MCP.
- Recoverable delete for notes and meetings, through the Trash.
- Search by meaning, opt-in and local through Ollama.

Not there yet: system audio capture (the other side of a call, macOS only, with the Screen & System Audio Recording permission) is not yet verified in a release, and cloud sync is an opt-in later phase.
