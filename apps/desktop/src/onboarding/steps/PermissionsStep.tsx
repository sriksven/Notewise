import { useEffect, useState } from "react";
import { Mic, Volume2 } from "lucide-react";

import { api, SETTINGS_BY_HAND } from "../../lib/api";
import type { PermissionsReadiness } from "../readiness";
import { PermissionRow } from "./PermissionRow";

interface PermissionsStepProps {
  readiness: PermissionsReadiness;
  onChanged: () => Promise<void>;
}

type Kind = "microphone" | "system_audio";

export function PermissionsStep({ readiness, onChanged }: PermissionsStepProps) {
  const [busy, setBusy] = useState<Kind | null>(null);
  const [error, setError] = useState<string | null>(null);

  const enable = async (kind: Kind) => {
    setBusy(kind);
    setError(null);
    try {
      await api.requestPermission(kind);
      await onChanged();
    } catch {
      setError("Could not ask the system for that permission.");
    } finally {
      setBusy(null);
    }
  };

  // The pane for that row, not the top of Privacy & Security. System audio is granted under
  // Screen & System Audio Recording.
  const openSettings = (kind: Kind) => {
    setError(null);
    api
      .openSystemSettings(kind === "microphone" ? "microphone" : "screen_recording")
      .catch(() => setError(SETTINGS_BY_HAND));
  };

  // Look again when the window comes back: the person has just been in System Settings.
  useEffect(() => {
    const again = () => void onChanged();
    window.addEventListener("focus", again);
    return () => window.removeEventListener("focus", again);
  }, [onChanged]);

  return (
    <div className="flex flex-col items-center text-center">
      <h1 className="text-[26px] font-semibold tracking-tight text-ink">Permissions</h1>
      <p className="mt-2 max-w-md text-[14px] text-ink-muted">
        Notewise needs the operating system's permission to hear a meeting. Nothing is recorded
        until you press record.
      </p>

      {error && (
        <div
          role="alert"
          className="mt-6 w-full max-w-md rounded-lg border border-warn-line bg-warn px-3 py-2 text-left text-[13px] text-warn-text"
        >
          {error}
        </div>
      )}

      <div className="mt-8 w-full max-w-md divide-y divide-hairline overflow-hidden rounded-xl border border-hairline text-left">
        <PermissionRow
          icon={Mic}
          title="Microphone"
          description="Captures your side of the conversation."
          readiness={readiness.microphone}
          busy={busy === "microphone"}
          onEnable={() => void enable("microphone")}
          onOpenSettings={() => openSettings("microphone")}
        />
        <PermissionRow
          icon={Volume2}
          title="System audio"
          description="Captures everyone else, straight from the meeting app."
          readiness={readiness.system_audio}
          busy={busy === "system_audio"}
          onEnable={() => void enable("system_audio")}
          onOpenSettings={() => openSettings("system_audio")}
        />
      </div>
    </div>
  );
}
