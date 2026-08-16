import { useCallback, useEffect, useState } from "react";
import { Fingerprint, Loader2, Trash2 } from "lucide-react";

import { api, ApiError } from "../lib/api";

/**
 * Whether the app may remember what people sound like.
 *
 * Presented as its own decision rather than buried with the audio settings, because it is the
 * only thing the product stores that is about someone other than the user. Everything else —
 * transcripts, notes, tickets — is their own material; a voice print identifies the colleague
 * who was in the room and never installed anything.
 *
 * Off by default, which is also what the comparable local tools do.
 */
export function VoiceprintSettings() {
  const [enabled, setEnabled] = useState(false);
  const [stored, setStored] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const status = await api.voiceprints();
      setEnabled(status.enabled);
      setStored(status.stored);
    } catch {
      // A status that will not load is not worth a banner over the settings screen; the
      // controls below simply stay as they are.
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const toggle = async (next: boolean) => {
    setBusy(true);
    setError(null);
    try {
      const status = await api.setVoiceprintsEnabled(next);
      setEnabled(status.enabled);
      setStored(status.stored);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not change the setting.");
    } finally {
      setBusy(false);
    }
  };

  const forget = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.forgetVoiceprints();
      await load();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not erase them.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <section>
      <h2 className="mb-1 flex items-center gap-1.5 text-[13px] font-semibold text-ink">
        <Fingerprint size={14} className="text-ink-faint" aria-hidden />
        Recognise people by voice
      </h2>
      <p className="mb-3 text-[12px] leading-relaxed text-ink-muted">
        Lets Notewise name the same colleague across meetings, including in-person ones where no
        meeting app can be asked who was speaking. It stores a numeric fingerprint of each
        voice — never audio, and nothing that can be played back.
      </p>

      <div className="card overflow-hidden">
        <label className="flex cursor-pointer items-start gap-3 px-4 py-3">
          <input
            type="checkbox"
            checked={enabled}
            disabled={busy}
            onChange={(event) => void toggle(event.target.checked)}
            className="mt-0.5 h-4 w-4 shrink-0 accent-[var(--accent)]"
          />
          <span className="min-w-0 flex-1">
            <span className="block text-[13px] font-medium text-ink">
              Remember voices between meetings
            </span>
            {/* The part that is easy to leave unsaid: the people identified are not the user. */}
            <span className="mt-0.5 block text-[12px] leading-relaxed text-ink-muted">
              This identifies the other people in your meetings, who have not been asked. It stays
              on this machine and is never uploaded. Off by default.
            </span>
          </span>
          {busy && <Loader2 size={14} className="mt-0.5 animate-spin text-ink-faint" aria-hidden />}
        </label>

        <div className="flex items-center gap-3 border-t border-hairline bg-overlay px-4 py-2.5">
          <span className="flex-1 text-[12px] text-ink-muted">
            {stored === 0
              ? "No voices stored."
              : `${stored} ${stored === 1 ? "voice" : "voices"} stored.`}
          </span>
          <button
            type="button"
            onClick={() => void forget()}
            disabled={busy || stored === 0}
            className="flex shrink-0 items-center gap-1 rounded-full border border-hairline px-2.5 py-1
                       text-[12px] text-ink-muted transition hover:bg-surface hover:text-ink
                       disabled:cursor-not-allowed disabled:opacity-40"
          >
            <Trash2 size={12} aria-hidden />
            Forget all
          </button>
        </div>
      </div>

      <p className="mt-2 text-[11px] leading-snug text-ink-faint">
        Switching this off erases what is already stored, not just what would be collected next.
        The people and their names are kept.
      </p>

      {error && (
        <p role="alert" className="mt-2 text-[12px] text-danger-text">
          {error}
        </p>
      )}
    </section>
  );
}
