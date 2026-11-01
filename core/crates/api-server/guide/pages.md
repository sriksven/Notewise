Guide sections for each sidebar page, a meeting's own page, and the top bar.

## Home
page: home
keywords: home, start page, landing, dashboard, overview, greeting, welcome, recent

Home is the first page in the sidebar, and clicking the logo at the top of the sidebar also returns to it.

- Start recording captures from your microphone and takes you to the Record page. While a recording runs it reads Recording now and takes you back to it.
- Import audio transcribes a recording you already have.
- Recent meetings lists your latest meetings; See all opens the Library.
- Open work shows a few open action items from recent meetings; All tasks opens My Tasks.
- Recent notes shows your latest notes; All notes opens Notes.
- Ask the agent to look something up opens the Agent.

On a build that cannot record, Start recording and Import audio are unavailable.

## Record
page: record
keywords: record, recording, capture, start, stop, begin, meeting, meetings, microphone, mic, input, live, import, upload, file

The Record page is for the moment before and during a meeting. Press the big button to start recording and press it again to stop. ⌘⇧R does the same from any page, as long as you are not typing in a text field.

- Before you start, choose the Input, or leave it on System default. The line below shows the spoken language and the AI model.
- While recording, a timer runs and Coming through shows the latest lines. Text arrives a few seconds behind the audio because transcription works in short windows. Open the meeting goes to its full page.
- Import an audio file instead transcribes a file you already have.

Audio is transcribed on this machine. While a recording runs, a red Recording link at the bottom of the sidebar takes you back to it from any page. If this build cannot capture audio, the page says so, and the button creates a meeting you can import a transcript into.

## Library
page: library
keywords: meetings, recordings, list, history, past, archive, filter, find, browse, older, previous

Library lists your meetings, newest first, grouped into Today, Yesterday, This week, This month and then by month. It shows up to the 50 most recent. Each row shows when it happened and where it came from, and a live tag marks one that is recording now.

- Filter by title narrows the list by meeting title. ⌘K jumps to this box from any page.
- To search what was said rather than titles, open any meeting and use Search what was said in the column of meetings on the left (⌘K goes there on a meeting page), or ask a question in Ask.
- With no meetings yet, Record and Import audio get you started.

Click a meeting to open it. Deleted meetings are in the Trash, not here.

## Meeting pages
page: library
keywords: meeting, transcript, summary, summarize, summarise, decisions, action items, tabs, rename, title, export, markdown, vault, email, panel

Opening a meeting, from Home, the Library or a citation, shows it with four tabs:

- Transcript: what was said, by speaker.
- Summary: Summarize runs your AI backend over the transcript. Below it, Summarise it differently runs another template, and Follow-up email drafts an email in a tone you pick for you to copy or open in your mail app. Nothing is sent.
- Notes: your own notes on the meeting. They are real notes and also appear under Notes.
- Ask: questions about this meeting only.

The panel on the right shows who was there, questions worth asking while recording, decisions, items carried over, action items and linked items. Double-click the title to rename the meeting. The More options menu (three dots) on the floating record bar has Import an audio file, Export as Markdown, Export summary only and Write to vault folder. The bin beside the title moves the meeting to the trash.

## Meeting transcripts and speakers
page: library
keywords: transcript, transcripts, speaker, speakers, name, rename, merge, who spoke, correct, correction, fix, mistake, typo, edit, play, playback, timestamp, unattributed

On a meeting's Transcript tab:

- Click a speaker's name, such as Speaker 2, to type who it is. You can also merge it into someone already in the meeting. Names apply across the whole transcript. Speakers cannot be renamed while the meeting is still recording.
- Double-click a line to correct a word the transcriber got wrong. Enter saves and Escape cancels.
- If the audio was kept, click a timestamp to play from that moment, and a player sits below the transcript. Audio is only kept if you chose to under Settings, then Recording.

Lines with no speaker show as Unattributed. To have imported recordings split into voices, see Settings, then Speakers.

## Notes
page: notes
keywords: note, notes, notebook, write, writing, document, editor, blocks, autosave, save, saved, new note, untitled, journal, delete, remove

Notes holds everything you write, including notes taken on a meeting's Notes tab. It lists your most recent notes.

- The + button, or ⌘N from any page when you are not typing in a field, makes a new note.
- Notes save themselves as you type; beside the title it says Saving, Saved or Not saved.
- Type / on an empty line for the block menu: text, headings, bulleted and numbered lists, to-dos, quotes, code, dividers, tables and images. ⌘B, ⌘I, ⌘E and ⌘⇧X format the selection.
- Ask, beside the title, opens a chat about that note only, drawn in a second colour.
- The bin beside a note in the list moves it to the trash.

Notes are kept as plain Markdown, which search, Ask and the agent can all read.

## My Tasks
page: tasks
keywords: task, tasks, todo, to-do, action item, action items, commitments, owed, assignments, due, deadline, overdue, complete, done, tick, checkbox

My Tasks gathers the action items from your meetings in one list, so you do not need to remember which meeting agreed them. It reads your 40 most recent meetings.

- Each task shows its owner, its due date (highlighted when overdue) and the meeting it came from. Click the meeting's name to open its summary.
- Tick the box to mark a task done, and tick it again to reopen it. Show completed brings done tasks back into the list.
- If you have allowed a tool under Settings, then Tools, open tasks get a Tool button. It proposes one call and sends nothing until you press Approve and run.

