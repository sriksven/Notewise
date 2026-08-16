import { useEffect, useRef, useState } from "react";
import {
  AlertTriangle,
  Bot,
  BookOpen,
  Check,
  FileText,
  Loader2,
  Search,
  Send,
  X,
} from "lucide-react";

import { api, ApiError, type AgentRun } from "../lib/api";
import type { Route } from "../lib/router";

interface Props {
  onNavigate: (route: Route) => void;
}

/** How often to ask a running agent what it is up to. */
const POLL_MS = 1_000;

const SUGGESTIONS = [
  "What did we decide about pricing across all our meetings?",
  "Summarize everything discussed about hiring this month.",
  "What commitments are still open, and who owns them?",
  "Write a brief on what changed since the last planning meeting.",
];

const ICONS: Record<string, typeof Search> = {
  search: Search,
  read: BookOpen,
  recent_meetings: BookOpen,
  write_note: FileText,
  finish: Check,
  think: Bot,
};

/**
 * The agent.
 *
 * Give it a task in plain language; it searches your meetings and notes, reads what looks
 * relevant, and writes up what it found as a new note.
 *
 * The step list is the point of this screen. An agent that hands back a paragraph asks to be
 * trusted; one that shows what it searched for and what it read can be checked. When it gets
 * something wrong — and a lexical search plus a local model will — the trace is what tells you
 * whether it looked in the wrong place or read the right thing and drew the wrong conclusion.
 *
 * What it can do to the workspace is exactly one thing: create a note. It cannot edit or
 * delete anything that already exists, and it has no route to a connector or to sending mail.
 */
