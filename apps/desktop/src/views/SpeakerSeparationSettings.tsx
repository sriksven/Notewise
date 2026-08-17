import { useCallback, useEffect, useState } from "react";
import { Check, Download, Loader2, Trash2, Users } from "lucide-react";

import { api, ApiError, type DiarizationStatus, type SpeakerModel } from "../lib/api";

/** How often to re-check while a model is downloading. */
const POLL_MS = 1000;

/**
 * Whether the app should try to work out who was speaking from the audio.
 *
 * # Why this is a setting and not a default
 *
 * Clustering voice embeddings is the only way to answer "who spoke" for a mono recording with no
 * meeting app to ask — an imported file, or one microphone in a room. It is also a guess, and how
 * accurate it is on real meetings is not something this project has measured. A wrong name on a
 * quote is worse than an honest `Speaker 1`, so it is opt-in and says what it is.
 *
 * # Three things must be true
 *
 * The build needs the feature, the model needs downloading, and the setting needs turning on. The
 * engine reports each separately and this screen shows whichever is missing — "it's on and nothing
 * happens" is the state worth designing against.
 */
export function SpeakerSeparationSettings() {
  const [status, setStatus] = useState<DiarizationStatus | null>(null);
  const [models, setModels] = useState<SpeakerModel[]>([]);
  const [busy, setBusy] = useState(false);
  const [downloading, setDownloading] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const [next, catalogue] = await Promise.all([api.diarization(), api.speakerModels()]);
      setStatus(next);
      setModels(catalogue.models);
      return catalogue.models;
    } catch {
      // A status that will not load is not worth a banner over the settings screen.
      return [];
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // Poll only while something is downloading. The engine streams progress over SSE for Whisper
  // models; these are ~29 MB and finish in seconds, so a poll that stops on its own is less
  // machinery for the same outcome.
  useEffect(() => {
    if (!downloading) return;
    const id = setInterval(async () => {
      const next = await load();
      if (next.some((m) => m.name === downloading && m.installed)) setDownloading(null);
    }, POLL_MS);
    return () => clearInterval(id);
  }, [downloading, load]);

  const act = async (run: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await run();
      await load();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not change the setting.");
    } finally {
      setBusy(false);
    }
  };

  const enabled = status?.mode === "acoustic";

  return (
    <section>
      <h2 className="mb-1 flex items-center gap-1.5 text-[13px] font-semibold text-ink">
        <Users size={14} className="text-ink-faint" aria-hidden />
        Separate speakers by voice
      </h2>
      <p className="mb-3 text-[12px] leading-relaxed text-ink-muted">
        For recordings where nothing can be asked who was talking — an imported file, or one
        microphone in a room. It groups the audio into distinct voices and labels them{" "}
        <span className="font-medium text-ink">Speaker 1</span>,{" "}
        <span className="font-medium text-ink">Speaker 2</span>, and so on. Naming them is still
        yours to do: click any speaker in a transcript.
      </p>

      <div className="card overflow-hidden">
        <label className="flex cursor-pointer items-start gap-3 px-4 py-3">
          <input
            type="checkbox"
            checked={enabled}
            disabled={busy || !status}
            onChange={(event) =>
              void act(() =>
                api.setDiarization({ mode: event.target.checked ? "acoustic" : "off" }),
              )
            }
            className="mt-0.5 h-4 w-4 shrink-0 accent-[var(--accent)]"
          />
          <span className="min-w-0 flex-1">
            <span className="block text-[13px] font-medium text-ink">
              Work out speakers from the audio
            </span>
            <span className="mt-0.5 block text-[12px] leading-relaxed text-ink-muted">
              Applies to imported recordings. Live calls already know who spoke from which stream
              the audio arrived on, which is exact — this would only make that worse. Off by
              default.
            </span>
          </span>
          {busy && <Loader2 size={14} className="mt-0.5 animate-spin text-ink-faint" aria-hidden />}
        </label>

        {/* The honest state line. Never "on" without saying whether it will actually do anything. */}
        <div className="flex items-center gap-2 border-t border-hairline bg-overlay px-4 py-2.5">
          {status?.effective ? (
            <>
              <Check size={12} className="shrink-0 text-ink-muted" aria-hidden />
              <span className="text-[12px] text-ink-muted">
                Active. The next import will be separated into voices.
              </span>
            </>
          ) : (
            <span className="text-[12px] text-ink-muted">
              {status?.blocked_by ?? "Checking…"}
            </span>
          )}
        </div>
      </div>

      {/* Only worth showing once someone has expressed the intent. */}
      {enabled && (
        <div className="mt-3">
          <p className="mb-2 text-[12px] font-medium text-ink">Voice model</p>

          {status && !status.supported && (
            <p className="mb-2 rounded-lg border border-hairline bg-overlay px-3 py-2 text-[12px] leading-relaxed text-ink-muted">
              This build was compiled without acoustic separation, so a downloaded model will not
              run. Rebuild the engine with the <code>speaker-diarization</code> feature.
            </p>
          )}

          <div className="card divide-y divide-hairline overflow-hidden">
            {models.map((model) => (
              <div key={model.name} className="flex items-start gap-3 px-4 py-3">
                <input
                  type="radio"
                  name="speaker-model"
                  checked={model.selected}
                  disabled={busy}
                  onChange={() => void act(() => api.setDiarization({ model: model.name }))}
                  className="mt-0.5 h-3.5 w-3.5 shrink-0 accent-[var(--accent)]"
                  aria-label={`Use ${model.name}`}
                />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="text-[13px] font-medium text-ink">{model.name}</span>
                    {model.recommended && (
                      <span className="rounded-full bg-overlay px-1.5 py-0.5 text-[10.5px] text-ink-muted">
                        Recommended
                      </span>
                    )}
                  </div>
                  {/* Three names and three sizes is a quiz, not a choice. */}
                  <p className="mt-0.5 text-[12px] leading-relaxed text-ink-muted">
                    {model.tradeoff}
                  </p>
                  <p className="mt-0.5 text-[11px] text-ink-faint">{model.approx_mb} MB</p>
                </div>

                {model.installed ? (
                  <button
                    type="button"
                    onClick={() => void act(() => api.removeSpeakerModel(model.name))}
                    disabled={busy}
                    className="flex shrink-0 items-center gap-1 rounded-full border border-hairline px-2.5 py-1
                               text-[12px] text-ink-muted transition hover:bg-surface hover:text-ink
                               disabled:cursor-not-allowed disabled:opacity-40"
                  >
                    <Trash2 size={12} aria-hidden />
                    Remove
                  </button>
                ) : (
                  <button
                    type="button"
                    onClick={() => {
                      setDownloading(model.name);
                      void act(() => api.downloadSpeakerModel(model.name));
                    }}
                    disabled={busy || downloading === model.name}
                    className="flex shrink-0 items-center gap-1 rounded-full border border-hairline px-2.5 py-1
                               text-[12px] text-ink-muted transition hover:bg-surface hover:text-ink
                               disabled:cursor-not-allowed disabled:opacity-40"
                  >
                    {downloading === model.name ? (
                      <Loader2 size={12} className="animate-spin" aria-hidden />
                    ) : (
                      <Download size={12} aria-hidden />
                    )}
                    {downloading === model.name ? "Downloading…" : "Download"}
                  </button>
                )}
              </div>
            ))}
          </div>

          <p className="mt-2 text-[11px] leading-snug text-ink-faint">
            Separation is a guess, and how well it works on your recordings has not been measured
            here — expect to merge or rename speakers afterwards. Everything runs on this machine;
            no audio is uploaded.
          </p>
        </div>
      )}

      {error && (
        <p role="alert" className="mt-2 text-[12px] text-danger-text">
          {error}
        </p>
      )}
    </section>
  );
}
