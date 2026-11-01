Guide sections for the nine Settings pages. Written from apps/desktop/src/views/SettingsView.tsx and the components it renders; keep them in step.


## Settings: Appearance
page: settings/appearance
keywords: theme, themes, settings, dark mode, light mode, night, colour, color, colours, colors, accent, brightness, look, style
live: appearance

Settings, then Appearance, sets how Notewise looks.

- Dark is the default, meant to be easier on the eyes through long meetings.
- Light is for a bright room.
- System follows whatever macOS is set to.

Below those is a row of eleven accent colours: Graphite, Crimson, Ember, Amber, Moss, Teal, Ocean, Indigo, Violet, Plum and Slate.

A choice applies the moment you click it, with no Save button. It is kept by the engine rather than the browser, so it survives a restart. The recording indicator stays red in every theme and accent, so red always means audio is being captured.

## Settings: Recording permissions
page: settings/recording
keywords: microphone, mic, permission, permissions, allowed, allow, grant, granted, access, denied, system audio, screen recording, other side, call, privacy, macos
live: recording

Settings, then Recording, starts with what macOS lets Notewise hear. Nothing is captured until you press record.

- Microphone captures your side of the conversation.
- System audio captures everyone else, straight from the meeting app. macOS files this under the Screen & System Audio Recording permission.

Each row shows Granted, Not available, or a button. Enable asks macOS for the permission. If it was declined, the row offers Open Settings, which opens the right pane of macOS System Settings, and Re-check. After switching System audio on in System Settings, quit and reopen Notewise: macOS only applies it at launch. The page reads the permissions again whenever you come back to the window.

## Settings: Noticing meetings
page: settings/recording
keywords: detect, detection, detected, automatic, auto, notice, notify, notification, notifications, alert, prompt, offer, extension, browser, chrome, calendar, zoom, teams
live: detection

Noticing meetings is also under Settings, then Recording. When a meeting starts, Notewise offers to record it at the top of the window. It never starts recording on its own. There is nothing to switch on here; the section shows which signals are working:

- Browser extension: meetings opened in a browser tab are noticed once the extension has reported in.
- Calendar: an event with a meeting link is noticed after a short wait. With no calendar connected, Connect one takes you to Connectors.
- Notifications: Allow notifications lets the offer reach you when Notewise is behind another window. Otherwise it only shows inside the window. Once answered, notifications are turned on or off for Notewise in macOS System Settings.

Meetings taken only in the desktop Zoom or Teams app, with nothing on your calendar, are not noticed: Notewise does not watch what is running on your machine.

## Settings: Keeping audio
page: settings/recording
keywords: audio, retention, retain, keep, kept, delete, storage, disk, space, playback, play, listen, hear, sound, expire, days, recordings
live: retention

Keep the recording, under Settings, then Recording, decides what happens to the audio once it has been transcribed. Transcripts are always kept.

- Delete it after transcribing: the default. Nothing is left on disk, but you cannot play lines back.
- Keep it for a number of days: older recordings are deleted on their own and the transcripts stay. Delete expired now removes them straight away.
- Keep it until I delete the meeting: nothing expires.

Keeping audio lets you click a timestamp in a transcript to hear that moment. The page shows how many meetings have audio and how much space it takes. Switching back to Delete it after transcribing deletes the kept audio at once, after asking you to confirm. If your workspace cannot keep audio (an encrypted one cannot), the page says why.

## Settings: Transcription
page: settings/transcription
keywords: whisper, transcribe, transcribing, speech, accuracy, accurate, download, downloads, size, ram, english, multilingual, turbo, large, base, small
live: transcription

Settings, then Transcription, lists the Whisper models that turn audio into text on this machine. Bigger models are more accurate and slower. A name ending in .en is English-only; for other languages, pick a multilingual one.

Each model shows its trade-off, its download size and roughly how much memory it needs, and one is marked recommended. Press Download to fetch one. The download carries on if you switch pages and resumes if the connection drops. Downloaded models are marked Installed, and the page shows the folder they are kept in.

This page downloads models. It has no control for choosing which installed model a recording uses. The spoken language is chosen per recording with the Language picker in the top bar.

## Settings: Models
page: settings/models
keywords: model, llm, ai, backend, backends, provider, providers, ollama, anthropic, claude, gemini, groq, openrouter, lm studio, unsloth, openai, local, cloud, switch, change, choose, pick, current, using
live: models

Settings, then Models, chooses the AI that writes summaries, answers questions and suggests questions during a meeting. Click a backend to switch; it takes effect straight away, with no restart.

