import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, Loader2, Mic, Trash2 } from "lucide-react";

import { api, ApiError, type AudioRetention } from "../lib/api";
import { size } from "../lib/format";

/** What the days field falls back to when the option is chosen with nothing typed. */
const DEFAULT_DAYS = 30;

/**
 * Whether the recording is kept after it has been transcribed.
 *
 * # Why this screen has to exist
 *
 * The engine has kept audio, swept it on a schedule, and served it over a range-request endpoint for
 * a while, and the transcript already renders a player when a meeting has audio to play. None of it
 * could be switched on: the setting was reachable over HTTP and from nowhere in the app. A privacy
 * control that only an HTTP client can find is not a privacy control, and the feature it gates might
 * as well not have shipped.
 *
 * # Off is the default and stays the default
 *
 * A transcript is text about a meeting. A recording is the meeting — every aside, every voice, and
 * everyone present whether or not they knew a laptop was keeping it. That is a different promise, so
 * nothing here is opt-out.
 *
 * # Turning it off deletes
 *
 * `PUT /v1/audio/retention` sweeps immediately when the policy becomes `off`, on the reasoning that
 * a user who says they do not want the recordings should not have them sitting on disk until a timer
 * fires. That is the right behaviour and it is destructive, so this screen says what will go and how
 * much of it before the click, and asks again when there is something to lose.
 */
