# Changelog

What changed, in terms of what it does for the person using Notewise rather than the commit that
changed it. Newest first.

The in-app version of this lives under Help → What's new and is deliberately shorter. If you edit
one, edit the other — `apps/desktop/src/views/HelpView.tsx`.

## 0.1.0 — 2026-08-21

The first tagged build.

### Meetings

- Record from a microphone, or import an audio file with a file picker.
- Transcription runs on this machine, via Whisper or NVIDIA Parakeet. Models are downloaded from
  inside the app, with resumable progress.
- Speakers can be separated by voice for recordings where nothing knows who was talking, and named
  once so the name sticks. In a call, the browser extension reads who is speaking from the meeting
  platform, which is exact — and sends no audio to do it.
- Transcripts are editable. A word the transcriber got wrong can be corrected in place, and a
  meeting can be renamed.
- Audio can be kept after transcription — off by default — which is what makes click-to-play on a
  transcript line possible.

### What a meeting means

- Summaries, with named templates you can write yourself. Running another template adds a summary
  rather than replacing the one you have.
- Decisions, action items and tickets, all of which outlive the summary that proposed them and can
  be added or removed by hand.
- Clarifying questions during a live meeting, when there is still time to ask them.
- What a recurring meeting is still carrying from last time.
- Search across everything said, and a question you can ask the whole workspace that answers only
  from your own material and cites it.
- An agent that reads across your meetings, notes and tickets and writes up what it finds. It only
  ever creates a note.

### Getting work out

- Notes, with a block editor, attached to meetings.
- Follow-up emails drafted with tone variants, approved by you, and delivered as a *draft* in Gmail
  or Outlook — never sent on your behalf.
- Meetings mirrored to a Markdown vault folder, and a folder watched for documents. A file you
  edited outside Notewise is never silently overwritten.
- A signed outbound webhook, for anything else.
- Markdown export, and `.eml` files any mail client will open.

### Around the edges

- Calendar connected to Google or Microsoft, which is what makes auto-join detection possible — and
  it always asks before recording.
- Scheduled jobs. They may propose an external tool call and never execute one.
- External tools over MCP, default-deny: nothing runs without you confirming that specific call.
- A desktop assistant on macOS — dictation into whatever has focus, a panel that can act on
  highlighted text, and inline suggestions. Every one of those needs a permission you grant.
- Memory: capped, listed in full, editable, and never a fact about somebody else.
- Model routing rules, so the cheap local model answers the cheap questions.
- Secrets in the OS keychain, and redaction before anything leaves the machine.
- A theme with eleven accents, keyboard shortcuts, and an address bar that survives a reload.

### Known limits of this build

- **Nothing is signed.** macOS will say the app "is damaged"; it is not, that is the message for any
  unsigned app. Windows will show a SmartScreen warning. The release notes say how to clear the
  macOS quarantine. Linux `.deb` and `.AppImage` need no signing and are the only fully
  first-class artifacts.
- **macOS system audio capture is not included.** It needs ScreenCaptureKit against a signed
  bundle, which is the same missing Apple Developer ID. Microphone capture works.
- **Transcription needs a model download** on first use. The default build stays small on purpose.
- **The desktop assistant needs permissions macOS cannot grant on your behalf** — Accessibility,
  and Input Monitoring for typing pauses. Without them the app says which pane to open rather than
  failing quietly.
- **Hosted anything is opt-in.** With no API key configured, nothing leaves the machine.
