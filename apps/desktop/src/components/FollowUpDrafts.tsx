import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, Check, Copy, Mail, Trash2 } from "lucide-react";

import { api, ApiError, type EmailDraft } from "../lib/api";

/** Tones the engine can draft. Named the way `ai_router::EmailTone` parses them. */
const TONES = ["concise", "detailed", "formal", "friendly"] as const;

interface Props {
  meetingId: string;
  /** Drafting from nothing produces invented commitments, so it is not offered. */
  hasSource: boolean;
}

/**
 * Follow-up emails for a meeting.
 *
 * # Nothing here sends
 *
 * The engine has no send endpoint and neither does this. A draft can be generated, read, copied,
 * approved and discarded — approving records that a human read it, and moves no mail. The only way
 * one becomes an outgoing message is the user pasting it into their own mail client, deliberately.
 *
 * That is not caution for its own sake. A wrong auto-send reaches other people, cannot be recalled,
 * and the user finds out from the recipient. Every other artifact this app produces stays on the
 * machine; this is the one that would not, so it gets the friction.
 *
 * # Why the whole body is on screen
 *
 * A collapsed preview invites approving something unread, which defeats the point of approval. The
 * draft is shown in full, in a monospace block, because it is about to be pasted somewhere it
 * cannot be taken back from.
 */
export function FollowUpDrafts({ meetingId, hasSource }: Props) {
  const [drafts, setDrafts] = useState<EmailDraft[]>([]);
  const [tone, setTone] = useState<string>("concise");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setDrafts(await api.emailDrafts(meetingId));
    } catch {
      // A draft list that will not load is not worth a banner over the summary.
    }
  }, [meetingId]);

  useEffect(() => {
    void load();
  }, [load]);

  async function draft() {
    setBusy(true);
    setError(null);
    try {
      const made = await api.draftEmails(meetingId, [tone]);
      setDrafts((prev) => [...made, ...prev]);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "could not draft that");
    } finally {
      setBusy(false);
    }
  }

  async function approve(id: string) {
    setError(null);
    try {
      const updated = await api.approveEmailDraft(id);
      setDrafts((prev) => prev.map((d) => (d.id === id ? updated : d)));
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "could not approve that");
    }
  }

  async function discard(id: string) {
    setError(null);
    try {
      await api.discardEmailDraft(id);
      setDrafts((prev) => prev.filter((d) => d.id !== id));
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "could not discard that");
    }
  }

  async function copy(draft: EmailDraft) {
    try {
      await navigator.clipboard.writeText(`Subject: ${draft.subject}\n\n${draft.body}`);
      setCopied(draft.id);
      setTimeout(() => setCopied(null), 1500);
    } catch {
      setError("could not reach the clipboard");
    }
  }

  const live = drafts.filter((d) => d.status !== "discarded");

  return (
    <section className="mt-8 border-t border-hairline pt-6">
      <h3 className="mb-1 flex items-center gap-2 text-[13px] font-semibold text-ink">
        <Mail className="h-3.5 w-3.5" /> Follow-up email
      </h3>
      <p className="mb-3 max-w-2xl text-[12.5px] leading-relaxed text-ink-muted">
        Drafted from this meeting's summary. Nothing is sent — copy it into your own mail client when
        you are happy with it.
      </p>

      {error && (
        <div className="mb-3 flex items-start gap-2 rounded-lg border border-amber-500/30 bg-amber-500/10 p-3 text-[12.5px] text-amber-200">
          <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          <span>{error}</span>
        </div>
      )}

      {hasSource ? (
        <div className="flex flex-wrap items-center gap-2">
          {TONES.map((t) => (
            <button
              key={t}
              type="button"
              onClick={() => setTone(t)}
              className={`rounded-md px-2 py-1 text-[11.5px] ${
                tone === t ? "bg-accent/20 text-accent-soft" : "text-ink-muted hover:text-ink"
              }`}
            >
              {t}
            </button>
          ))}
          <button
            type="button"
            disabled={busy}
            onClick={() => void draft()}
            className="btn-ghost px-3 py-1.5 text-[12px] disabled:opacity-50"
          >
            {busy ? "Drafting…" : "Draft it"}
          </button>
        </div>
      ) : (
        <p className="text-[12.5px] text-ink-faint">
          Summarize the meeting first — drafting from an empty meeting invents commitments.
        </p>
      )}

      <ul className="mt-4 space-y-3">
        {live.map((d) => (
          <li key={d.id} className="rounded-lg border border-hairline p-3">
            <div className="flex items-start justify-between gap-3">
              <div>
                <p className="text-[13px] font-medium text-ink">{d.subject}</p>
                <p className="mt-0.5 text-[11.5px] text-ink-faint">
                  {d.variant ?? "draft"}
                  {d.status === "approved" && " · approved, not sent"}
                </p>
              </div>
              <div className="flex shrink-0 items-center gap-1">
                <button
                  type="button"
                  onClick={() => void copy(d)}
                  aria-label="Copy draft"
                  className="btn-ghost px-2 py-1 text-[12px]"
                >
                  {copied === d.id ? (
                    <Check className="h-3.5 w-3.5" />
                  ) : (
                    <Copy className="h-3.5 w-3.5" />
                  )}
                </button>
                {d.status !== "approved" && (
                  <button
                    type="button"
                    onClick={() => void approve(d.id)}
                    className="btn-ghost px-2 py-1 text-[12px]"
                  >
                    Approve
                  </button>
                )}
                <button
                  type="button"
                  onClick={() => void discard(d.id)}
                  aria-label="Discard draft"
                  className="btn-ghost px-2 py-1 text-[12px]"
                >
                  <Trash2 className="h-3.5 w-3.5" />
                </button>
              </div>
            </div>
            {/* In full, on purpose. A collapsed preview invites approving something unread. */}
            <pre className="mt-2 whitespace-pre-wrap font-mono text-[12px] leading-relaxed text-ink-muted">
              {d.body}
            </pre>
          </li>
        ))}
      </ul>
    </section>
  );
}
