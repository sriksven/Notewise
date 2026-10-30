import { useCallback, useEffect, useState, type ReactNode } from "react";
import { Loader2 } from "lucide-react";

import { api } from "../lib/api";
import { bannerProblems, type SetupReadiness } from "./readiness";
import { SetupBanner } from "./SetupBanner";
import { SetupFlow } from "./SetupFlow";

/**
 * Asks the app to show its settings screen.
 *
 * A window event because the banner lives outside the app's own tree — the gate wraps it — and
 * lifting the whole navigation state into the gate to let one button work would put routing in
 * the component whose only job is deciding between the wizard and the app.
 */
export const OPEN_SETTINGS_EVENT = "notewise:open-settings";

interface SetupGateProps {
  children: ReactNode;
}

/**
 * Decides whether this launch shows the wizard or the app.
 *
 * Readiness comes from the engine, never from browser storage: the shell binds port 0, so the
 * window's origin changes every launch and anything kept in `localStorage` would be gone by
 * the next one.
 */
export function SetupGate({ children }: SetupGateProps) {
  const [readiness, setReadiness] = useState<SetupReadiness | null>(null);
  const [loading, setLoading] = useState(true);
  const [dismissed, setDismissed] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setReadiness(await api.setup());
    } catch {
      // An unreachable engine is not an unfinished setup, and blocking the app behind a wizard
      // we cannot populate would strand the user. Let the app load and report the failure
      // through its own error banner.
      setReadiness(null);
    }
  }, []);

  useEffect(() => {
    void refresh().finally(() => setLoading(false));
  }, [refresh]);

  const problems =
    readiness && readiness.completed_at !== null && !dismissed ? bannerProblems(readiness) : [];
  const showing = problems.length > 0;

  // While the banner is up, look again: when the window comes back to the front — which is the
  // moment after someone has gone and opened Ollama — and every 15 seconds. It used to be read
  // once at launch, so it went on saying Ollama was down after it had been started.
  useEffect(() => {
    if (!showing) return;
    const again = () => void refresh();
    window.addEventListener("focus", again);
    const timer = window.setInterval(again, 15_000);
    return () => {
      window.removeEventListener("focus", again);
      window.clearInterval(timer);
    };
  }, [showing, refresh]);

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center bg-overlay">
        <Loader2 size={20} className="animate-spin text-ink-faint" aria-label="Loading" />
      </div>
    );
  }

  if (readiness && readiness.completed_at === null) {
    return <SetupFlow readiness={readiness} refresh={refresh} onFinished={() => void refresh()} />;
  }

  return (
    <div className="flex h-full flex-col">
      <SetupBanner
        problems={problems}
        onOpenSettings={() => window.dispatchEvent(new CustomEvent(OPEN_SETTINGS_EVENT))}
        onDismiss={() => setDismissed(true)}
      />
      <div className="min-h-0 flex-1">{children}</div>
    </div>
  );
}