export function AudioRetentionSettings() {
  const [status, setStatus] = useState<AudioRetention | null>(null);
  const [days, setDays] = useState(DEFAULT_DAYS);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [swept, setSwept] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const next = await api.audioRetention();
      setStatus(next);
      // Reflect a `days:N` already stored, so the field shows the real policy rather than the
      // default the moment this screen opens.
      const stored = /^days:(\d+)$/.exec(next.policy);
      if (stored) setDays(Number(stored[1]));
    } catch {
      // A status that will not load is not worth a banner over the settings screen.
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const apply = async (policy: string) => {
    setBusy(true);
    setError(null);
    setSwept(null);
    try {
      setStatus(await api.setAudioRetention(policy));
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not change the setting.");
      await load();
    } finally {
      setBusy(false);
    }
  };

  /**
   * Choosing "off" throws recordings away, so it is confirmed — but only when there are any.
   *
   * Confirming a no-op teaches people to click through dialogs, which is how the one that mattered
   * gets clicked through too.
   */
  const turnOff = () => {
    const kept = status?.retained ?? 0;
    if (kept > 0) {
      const ok = window.confirm(
        `Delete the audio kept for ${kept} meeting${kept === 1 ? "" : "s"} (${size(
          status?.bytes ?? 0,
        )})?\n\nThe transcripts stay. The recordings cannot be recovered.`,
      );
      if (!ok) return;
    }
    void apply("off");
  };

  const sweepNow = async () => {
    setBusy(true);
    setError(null);
    try {
      const report = await api.sweepAudio();
      setSwept(
        report.deleted === 0
          ? "Nothing was past the cutoff."
          : `Deleted ${report.deleted} recording${report.deleted === 1 ? "" : "s"}, freeing ${size(
              report.bytes_freed,
            )}.`,
      );
      await load();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not delete the expired recordings.");
    } finally {
      setBusy(false);
    }
  };

  const policy = status?.policy ?? "off";
  const isOff = policy === "off";
  const isForever = policy === "until_deleted";
  const isDays = policy.startsWith("days:");
  // An encrypted workspace refuses retention outright: the audio would be written unencrypted
  // beside it, which is a worse promise than not keeping it at all.
  const locked = status !== null && !status.can_enable;

  return (
    <section>
      <h2 className="mb-1 flex items-center gap-1.5 text-[13px] font-semibold text-ink">
        <Mic size={14} className="text-ink-faint" aria-hidden />
        Keep the recording
      </h2>
      <p className="mb-3 text-[12px] leading-relaxed text-ink-muted">
        Transcripts are always kept. This is about the audio they were made from — keeping it lets
        you click any line of a transcript to hear it, which is the only way to check a word the
        transcriber got wrong. It is also a recording of everyone who was in the room.
      </p>

      {locked && (
        <p className="mb-3 flex items-start gap-2 rounded-lg border border-warn-line bg-warn px-3 py-2 text-[12px] leading-relaxed text-warn-text">
          <AlertTriangle size={13} className="mt-0.5 shrink-0" aria-hidden />
          {status?.blocked_by}
        </p>
      )}

      <div className="card divide-y divide-hairline overflow-hidden">
        <label className="flex cursor-pointer items-start gap-3 px-4 py-3">
          <input
            type="radio"
            name="audio-retention"
            checked={isOff}
            disabled={busy || !status}
            onChange={turnOff}
            className="mt-0.5 h-3.5 w-3.5 shrink-0 accent-[var(--accent)]"
          />
          <span className="min-w-0 flex-1">
            <span className="block text-[13px] font-medium text-ink">
              Delete it after transcribing
            </span>
            <span className="mt-0.5 block text-[12px] leading-relaxed text-ink-muted">
              The default. Nothing to find on disk, and nothing to leak. You lose click-to-play on
              transcripts.
            </span>
          </span>
        </label>

        <label
          className={`flex items-start gap-3 px-4 py-3 ${
            locked ? "cursor-not-allowed opacity-50" : "cursor-pointer"
          }`}
        >
          <input
            type="radio"
            name="audio-retention"
            checked={isDays}
            disabled={busy || locked || !status}
            onChange={() => void apply(`days:${days}`)}
            className="mt-0.5 h-3.5 w-3.5 shrink-0 accent-[var(--accent)]"
          />
          <span className="min-w-0 flex-1">
            <span className="block text-[13px] font-medium text-ink">
              Keep it for
              <input
                type="number"
                min={1}
                max={3650}
                value={days}
                disabled={busy || locked}
                onChange={(event) => {
                  const next = Number(event.target.value);
                  setDays(next);
                  // Only writes through when this is already the active choice. Typing in the box
                  // is not a request to switch away from "off".
                  if (isDays && next >= 1) void apply(`days:${next}`);
                }}
                aria-label="Days to keep recordings"
                className="mx-1.5 w-16 rounded border border-hairline bg-surface px-1.5 py-0.5
                           text-[13px] tabular-nums text-ink disabled:opacity-50"
              />
              days
            </span>
            <span className="mt-0.5 block text-[12px] leading-relaxed text-ink-muted">
              Long enough to go back over a meeting, short enough that a laptop is not carrying a
              year of them. Older recordings are deleted on their own; the transcripts stay.
            </span>
          </span>
        </label>

        <label
          className={`flex items-start gap-3 px-4 py-3 ${
            locked ? "cursor-not-allowed opacity-50" : "cursor-pointer"
          }`}
        >
          <input
            type="radio"
            name="audio-retention"
            checked={isForever}
            disabled={busy || locked || !status}
            onChange={() => void apply("until_deleted")}
            className="mt-0.5 h-3.5 w-3.5 shrink-0 accent-[var(--accent)]"
          />
          <span className="min-w-0 flex-1">
            <span className="block text-[13px] font-medium text-ink">
              Keep it until I delete the meeting
            </span>
            <span className="mt-0.5 block text-[12px] leading-relaxed text-ink-muted">
              Nothing expires. Deleting a meeting deletes its audio with it — that is the only thing
              that removes one.
            </span>
          </span>
        </label>

        {/* What is actually on disk. A policy without a number beside it is a promise; this is the
            state. It also answers the question the policy cannot: whether anything was kept before
            the setting was last changed. */}
        <div className="flex items-center gap-2 bg-overlay px-4 py-2.5">
          {busy && <Loader2 size={12} className="shrink-0 animate-spin text-ink-faint" aria-hidden />}
          <span className="text-[12px] text-ink-muted">
            {!status
              ? "Checking…"
              : status.retained === 0
                ? "No recordings kept."
                : `${status.retained} meeting${status.retained === 1 ? "" : "s"} with audio, ${size(
                    status.bytes,
                  )}.`}
          </span>

          {/* Only under a day-based policy. `until_deleted` has nothing to expire, and `off` has
              already swept — a button that always reports "nothing to do" is noise. */}
          {isDays && (status?.retained ?? 0) > 0 && (
            <button
              type="button"
              onClick={() => void sweepNow()}
              disabled={busy}
              className="ml-auto flex shrink-0 items-center gap-1 rounded-full border border-hairline
                         px-2.5 py-1 text-[12px] text-ink-muted transition hover:bg-surface
                         hover:text-ink disabled:cursor-not-allowed disabled:opacity-40"
            >
              <Trash2 size={12} aria-hidden />
              Delete expired now
            </button>
          )}
        </div>
      </div>

      {swept && <p className="mt-2 text-[12px] text-ink-muted">{swept}</p>}

      {error && (
        <p role="alert" className="mt-2 text-[12px] text-danger-text">
          {error}
        </p>
      )}
    </section>
  );
}
