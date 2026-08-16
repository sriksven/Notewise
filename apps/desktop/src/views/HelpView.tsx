import { BookOpen, CircleHelp, Keyboard, LifeBuoy, Sparkles } from "lucide-react";

import type { HelpSection, Route } from "../lib/router";

interface Props {
  section: HelpSection;
  onNavigate: (route: Route) => void;
}

const TABS: Array<{ id: HelpSection; label: string; Icon: typeof BookOpen }> = [
  { id: "docs", label: "Documentation", Icon: BookOpen },
  { id: "shortcuts", label: "Shortcuts", Icon: Keyboard },
  { id: "whats-new", label: "What's new", Icon: Sparkles },
  { id: "support", label: "Get support", Icon: LifeBuoy },
];

/**
 * Help.
 *
 * Written in the app rather than linked out to a website, because the failure modes worth
 * documenting are all local: a build that cannot record, a model that has not been downloaded,
 * a daemon that is not running. A user hitting one of those may also have no network.
 *
 * Everything here is answerable from the repository as it stands. There are no links to pages
 * that do not exist and no promises about a roadmap — a help page that describes a different
 * version of the app is worse than none.
 */
export function HelpView({ section, onNavigate }: Props) {
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex items-center gap-3 border-b border-hairline px-8 py-3">
        <CircleHelp size={16} className="shrink-0 text-ink-faint" aria-hidden />
        <h1 className="text-[14px] font-semibold text-ink">Help</h1>
      </header>

      <nav
        aria-label="Help sections"
        className="flex gap-1 border-b border-hairline px-8 py-2"
      >
        {TABS.map((tab) => (
          <button
            key={tab.id}
            type="button"
            onClick={() => onNavigate({ name: "help", section: tab.id })}
            aria-current={section === tab.id ? "page" : undefined}
            className={`flex items-center gap-1.5 rounded-full px-3 py-1 text-[12.5px] transition ${
              section === tab.id
                ? "bg-overlay font-medium text-ink"
                : "text-ink-muted hover:bg-overlay hover:text-ink"
            }`}
          >
            <tab.Icon size={13} aria-hidden />
            {tab.label}
          </button>
        ))}
      </nav>

      <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
        <div className="mx-auto max-w-2xl">
          {section === "docs" && <Docs onNavigate={onNavigate} />}
          {section === "shortcuts" && <Shortcuts />}
          {section === "whats-new" && <WhatsNew />}
          {section === "support" && <Support onNavigate={onNavigate} />}
        </div>
      </div>
    </div>
  );
}

function Topic({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="mb-7">
      <h2 className="mb-1.5 text-[13.5px] font-semibold text-ink">{title}</h2>
      <div className="space-y-2 text-[12.5px] leading-relaxed text-ink-muted">{children}</div>
    </section>
  );
}

function Link({ children, onClick }: { children: React.ReactNode; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="text-ink underline decoration-hairline underline-offset-2 transition hover:decoration-ink"
    >
      {children}
    </button>
  );
}