To get tasks, summarize a meeting: anything that sounded like a commitment lands here. You can also add action items by hand in a meeting's side panel.

## Ask
page: ask
keywords: ask, question, questions, chat, answer, answers, query, citation, citations, sources, references, conversation, chatbot

Ask answers questions from your own meetings and notes. Every claim carries a numbered citation; click it to open the source. If nothing you have answers the question, Ask says so instead of guessing.

- Ask in the sidebar opens it as a page.
- The Ask button at the bottom right of most pages opens the same thing as a panel beside what you are looking at. It stays open as you move around, so a citation opens next to the answer.
- Each meeting has an Ask tab and each note has its own Ask, for questions about just that one.

Press Enter to send and ⇧Enter for a new line. Ask also answers questions about Notewise itself, such as where a setting is or how many recordings you have. It explains and links to the page; it never changes a setting. It matches words unless you turn on search by meaning under Settings, then Search.

## Agent
page: agent
keywords: agent, research, researcher, look up, investigate, report, brief, write up, bot, autonomous

The Agent takes a task in plain language, searches your meetings and notes, reads what looks relevant, and saves a write-up as a new note.

- Type what you want looked up and press Enter, or pick one of the suggestions.
- Each step is listed as it happens: what it searched for, what it read, and why. That is how you check whether it looked in the right place.
- When it finishes, a button opens the note it wrote. New task starts another.
- Earlier lists past runs and the notes they produced. A run keeps going if you leave the page.

It only ever creates a note: it cannot change or delete anything you already have. Nothing leaves this machine unless your AI backend is a hosted one.

## Connectors
page: connectors
keywords: connector, connectors, integration, integrations, export, obsidian, logseq, webhook, vault, markdown, folder, send, deliver, delivery

Connectors lists what this build can send meetings to or read from. There is nothing to install, and each one says whether it stays on this machine or leaves it.

- Markdown vault writes each meeting to a folder as Markdown, for Obsidian, Logseq or anything that reads plain files. Enter the folder and press Connect. On a meeting, Write to vault folder in the More options menu writes that meeting now.
- Webhook posts each meeting as signed JSON to a URL, which leaves your machine. Its signing secret is shown once when you connect, so copy it then.
- Files you edited appears if you changed a vault file yourself. Choose Keep as a note, then refresh; Leave my file, stop mirroring; or Discard my edits.
- Failed deliveries lists anything that could not be sent.

Calendar and mail, and Documents to read, are further down the same page.

## Connectors: Calendar and mail
page: connectors
keywords: calendar, calendars, google, gmail, outlook, microsoft, office, email, mail, drafts, events, schedule, sign in, apps script

Calendar and mail, under Connectors, connects Microsoft 365 or Google. A connected calendar gives meetings their real titles, links a recording to its event, turns attendees into names, and lets Notewise notice meetings as they start. Notewise never writes to your calendar and never sends mail: a follow-up becomes a draft in your own mailbox for you to send.

- Microsoft 365: enter the client id of an app registration in your Microsoft 365 tenant, tick whether Notewise may write drafts, and press Sign in with Microsoft to finish in your browser.
- Google: Show me the five steps explains deploying a small script into your own Google account. Paste its deployment URL and the key you chose, then press Connect. The key goes to your system keychain.

Once connected, Notewise checks every ten minutes, and Sync now checks straight away.

## Connectors: Documents to read
page: connectors
keywords: watched folder, watch, documents, docs, files, specs, import, text, folder, cite, read only

Documents to read, under Connectors, makes the text and Markdown files in a folder you choose searchable alongside your meetings, so an answer can cite a spec you wrote elsewhere.

Type or paste the folder's full path and press Watch this folder. Notewise reads at most a few hundred files, skips anything large or binary, and never reads above the folder you name. It checks every ten minutes. It is read only: nothing in the folder is ever changed, moved or written to. Stop watching disconnects it.

## Trash
page: trash
keywords: trash, bin, deleted, delete, recycle, restore, undo, recover, permanently, empty, purge, removed, notes, meetings

Deleted things go to the Trash, near the bottom of the sidebar: deleting a meeting or a note moves it there. Nothing in the Trash expires on a timer.

- Restore brings an item back. A meeting comes back with its transcript, summaries, decisions and action items.
- The bin button on a row, then Delete for good, removes that item permanently.
- Empty trash, then Delete, removes everything in it permanently, including the transcripts of trashed meetings.

Deleting for good cannot be undone. A meeting goes to the trash from its own page (the bin beside the title, then Delete), and a note from the bin beside it in the Notes list.

## Top bar
page: record
keywords: top bar, toolbar, header, pill, dropdown, menu, model, models, current, devices, device, microphone, mic, input, headset, headphones, airpods, language, languages, spoken, detect, local, cloud, badge

The bar across the top of every page has three pickers and a badge.

- The model pill shows the AI model in use. Its menu lists each AI backend, saying whether it runs on this machine, sends transcripts to the provider, or still needs a key or URL, plus the models Ollama or LM Studio has installed when one of them is active. Choosing one switches immediately, the same as Settings, then Models.
- Devices picks the microphone for the next recording, or System default. An empty list usually means microphone access has not been granted.
- Language picks the spoken language, or Detect to let the model work it out. English-only models ignore it.
- The badge on the right reads Local when transcripts are processed on this machine and Cloud when they are sent to a provider.

Devices and Language cannot be changed while recording, and they are not kept after the app restarts.
