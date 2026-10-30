import { AlertCircle, X } from "lucide-react";

interface SetupBannerProps {
  /**
   * What is not working and what to do about it, as whole sentences — see `bannerProblems`.
   *
   * "permissions needs attention" describes the setup screen's state and tells someone with a
   * meeting starting in a minute nothing. "Ollama isn't running. Open the Ollama app…" tells
   * them what to do.
   */
  problems: string[];
  onOpenSettings: () => void;
  onDismiss: () => void;
}

/**
 * Something setup established has since broken, or was skipped.
 *
 * A banner rather than the wizard: a stopped Ollama, a revoked grant or a permission the user
 * deliberately passed on is a small problem, and demoting an established user to a welcome
 * screen over it would be absurd.
 */
export function SetupBanner({ problems, onOpenSettings, onDismiss }: SetupBannerProps) {
  if (problems.length === 0) return null;

  return (
    <div
      role="status"
      className="flex items-center gap-2 border-b border-warn-line bg-warn px-4 py-2 text-[13px] text-warn-text"
    >
      <AlertCircle size={15} className="shrink-0" aria-hidden />
      <span className="flex-1 leading-snug">{problems.join(" ")}</span>

      {/* A real button. The old banner ended in "Open Settings to fix it" as advice, pointing
          at a screen that had no permissions on it at all. */}
      <button
        type="button"
        onClick={onOpenSettings}
        className="shrink-0 rounded-full border border-warn-line px-2.5 py-0.5 text-[12px]
                   font-medium transition hover:bg-warn"
      >
        Fix in Settings
      </button>

      <button
        type="button"
        onClick={onDismiss}
        aria-label="Dismiss"
        className="shrink-0 rounded p-0.5 transition hover:bg-warn"
      >
        <X size={14} aria-hidden />
      </button>
    </div>
  );
}