- A drive icon means it runs on this machine (Ollama, LM Studio, Unsloth). A cloud icon means transcripts are sent to that provider (Anthropic, Google Gemini, Groq, OpenRouter). Claude is Anthropic's: choose Anthropic to use Claude. The bar above the list names the current model and says which.
- While Ollama or LM Studio is active, the models it has installed are listed under it. Click one to use exactly that model.
- needs key means you must paste an API key first. needs URL (the OpenAI-compatible endpoint) means this window cannot supply the address, so it cannot be chosen here.

The model pill in the top bar offers the same switch from any page.

## Settings: API keys
page: settings/models
keywords: api key, key, keys, token, tokens, secret, keychain, paste, anthropic, claude, gemini, groq, openrouter, console, password, remove

Hosted providers (Anthropic, Google Gemini, Groq and OpenRouter) need an API key. Claude is Anthropic's, so a Claude API key is an Anthropic key and goes in the Anthropic row. In Settings, then Models, each one has a row under it: paste your key and press Save & use. Saving also switches Notewise to that provider.

The key goes to your operating system's keychain. It is never kept in the database or written to a log, and it is never shown again, so the row can only say that a key is saved. Remove deletes it. Keys come from the provider: console.anthropic.com, aistudio.google.com, console.groq.com or openrouter.ai/keys.

A key set in the engine's environment also works, and is never sent anywhere but its provider.

## Settings: Model routing
page: settings/models
keywords: routing, route, routes, rule, rules, policy, cost, costs, money, cheaper, expensive, heavy, split, mix

Model routing, lower down Settings, then Models, sends some kinds of work to a different model while everything else stays on the backend chosen above. With no rules, every request goes to that backend.

- With no rules, a Summaries to button for each backend installs a starting rule that sends summaries there.
- Each rule shows what it matches and where it sends it. A rule marked not in force is stored but cannot run, usually because its provider has no key. The bin button removes a rule.
- Where would a request go? shows, for summarize, chat, extract decisions or extract action items, which model would answer, without sending anything.

Rules cannot be written freely on this page; it installs the starting rule, shows what is in force, and removes rules.

## Settings: Summaries
page: settings/summaries
keywords: summary, summaries, summarise, summarize, template, templates, prompt, prompts, format, style, recap, minutes, custom, builtin, built

Settings, then Summaries, holds the summary templates. Each template is one prompt telling the model what to do with a transcript.

- New template adds one: give it a name, which appears on its button, and a prompt.
- Hover over a template and use the pencil to edit it or the bin to delete it.
- Built-in templates can be edited but not deleted, and their original wording is not kept, so an edit to one cannot be undone.

The templates appear on a meeting's Summary tab under Summarise it differently. Running one adds another summary rather than replacing the one you have, and deleting a template does not touch summaries already made with it.

## Settings: Memory
page: settings/assistant
keywords: memory, memories, remember, remembers, preferences, profile, personalise, personalize, learn, learning, facts, instructions, forget

Settings, then Assistant, starts with What it remembers about you: short facts added to the instructions for summaries and answers, so they shape what you get back.

- Type one (for example, that you prefer short summaries with bullet points) and press Remember. There is a fixed limit, and the page shows how many are used.
- Double-click a memory to edit it, or use its delete button to forget it. Each says whether you added it or it was inferred from a meeting.
- Learn from my meetings is off until you turn it on. It reads meetings you have recorded and keeps durable facts about you, never about other people. Read my meetings now runs it straight away and reports what it kept and what it discarded.

Everything remembered is listed on this page; nothing is hidden.

## Settings: Dictation
page: settings/assistant
keywords: dictate, dictation, voice typing, speech, talk, hotkey, hotkeys, type, insert, cursor, punctuation, accessibility, raw

Dictation, under Settings, then Assistant, types what you say wherever your cursor is, in any app. It is transcribed on this machine.

- Hotkey: the key combination that starts it. Press it once to start and again to stop. Type a combination using cmd, ctrl, alt and shift, then click away to save it.
- What to insert: Exactly what you said, where nothing leaves this machine, or Punctuation fixed, which sends the transcript to your configured model to add punctuation and capitals without rewriting your words.
- Try it checks whether dictation works on this machine. While it is listening, Stop and insert types the text and Discard throws it away.

If macOS has not granted a permission dictation needs, the page names it with a button that opens its settings.

## Settings: Assistant panel
page: settings/assistant
keywords: overlay, panel, floating, hotkey, screen, window, highlight, selection, selected, rewrite, shorten, translate, accessibility, apps

The assistant panel opens over whatever app you are working in. Ask about the window in front of you, or highlight text first and rewrite, shorten or translate it in place. What is sent to the model is shown in the panel every time.

