# Asking out loud

**Status:** built on `feat/voice-ask`.

## What it is

A mic button beside the send button in every Ask box: the Ask panel at the bottom right, a
note's Ask, and the Ask page. All three render `AskThread`, so one change covers them.

Press the mic and talk. The words appear in the box as they are said, in grey italics, because
they are a guess the next phrase may revise. Press the mic again and the grey words become the
final transcript, in ordinary text, after anything already typed. Nothing is sent until the
person sends it: Whisper mishears names, and a misheard question costs a whole answer. Escape
throws the recording away. Closing the panel or switching notes mid-recording does the same and
closes the microphone.

## Why the engine's dictation, and not the browser

The engine already has local dictation (`/v1/dictation`): the microphone, Whisper on this
machine, the microphone permission, a two-minute cap, and one session at a time. The browser's
speech recognition was ruled out: inside the app's web view it sends audio to Apple or Google,
which breaks the local-first promise, and it does not exist in WebView2 on Windows.

Two things dictation could not do, and now can:

1. **Hand the words back without typing them.** Dictation's stop always typed the text at the
   cursor through Accessibility or the clipboard. A box inside Notewise places the words itself,
   so `POST /v1/dictation` takes `insert: false`. It is decided at the start, not the stop, so a
   session started for a box never types into another application, whoever ends it.
2. **Show words while they are being said.** Whisper decodes buffers, and the engine cuts one
   at a pause (about 0.8 s after the speaker stops) or at 8 s of unbroken speech. With
   `partials: true`, the capture loop also decodes the unfinished phrase every 300 ms and
   publishes `{ committed, interim }` on `GET /v1/dictation`.

Both default to what the hotkey did before, so it is unchanged and spends nothing on live text.

## Engine

- `UtteranceBuffer::pending()` returns the unfinished phrase without consuming it, and only once
  it holds enough speech to decode. That keeps the gate against Whisper inventing text over
  silence.
- `WhisperEngine::preview()` decodes it greedily. Kept text is still decoded with beam search.
- The gap between looks is at least 300 ms, counted from when the last look finished, and
  stretches to as long as that look took. A slow machine shows text less often instead of
  falling behind the microphone.
- A cut phrase replaces the guess at it, so no words are ever shown twice.

Measured on an M4 with Metal, on a `say` recording of a two-sentence question: a look takes
52 ms on average (slowest 94 ms) with `tiny.en`, which dictation picks when installed, and
92 ms (slowest 158 ms) with `base.en`. The first run after a build compiles Metal shaders and
took 2.2 s once.

## Client

- `lib/voice.ts`: `useVoiceInput` starts with `{ mode: "raw", insert: false, partials: true }`
  and reads the status every 250 ms while listening. Raw, because the question goes to a model
  anyway and cleaning first would be a second round trip.
- The button shows only when `dictationStatus().supported` is true.
- While listening, the box is a read-only view of the same size. It is not a live region: a
  screen reader reading the words back would be heard by the microphone.
- The box now grows with its content up to its existing cap, so a two-line spoken question
  shows both lines once it turns solid.

## Not built

Hold to talk, a keyboard shortcut for the mic, and live text for the dictation hotkey.

## Tests

- `stream.rs`: a look at a quiet room sees nothing; a look mid-phrase sees the speech; a look
  takes nothing (the cuts are identical with and without looks); a look after a cut does not
  repeat the phrase.
- `dictation.rs`: the throttle, a cut phrase replacing its guess, the request defaults, and the
  status fields.
- `engine.rs`, ignored (needs a model and a speech sample): words show before the first phrase
  is cut, and looking leaves the kept text unchanged. Prints each look's timing.
- `voice.test.ts`: joining kept and guessed words, and adding spoken words to typed ones.
- The real capture loop, with a live microphone, needs a person and a TCC grant, and was not
  run headlessly.
