import { AlertTriangle, Check, CircleHelp, Loader2, Send, X } from "lucide-react";

import type { ToolExecution } from "../lib/api";

interface Props {
  execution: ToolExecution;
  /** The server's name, resolved by the caller from the server list. */
  serverName: string;
  onConfirm: () => void;
  onReject: () => void;
  /** Send a call that was confirmed and never went out. */
  onSend?: () => void;
  busy: boolean;
}

/**
 * One external tool call, before it runs and after.
 *
 * # Why the arguments are the main thing on the card
 *
 * The confirmation is the only thing standing between a model's suggestion and a ticket filed in
 * someone else's system. It is worth nothing if what the user approves is a summary of the call
 * rather than the call. So every field is shown, exactly as it will be sent, and the raw JSON is
 * there too — a nested object rendered as "[object]" is how a confirmation stops meaning anything.
 *
 * # Why a timeout does not say "failed"
 *
 * A call that timed out may have taken effect. Telling someone it failed invites them to do it
 * again, and the second ticket is not free to undo. So `unknown` gets its own wording, and it
 * names the server they have to go and look at.
 */
export function ToolCallReview({
  execution,
  serverName,
  onConfirm,
  onReject,
  onSend,
  busy,
}: Props) {
  const fields = readArguments(execution.arguments);

  return (
    <div className="card overflow-hidden">
      <header className="flex items-center gap-2 border-b border-hairline px-3 py-2">
        <code className="rounded bg-overlay px-1.5 py-0.5 text-[11.5px] text-ink">
          {execution.tool_name}
        </code>
        <span className="text-[11.5px] text-ink-faint">on {serverName}</span>
        <span className="flex-1" />
        <StatusChip execution={execution} />
      </header>

      <div className="px-3 py-2.5">
        <p className="mb-1.5 text-[11px] font-medium uppercase tracking-wide text-ink-faint">
          Arguments
        </p>

        {fields.length === 0 ? (
          <p className="text-[12.5px] text-ink-muted">This tool takes no arguments.</p>
        ) : (
          <dl className="space-y-1.5">
            {fields.map(({ key, value }) => (
              <div key={key} className="grid grid-cols-[7rem_1fr] gap-2">
                <dt className="truncate text-[12px] text-ink-faint" title={key}>
                  {key}
                </dt>
                <dd className="min-w-0 whitespace-pre-wrap break-words font-mono text-[12px] leading-snug text-ink">
                  {value}
                </dd>
              </div>
            ))}
          </dl>
        )}

        {/* The exact bytes, for anyone who wants to check rather than trust the rendering above. */}
        <details className="mt-2">
          <summary className="cursor-pointer text-[11.5px] text-ink-faint hover:text-ink">
            Raw JSON
          </summary>
          <pre className="mt-1 overflow-x-auto rounded bg-overlay p-2 font-mono text-[11.5px] leading-snug text-ink-muted">
            {raw(execution.arguments)}
          </pre>
        </details>

        {execution.result && (
          <div className="mt-2.5">
            <p className="mb-1 text-[11px] font-medium uppercase tracking-wide text-ink-faint">
              {execution.status === "succeeded" ? "Answer" : "What came back"}
            </p>
            <pre
              className={`overflow-x-auto whitespace-pre-wrap rounded p-2 font-mono text-[11.5px]
                          leading-snug ${
                            execution.status === "succeeded"
                              ? "bg-overlay text-ink-muted"
                              : "bg-warn text-warn-text"
                          }`}
            >
              {execution.result}
            </pre>
          </div>
        )}

        {execution.status === "unknown" && (
          <p className="mt-2 flex items-start gap-1.5 text-[12px] leading-relaxed text-warn-text">
            <CircleHelp size={13} className="mt-0.5 shrink-0" aria-hidden />
            <span>
              This did not answer in time, so whether it ran is unknown. Check {serverName} before
              proposing it again — running it twice may not be undoable.
            </span>
          </p>
        )}
      </div>

      {execution.status === "proposed" && (
        <footer className="flex items-center gap-2 border-t border-hairline bg-overlay px-3 py-2">
          <p className="flex-1 text-[11.5px] text-ink-faint">
            Nothing has been sent. It runs only if you approve it.
          </p>
          <button
            type="button"
            onClick={onReject}
            disabled={busy}
            className="flex items-center gap-1 rounded-full border border-hairline px-2.5 py-1
                       text-[11.5px] text-ink-muted transition hover:text-ink disabled:opacity-50"
          >
            <X size={11} aria-hidden />
            Decline
          </button>
          <button
            type="button"
            onClick={onConfirm}
            disabled={busy}
            className="flex items-center gap-1 rounded-full bg-accent px-2.5 py-1 text-[11.5px]
                       text-accent-on transition hover:opacity-90 disabled:opacity-50"
          >
            {busy ? (
              <Loader2 size={11} className="animate-spin" aria-hidden />
            ) : (
              <Check size={11} aria-hidden />
            )}
            Approve and run
          </button>
        </footer>
      )}

      {execution.status === "confirmed" && onSend && (
        <footer className="flex items-center gap-2 border-t border-hairline bg-overlay px-3 py-2">
          <p className="flex-1 text-[11.5px] text-ink-faint">
            Approved, but it never went out.
          </p>
          <button
            type="button"
            onClick={onSend}
            disabled={busy}
            className="flex items-center gap-1 rounded-full bg-accent px-2.5 py-1 text-[11.5px]
                       text-accent-on transition hover:opacity-90 disabled:opacity-50"
          >
            {busy ? (
              <Loader2 size={11} className="animate-spin" aria-hidden />
            ) : (
              <Send size={11} aria-hidden />
            )}
            Send it
          </button>
        </footer>
      )}
    </div>
  );
}