export function AgentView({ onNavigate }: Props) {
  const [task, setTask] = useState("");
  const [run, setRun] = useState<AgentRun | null>(null);
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const endRef = useRef<HTMLDivElement>(null);

  const running = run?.status === "running";

  // Poll while it works. Stops on the terminal status rather than running forever, and the
  // interval is torn down if the user navigates away mid-run — the run itself continues in the
  // engine, and coming back re-reads it.
  useEffect(() => {
    if (!run || run.status !== "running") return;

    let cancelled = false;
    const id = setInterval(() => {
      void api
        .agentRun(run.id)
        .then((next) => !cancelled && setRun(next))
        .catch((e) => {
          if (cancelled) return;
          setError(e instanceof ApiError ? e.message : "Lost track of the run.");
        });
    }, POLL_MS);

    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [run]);

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [run?.steps.length, run?.status]);

  const start = async (text: string) => {
    const trimmed = text.trim();
    if (!trimmed || starting || running) return;

    setStarting(true);
    setError(null);
    try {
      setRun(await api.startAgentRun(trimmed));
      setTask("");
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not start the agent.");
    } finally {
      setStarting(false);
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex items-center gap-3 border-b border-hairline px-8 py-3">
        <Bot size={16} className="shrink-0 text-ink-faint" aria-hidden />
        <h1 className="text-[14px] font-semibold text-ink">Agent</h1>
        <span className="flex-1 text-[12px] text-ink-faint">
          {running
            ? `Working — step ${run.steps.length}`
            : run
              ? run.status === "failed"
                ? "Failed"
                : "Finished"
              : "Idle"}
        </span>
        {run && !running && (
          <button
            type="button"
            onClick={() => setRun(null)}
            className="rounded-full border border-hairline px-2.5 py-1 text-[12px]
                       text-ink-muted transition hover:bg-overlay hover:text-ink"
          >
            New task
          </button>
        )}
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto px-8 py-6">
        <div className="mx-auto max-w-2xl">
          {!run ? (
            <>
              <p className="text-[13px] leading-relaxed text-ink-muted">
                Describe what you want looked up. The agent searches your meetings, notes and
                tickets, reads what it finds, and saves a write-up as a new note.
              </p>
              <p className="mt-2 text-[12px] leading-relaxed text-ink-faint">
                It only ever creates a note. It cannot change or delete anything you already
                have, and nothing leaves this machine unless your AI backend is a hosted one.
              </p>

              <div className="mt-6 space-y-1.5">
                {SUGGESTIONS.map((suggestion) => (
                  <button
                    key={suggestion}
                    type="button"
                    onClick={() => void start(suggestion)}
                    className="card w-full px-4 py-2.5 text-left text-[12.5px] text-ink-muted
                               transition hover:bg-overlay hover:text-ink"
                  >
                    {suggestion}
                  </button>
                ))}
              </div>
            </>
          ) : (
            <>
              <div className="card mb-5 px-4 py-3">
                <p className="text-[11px] font-semibold uppercase tracking-wider text-ink-faint">
                  Task
                </p>
                <p className="mt-0.5 text-[13px] leading-relaxed text-ink">{run.task}</p>
              </div>

              <ol className="space-y-2">
                {run.steps.map((step) => {
                  const Icon = ICONS[step.action] ?? Bot;
                  return (
                    <li key={step.n} className="card overflow-hidden">
                      <div className="flex items-start gap-2.5 px-4 py-2.5">
                        <Icon
                          size={14}
                          className="mt-0.5 shrink-0 text-ink-faint"
                          aria-hidden
                        />
                        <div className="min-w-0 flex-1">
                          <p className="text-[12.5px] font-medium text-ink">
                            {label(step.action)}
                            {step.reason && (
                              <span className="font-normal text-ink-muted">
                                {" — "}
                                {step.reason}
                              </span>
                            )}
                          </p>
                          {/* Pre-wrapped: the observation is a list of search results or a
                              slab of transcript, and reflowing it makes both unreadable. */}
                          <pre className="mt-1 whitespace-pre-wrap break-words font-sans text-[12px] leading-relaxed text-ink-muted">
                            {step.observation}
                          </pre>
                        </div>
                      </div>
                    </li>
                  );
                })}
              </ol>

              {running && (
                <p className="mt-3 flex items-center gap-2 text-[12.5px] text-ink-faint">
                  <Loader2 size={13} className="animate-spin" aria-hidden />
                  Thinking
                </p>
              )}

              {run.status === "failed" && (
                <div
                  role="alert"
                  className="mt-4 flex items-start gap-2 rounded-xl border border-warn-line
                             bg-warn px-4 py-3 text-[12.5px] leading-relaxed text-warn-text"
                >
                  <AlertTriangle size={14} className="mt-0.5 shrink-0" aria-hidden />
                  {run.error ?? "The run stopped without saying why."}
                </div>
              )}

              {run.status === "done" && (
                <div className="mt-4 space-y-3">
                  {run.result && (
                    <p className="text-[13px] leading-relaxed text-ink">{run.result}</p>
                  )}
                  {run.note_id ? (
                    <button
                      type="button"
                      onClick={() =>
                        onNavigate({ name: "notes", id: run.note_id ?? undefined })
                      }
                      className="btn-accent"
                    >
                      <FileText size={14} aria-hidden />
                      Open “{run.note_title ?? "the note"}”
                    </button>
                  ) : (
                    <p className="text-[12.5px] text-ink-faint">
                      It did not write a note. The steps above show what it looked at.
                    </p>
                  )}
                </div>
              )}

              <div ref={endRef} />
            </>
          )}

          {error && (
            <p role="alert" className="mt-4 flex items-start gap-2 text-[12.5px] text-danger-text">
              <X size={14} className="mt-0.5 shrink-0" aria-hidden />
              {error}
            </p>
          )}
        </div>
      </div>

      {!run && (
        <div className="border-t border-hairline px-8 py-3">
          <div className="mx-auto flex max-w-2xl items-end gap-2">
            <textarea
              value={task}
              onChange={(event) => setTask(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && !event.shiftKey) {
                  event.preventDefault();
                  void start(task);
                }
              }}
              rows={1}
              placeholder="What should it look up?"
              aria-label="Task for the agent"
              className="max-h-32 flex-1 resize-none rounded-xl border border-hairline px-3 py-2
                         text-[14px] text-ink outline-none transition
                         placeholder:text-ink-faint focus:border-accent"
            />
            <button
              type="button"
              onClick={() => void start(task)}
              disabled={starting || task.trim().length === 0}
              aria-label="Start"
              className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full
                         bg-accent text-accent-on transition hover:bg-accent-hover
                         disabled:bg-hairline disabled:text-ink-faint"
            >
              {starting ? (
                <Loader2 size={15} className="animate-spin" aria-hidden />
              ) : (
                <Send size={15} aria-hidden />
              )}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function label(action: string): string {
  switch (action) {
    case "search":
      return "Searched";
    case "read":
      return "Read";
    case "recent_meetings":
      return "Listed recent meetings";
    case "write_note":
      return "Wrote a note";
    case "finish":
      return "Finished";
    case "think":
      // Named honestly. This step is the agent failing to produce a usable action and being
      // told so; calling it "Thought" would dress up a stumble as deliberation.
      return "Lost the thread";
    default:
      return action;
  }
}
