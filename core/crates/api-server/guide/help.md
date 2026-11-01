Guide sections for Help: the Documentation topics, Shortcuts and Get support. "What's new" is left out on purpose. "Where your data lives" is in about.md and "Deleting things" is covered by the Trash section in pages.md.

## Help: Recording a meeting
page: help/docs
keywords: record, recording, how to record, start, capture, delay, lag, slow, behind, stalled, stuck, late, words

Press record on the Record page, or press ⌘⇧R. The engine opens the microphone and creates the meeting in the same step, so reloading the window never leaves a recording running with nothing attached to it.

Transcription runs in short windows rather than word by word, so text lands a few seconds behind the audio. That is normal and not a sign it has stalled.

Help, in the sidebar, opens on Documentation, which covers this along with recording the other side of a call, choosing a transcription model, where your data lives, asking questions, and deleting things.

## Help: Recording the other side of a call
page: help/docs
keywords: system audio, other side, other people, remote, call, calls, zoom, teams, meet, screen recording, permission, sequoia, missing, half, only me

The microphone hears you and whoever is in the room. It does not hear the other people on a call, because their audio goes to your speakers, not to the microphone.

Capturing it needs the macOS permission called Screen & System Audio Recording, because macOS files system audio under screen recording. Enable System audio under Settings, then Recording, and macOS points you to System Settings, Privacy & Security, where Notewise has a switch. Turn it on, then quit and reopen Notewise: macOS only applies it at launch. Nothing from the screen is kept.

Every release is signed with the same certificate, so the permission carries over to updates. macOS still asks from time to time whether Notewise should keep it. That is a check macOS Sequoia added for every app that records the screen, not something Notewise does.

## Help: Choosing a transcription model
page: help/docs
keywords: whisper, accuracy, accurate, poor, bad, quality, wrong words, mishear, misheard, accent, english, multilingual, turbo, large, which model

Larger Whisper models hear better, especially through a bad connection or an accent the small ones were not trained for. Help recommends large-v3-turbo: it is close to the largest model in accuracy at a fraction of the time.

The .en models are English-only and slightly better at it. For anything else, pick a multilingual one.

Models are downloaded under Settings, then Transcription, and stored beside the database.

## Help: Asking questions
page: help/docs
keywords: ask, question, questions, citation, citations, sources, wrong answer, irrelevant, not found, nothing matches, refused, cannot find

Ask is in the sidebar, and the Ask button at the bottom right of most screens opens the same thing beside whatever you are looking at. Every meeting has an Ask tab, every note has its own, and the Agent searches across all of it. Answers cite what they were drawn from, and clicking a citation opens it.

By default Ask matches words: asking about "pricing" will not find a meeting that only ever said "cost structure". Building the search index under Settings, then Search, fixes that. It runs locally through Ollama, never through your chat provider.

Even then, a small workspace can surface the nearest thing rather than nothing. Read the citations: an answer citing something irrelevant is telling you it did not find what you asked for.

## Help: Keyboard shortcuts
page: help/shortcuts
keywords: shortcut, shortcuts, keyboard, keys, key, keystroke, hotkey, hotkeys, cmd, command, ctrl, control, bold, italic, strikethrough, inline code, enter, escape, esc

These are listed under Help, then Shortcuts. Ctrl works in place of ⌘.

- ⌘K: Jump to the search box
- ⌘N: New note
- ⌘⇧R: Start or stop recording
- ⌘B: Bold the selection
- ⌘I: Italic
- ⌘E: Inline code
- ⌘⇧X: Strikethrough
- /: Block menu, on an empty line
- Enter: Send a question
- ⇧Enter: New line in a question
- Esc: Close an open menu

The formatting keys act on the selection inside a note. Recording is on ⌘⇧R rather than ⌘R because ⌘R reloads the window. Dictation and the assistant panel have their own system-wide hotkeys, set under Settings, then Assistant.

## Help: Getting support
page: help/support
keywords: support, bug, bugs, report, issue, broken, not working, problem, contact, feedback, troubleshoot, crash, help

If something is not working, start with About. It reports what this build can actually do: whether the engine is reachable, whether it can record, which model is loaded and whether it runs locally. Most problems turn out to be one of those saying no.

To report a bug, include what About shows, what you did, and what happened instead. If it involves a transcript, the model name matters more than anything else. Do not paste a transcript into a public issue: it is a recording of people who did not agree to that.

Help, then Get support, has this and a short list of common answers.

## Help: Common problems
page: help/support
keywords: nothing happens, does nothing, greyed, disabled, no devices, no input, summaries fail, failed, error, errors, poor transcript, troubleshooting, fix, broken

From Help, then Get support:

- The record button does nothing: this build was made without capture, or the engine is running on a temporary in-memory database. About says which.
- No input devices are listed: macOS does not reveal them until microphone access is granted. Grant it under Settings, then Recording, and reopen the picker.
- Summaries fail: a local backend such as Ollama needs to be running, and a hosted one needs an API key. Settings, then Models, reports both.
- The transcript is poor: almost always the model. Help suggests downloading large-v3-turbo under Settings, then Transcription, and trying the same audio again.
