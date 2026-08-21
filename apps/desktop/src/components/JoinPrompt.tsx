import { useCallback, useEffect, useState } from "react";
import { Loader2, Radio, X } from "lucide-react";

import { api, ApiError, type JoinOffer } from "../lib/api";

interface Props {
  /** Whether this build can record at all. A prompt that cannot be taken up is worse than none. */
  canRecord: boolean;
  /** Called with the meeting id once a recording has started. */
  onStarted: (meetingId: string) => void;
}

/** How often to ask whether a meeting started. */
const POLL_MS = 10_000;

/**
 * "A meeting seems to have started. Record it?"
 *
 * # Why this exists next to the OS notification and not instead of it
 *
 * The notification is what reaches somebody looking at their meeting rather than at Notewise. But a
 * notification cannot be taken up if it was missed, dismissed, or never permitted — and notification
 * permission is an OS grant a user may well have refused. A banner in the window costs nothing and
 * catches every case the notification does not.
 *
 * When both would fire, the notification for an offer shown here is marked delivered, so nobody is
 * told the same thing twice.
 *
 * # Why it never starts recording on its own
 *
 * Detection is a guess. A false positive here is a banner nobody wanted; if it recorded instead, a
 * false positive would be audio of other people captured because software guessed a meeting began.
 * Those are not the same kind of mistake, and the asymmetry is what decides it.
 */
export function JoinPrompt({ canRecord, onStarted }: Props) {
  const [offers, setOffers] = useState<JoinOffer[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const found = await api.joinOffers();
      setOffers(found);

      // Shown here, so the OS notification for it is answered. Best effort: failing to mark it
      // costs a duplicate, not the offer.
      for (const offer of found) {
        if (offer.notification_id) {
          void api.markNotificationDelivered(offer.notification_id).catch(() => {});
        }
      }
    } catch {
      // An engine that cannot answer is not worth a banner about banners.
    }
  }, []);

  useEffect(() => {
    void load();
    const timer = window.setInterval(() => void load(), POLL_MS);
    return () => window.clearInterval(timer);
  }, [load]);

  const dismiss = async (offer: JoinOffer) => {
    setOffers((current) => current.filter((o) => o.id !== offer.id));
    await api.dismissJoinOffer(offer.id).catch(() => {});
  };

  const record = async (offer: JoinOffer) => {
    setBusy(offer.id);
    setError(null);
    try {
      // Two steps on purpose: the engine says what to call it, and the ordinary recording endpoint
      // starts it. One path into the microphone, with its device and model errors already handled.
      const accepted = await api.acceptJoinOffer(offer.id);
      const started = await api.startRecording({ title: accepted.title });

      setOffers((current) => current.filter((o) => o.id !== offer.id));
      if (started.meeting_id) onStarted(started.meeting_id);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not start recording.");
      // The offer stays: whatever went wrong, the meeting is still happening.
    } finally {
      setBusy(null);
    }
  };

  if (offers.length === 0) return null;

  return (
    <div className="border-b border-hairline bg-overlay">
      {error && (
        <p role="alert" className="px-8 py-1.5 text-[12px] text-warn-text">
          {error}
        </p>
      )}

      {offers.map((offer) => (
        <div key={offer.id} className="flex items-center gap-3 px-8 py-2">
          <Radio size={14} className="shrink-0 text-accent" aria-hidden />

          <p className="min-w-0 flex-1 truncate text-[12.5px] text-ink">
            <span className="font-medium">{offer.title}</span>
            <span className="text-ink-faint"> seems to have started.</span>
          </p>

          {canRecord ? (
            <button
              type="button"
              onClick={() => void record(offer)}
              disabled={busy === offer.id}
              className="flex shrink-0 items-center gap-1 rounded-full bg-accent px-2.5 py-1
                         text-[11.5px] text-accent-on transition hover:opacity-90 disabled:opacity-50"
            >
              {busy === offer.id && <Loader2 size={11} className="animate-spin" aria-hidden />}
              Record it
            </button>
          ) : (
            <span className="shrink-0 text-[11.5px] text-ink-faint">
              This build cannot record
            </span>
          )}

          <button
            type="button"
            onClick={() => void dismiss(offer)}
            aria-label={`Dismiss ${offer.title}`}
            className="shrink-0 rounded p-1 text-ink-faint transition hover:text-ink"
          >
            <X size={12} aria-hidden />
          </button>
        </div>
      ))}
    </div>
  );
}