Set its hotkey under Settings, then Assistant: type a combination and click away to save it. Reading the window in front needs the Accessibility permission. Reading text that is only on screen as pixels, such as a PDF or a screenshot, also needs Screen & System Audio Recording. Show me what it would read displays what the panel would see right now, before you use it.

## Settings: Inline suggestions
page: settings/assistant
keywords: autocomplete, complete, completion, completions, suggest, suggestion, finish, predict, prediction, typing, input monitoring, keystrokes

Inline suggestions notice when you stop typing and offer to finish the sentence. The suggestion appears in Notewise, not as grey text inside the other app, because macOS gives no way to draw into another app's window.

Turn on Watch for typing pauses under Settings, then Assistant. It needs Input Monitoring, the most invasive permission macOS has. Notewise records only that a key was pressed and when: no key codes, no characters, nothing kept. It is off until you turn it on, and no other feature turns it on.

The Try it box below works without that permission: type half a sentence and press Suggest to see what the model would offer.

## Settings: Tools
page: settings/tools
keywords: mcp, tool, tools, server, servers, external, integration, integrations, plugin, plugins, linear, issue tracker, approve, confirm, connect

Settings, then Tools, connects external tool servers (MCP servers), so an action item can become an issue in another system, a message, or whatever else a server offers.

- Add a server: give it a name and either a local command with its arguments or a remote URL. Any environment values or headers go to your system keychain.
- Each server has an On/Off button, a start or stop button, and a remove button. Expand it to see its tools and tick the ones that may be proposed.
- On My Tasks, an open task then gets a Tool button that proposes one call.

Every call is shown in full and runs only when you press Approve and run. There is no automatic execution and no way to turn confirmation off. A server sees whatever a call you approve contains, which can include what was said in a meeting.

## Settings: Search
page: settings/search
keywords: semantic, meaning, embedding, embeddings, index, indexing, vector, similar, synonyms, related, fuzzy, ollama, rebuild
live: search

Settings, then Search, turns on search by meaning. Without it, search matches words: asking about "pricing" will not find a meeting that only ever said "cost structure". Building the index fixes that for Ask, the Ask tabs and the agent. Word search keeps working either way.

- Build index starts it and shows progress. Once built, the page says On and how many passages are indexed.
- It keeps itself up to date: a note you write becomes answerable a few seconds after you stop typing. Update by hand only after restoring a backup or if a pass failed. Clear removes the index.
- It runs on this machine through Ollama. Nothing is uploaded, and it never goes to your chat provider. If Ollama is not running or its model has not been pulled, the page says so.

## Settings: Speakers
page: settings/speakers
keywords: speaker, diarization, diarisation, voices, voice, separate, separation, label, labels, names, rename, identify, imported
live: speakers

Settings, then Speakers, has Separate speakers by voice. It is for recordings where nothing can say who was talking: an imported file, or one microphone in a room. It groups the audio into distinct voices labelled Speaker 1, Speaker 2 and so on. Naming them is up to you: click a speaker's name in a transcript.

- Work out speakers from the audio is off by default and applies to imported recordings. Live calls already know who spoke from which audio stream it arrived on.
- Once it is on, choose a model and Download it (Remove deletes it later). A line underneath says whether it is active or what is still missing; some builds do not include it.

It is a guess, so expect to merge or rename speakers afterwards. Everything runs on this machine and no audio is uploaded.

## Settings: Recognising voices
page: settings/speakers
keywords: voiceprint, voiceprints, recognise, recognize, recognition, fingerprint, biometric, identify, colleagues, forget

Also under Settings, then Speakers, Recognise people by voice is not in this build yet: nothing records or matches voices across meetings so far. Its switch, Remember voices between meetings, records your answer now, so nothing is kept until you have said yes. It is off by default.

When it arrives it will keep a numeric fingerprint of each voice, never audio, on this machine only. It identifies the other people in your meetings, who have not been asked. Forget all erases any stored voices, and switching it off erases them too.

## Settings
page: settings/appearance
keywords: preferences, options, configure, configuration, setup, categories, tabs, saved, restart, banner

Settings is near the bottom of the sidebar. It has nine pages, chosen from the row of tabs across the top: Appearance, Recording, Transcription, Models, Summaries, Assistant, Tools, Search and Speakers. Opening Settings shows Appearance first.

Settings are kept by the Notewise engine on this machine, not by the window, so they survive a restart. Most apply the moment you pick an option.

If a banner at the top of the window says something needs fixing (no transcription model, an AI backend that is not answering, or missing permissions), its Fix in Settings button opens the page that fixes it.

Ask can explain any setting and tell you where it is, but it never changes one for you.