function Docs({ onNavigate }: { onNavigate: (route: Route) => void }) {
  return (
    <>
      <Topic title="Recording a meeting">
        <p>
          Press record on the{" "}
          <Link onClick={() => onNavigate({ name: "record" })}>Record</Link> page. The engine
          opens the microphone and creates the meeting in the same step, so a window reload
          never leaves a recording running with nothing attached to it.
        </p>
        <p>
          Transcription runs in windows rather than word by word, so text lands a few seconds
          behind the audio. That is normal and not a sign it has stalled.
        </p>
      </Topic>

      <Topic title="Why system audio does not work yet">
        <p>
          The microphone hears you and whoever is in the room. It does not hear the other
          people on a call, because that audio never reaches it — it goes to your speakers.
        </p>
        <p>
          Capturing it needs macOS's ScreenCaptureKit, which will only grant screen-recording
          permission to a signed, bundled application. A development build cannot hold that
          permission no matter how many times the dialog is accepted, so Notewise stops asking
          rather than sending you to System Settings for a switch that will not stick.
        </p>
        <p>
          Until then, the practical answer is the meeting app's own recording, imported here.
        </p>
      </Topic>

      <Topic title="Choosing a transcription model">
        <p>
          Larger models hear better, especially through a bad connection or an accent the small
          ones were not trained for. <code className="text-ink">large-v3-turbo</code> is the
          one worth downloading — it is close to the largest model in accuracy at a fraction of
          the time, and it is what fixes most "it did not hear that properly" transcripts.
        </p>
        <p>
          The <code className="text-ink">.en</code> models are English-only and slightly better
          at it. Anything else, pick a multilingual one.
        </p>
        <p>
          Models are downloaded in{" "}
          <Link onClick={() => onNavigate({ name: "settings" })}>Settings</Link> and stored
          beside the database.
        </p>
      </Topic>

      <Topic title="Where your data lives">
        <p>
          Everything is a SQLite file on this machine. Transcription runs locally. Whether
          summaries and answers stay local depends on the backend you picked: a local one keeps
          them here, and a hosted one sends the transcript to that provider.{" "}
          <Link onClick={() => onNavigate({ name: "about" })}>About</Link> reports which is
          active.
        </p>
        <p>
          API keys are kept in the OS keychain. No endpoint returns one, and no key is ever sent
          over HTTP — the engine reads them from its own environment.
        </p>
      </Topic>

      <Topic title="Asking questions about your material">
        <p>
          Every meeting has an Ask tab, every note has one, and the{" "}
          <Link onClick={() => onNavigate({ name: "agent" })}>Agent</Link> searches across all
          of it. Answers cite what they were drawn from, and clicking a citation opens it.
        </p>
        <p>
          By default retrieval matches <em>words</em>: asking about "pricing" will not find a
          meeting that only ever said "cost structure". Building the search index in{" "}
          <Link onClick={() => onNavigate({ name: "settings" })}>Settings</Link> fixes that —
          it embeds your workspace locally, through Ollama, and never through your chat
          provider.
        </p>
        <p>
          Even with it on, a small workspace can surface the nearest thing rather than nothing.
          Read the citations: an answer citing something irrelevant is telling you it did not
          find what you asked for.
        </p>
      </Topic>

      <Topic title="Deleting things">
        <p>
          Deleting a note moves it to the{" "}
          <Link onClick={() => onNavigate({ name: "trash" })}>Trash</Link>, where it stays until
          you empty it. Nothing expires on a timer.
        </p>
        <p>
          Meetings cannot be deleted from this window. That is deliberate for now — a meeting
          owns its transcript and everything derived from it, and a one-click delete of all of
          that has no undo.
        </p>
      </Topic>
    </>
  );
}

/**
 * The keyboard.
 *
 * This list and `lib/shortcuts.ts` have to agree. A help page describing keys that do nothing
 * is worse than no help page — it sends someone looking for a fault in their keyboard.
 */
function Shortcuts() {
  const rows: Array<[string, string]> = [
    ["⌘K", "Jump to the search box"],
    ["⌘N", "New note"],
    ["⌘⇧R", "Start or stop recording"],
    ["⌘B", "Bold the selection"],
    ["⌘I", "Italic"],
    ["⌘E", "Inline code"],
    ["⌘⇧X", "Strikethrough"],
    ["/", "Block menu, on an empty line"],
    ["Enter", "Send a question"],
    ["⇧Enter", "New line in a question"],
    ["Esc", "Close an open menu"],
  ];

  return (
    <>
      <Topic title="Keyboard">
        <p>
          The few that are worth the muscle memory. Ctrl works in place of ⌘. The formatting
          chords act on the selection inside a note.
        </p>
      </Topic>
      <dl className="card divide-y divide-hairline overflow-hidden">
        {rows.map(([keys, what]) => (
          <div key={keys} className="flex items-center gap-4 px-4 py-2.5">
            <dt className="w-24 shrink-0">
              <kbd className="rounded border border-hairline bg-overlay px-1.5 py-0.5 font-mono text-[11.5px] text-ink">
                {keys}
              </kbd>
            </dt>
            <dd className="text-[12.5px] text-ink-muted">{what}</dd>
          </div>
        ))}
      </dl>
      <p className="mt-3 text-[12px] leading-relaxed text-ink-faint">
        Recording is on ⌘⇧R rather than ⌘R because an unshifted ⌘R reloads the window, and
        taking that away removes the way out of one that has wedged.
      </p>
    </>
  );
}