function StatusChip({ execution }: { execution: ToolExecution }) {
  const { status } = execution;

  if (status === "succeeded") {
    return (
      <span className="flex items-center gap-1 rounded-full bg-ok px-2 py-0.5 text-[11px] text-ok-text">
        <Check size={10} aria-hidden />
        Ran
      </span>
    );
  }

  if (status === "failed") {
    return (
      <span className="flex items-center gap-1 rounded-full bg-warn px-2 py-0.5 text-[11px] text-warn-text">
        <AlertTriangle size={10} aria-hidden />
        Failed
      </span>
    );
  }

  // Deliberately not "failed": it may have run.
  if (status === "unknown") {
    return (
      <span className="flex items-center gap-1 rounded-full bg-warn px-2 py-0.5 text-[11px] text-warn-text">
        <CircleHelp size={10} aria-hidden />
        Outcome unknown
      </span>
    );
  }

  const label =
    status === "proposed"
      ? "Waiting for you"
      : status === "confirmed"
        ? "Approved, not sent"
        : "Declined";

  return (
    <span className="rounded-full border border-hairline px-2 py-0.5 text-[11px] text-ink-faint">
      {label}
    </span>
  );
}

/**
 * The arguments as label-and-value pairs.
 *
 * Nested values are stringified rather than flattened: a user checking a call needs to see what is
 * in a nested object, and "[object Object]" is the opposite of a confirmation.
 */
function readArguments(
  args: ToolExecution["arguments"],
): Array<{ key: string; value: string }> {
  if (typeof args === "string") {
    // Only reachable if the stored arguments would not parse, which the engine refuses to store.
    // Shown rather than hidden, because a record of something that ran must stay readable.
    return [{ key: "raw", value: args }];
  }

  return Object.entries(args).map(([key, value]) => ({
    key,
    value:
      typeof value === "string" ? value : JSON.stringify(value, null, 2) ?? String(value),
  }));
}

function raw(args: ToolExecution["arguments"]): string {
  return typeof args === "string" ? args : JSON.stringify(args, null, 2);
}
