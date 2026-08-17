import { useEffect, useRef, useState } from "react";

import { api, ApiError, type DownloadState } from "./api";

export interface ModelDownload {
  /** The model currently downloading, or null. */
  downloading: string | null;
  progress: DownloadState | null;
  error: string | null;
  start: (name: string) => Promise<void>;
}

/**
 * Start and follow a model download.
 *
 * The engine owns the download, so this hook is only a view onto it: it re-attaches to one
 * already running when it mounts, which is what lets a user leave the screen and come back
 * without losing the progress bar.
 */
export function useModelDownload(onFinished: () => void | Promise<void>): ModelDownload {
  const [downloading, setDownloading] = useState<string | null>(null);
  const [progress, setProgress] = useState<DownloadState | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Held in a ref so the mount effect below can call the latest callback without listing it
  // as a dependency — depending on it would re-run the effect on every parent render and
  // open a second EventSource onto the same download.
  const finished = useRef(onFinished);
  finished.current = onFinished;

  const start = async (name: string) => {
    setDownloading(name);
    setProgress(null);
    setError(null);

    const done = async () => {
      setDownloading(null);
      setProgress(null);
      await finished.current();
    };

    const fail = (message: string) => {
      setError(message);
      setDownloading(null);
      setProgress(null);
    };

    try {
      const started = await api.downloadModel(name);

      // Already on disk: the POST answers `done` and there is nothing to stream.
      if (started.status === "done") {
        await done();
        return;
      }

      setProgress(started);
      api.watchDownload(name, setProgress, () => void done(), fail);
    } catch (e) {
      fail(e instanceof ApiError ? e.message : "Download failed.");
    }
  };

  // Recover a download already running when this mounted — the engine owns it, so navigating
  // away and back must not lose the progress bar.
  useEffect(() => {
    let cancel: (() => void) | undefined;

    void api
      .downloads()
      .then((states) => {
        // Transcription models only. The engine tracks speaker-model downloads in the same
        // registry, and `watchDownload` streams from `/v1/models/:name/download` — which does not
        // know those names and answered 400 for each one.
        const running = states.find(
          (s) => s.status === "downloading" && s.kind === "transcription",
        );
        if (!running) return;

        setDownloading(running.model);
        setProgress(running);

        cancel = api.watchDownload(
          running.model,
          setProgress,
          () => {
            setDownloading(null);
            setProgress(null);
            void finished.current();
          },
          (message) => {
            setError(message);
            setDownloading(null);
            setProgress(null);
          },
        );
      })
      .catch(() => {
        // A failed poll is not a failed download. Staying silent leaves the user with a
        // Download button rather than an error about something they did not ask for.
      });

    return () => cancel?.();
  }, []);

  return { downloading, progress, error, start };
}