/**
 * The changelog.
 *
 * Kept in the app and written in terms of what changed for the person using it, not the commit
 * that changed it. Newest first, and short — a release note nobody reads is worse than none,
 * and the way to make one unreadable is to list everything.
 */
function WhatsNew() {
  const releases: Array<{ version: string; date: string; changes: string[] }> = [
    {
      version: "Unreleased",
      date: "in development",
      changes: [
        "A home page, a record page, and a library that groups meetings by when they happened.",
        "Notes attach to a meeting, and every note can be asked questions about itself.",
        "An agent that searches across your workspace and writes up what it finds.",
        "Deleting a note now moves it to a trash you can recover from.",
        "Help, in the app rather than a website you might not be able to reach.",
      ],
    },
    {
      version: "Earlier",
      date: "",
      changes: [
        "Import an audio file with a real file picker instead of typing a path.",
        "The input device list no longer hangs when microphone access has not been granted.",
        "large-v3-turbo and its quantized build, which fix most poor transcripts.",
        "Recognising people by voice is off by default, and switching it off erases what was stored.",
        "A theme with eleven accents, and API keys that actually persist to the keychain.",
      ],
    },
  ];

  return (
    <>
      {releases.map((release) => (
        <section key={release.version} className="mb-7">
          <div className="mb-2 flex items-baseline gap-2">
            <h2 className="text-[13.5px] font-semibold text-ink">{release.version}</h2>
            {release.date && (
              <span className="text-[11.5px] text-ink-faint">{release.date}</span>
            )}
          </div>
          <ul className="space-y-1.5">
            {release.changes.map((change) => (
              <li
                key={change}
                className="flex gap-2 text-[12.5px] leading-relaxed text-ink-muted"
              >
                <span className="mt-[7px] h-1 w-1 shrink-0 rounded-full bg-ink-faint" aria-hidden />
                {change}
              </li>
            ))}
          </ul>
        </section>
      ))}
    </>
  );
}

function Support({ onNavigate }: { onNavigate: (route: Route) => void }) {
  return (
    <>
      <Topic title="Something is not working">
        <p>
          Start with <Link onClick={() => onNavigate({ name: "about" })}>About</Link>. It reports
          what this build can actually do — whether the engine is reachable, whether it can
          record, which model is loaded and whether it runs locally. Most reports turn out to be
          one of those saying no.
        </p>
      </Topic>

      <Topic title="Common answers">
        <p>
          <strong className="text-ink">The record button does nothing.</strong> This build was
          compiled without capture, or the engine is running against an in-memory database. About
          says which.
        </p>
        <p>
          <strong className="text-ink">No input devices are listed.</strong> macOS does not
          reveal them until microphone access is granted. Grant it in Settings and reopen the
          picker.
        </p>
        <p>
          <strong className="text-ink">Summaries fail.</strong> A local backend needs its daemon
          running; a hosted one needs a key. Settings reports both.
        </p>
        <p>
          <strong className="text-ink">The transcript is poor.</strong> Almost always the model.
          Download <code className="text-ink">large-v3-turbo</code> and try the same audio again.
        </p>
      </Topic>

      <Topic title="Reporting a bug">
        <p>
          Include what About shows, what you did, and what happened instead. If it involves a
          transcript, the model name matters more than anything else.
        </p>
        <p>
          Do not paste a transcript into a public issue. It is a recording of people who did not
          agree to that.
        </p>
      </Topic>
    </>
  );
}
