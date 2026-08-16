import { AlertCircle, X } from "lucide-react";

interface SetupBannerProps {
  /**
   * What is not working, in the user's terms — not the names of wizard steps.
   *
   * "permissions needs attention" describes the setup screen's state and tells someone with a
   * meeting starting in a minute nothing. "Notewise will not be able to hear a meeting" tells
   * them what will happen.
   */
  consequences: string[];
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
export function SetupBanner({ consequences, onOpenSettings, onDismiss }: SetupBannerProps) {
  if (consequences.length === 0) return null;

  // Capitalised here rather than in the source strings, which are also used mid-sentence in
  // the setup wizard.
  const [first, ...rest] = consequences;
  const sentence = [first.charAt(0).toUpperCase() + first.slice(1), ...rest].join(", and ");

  return (
    <div
      role="status"
      className="flex items-center gap-2 border-b border-warn-line bg-warn px-4 py-2 text-[13px] text-warn-text"
    >
      <AlertCircle size={15} className="shrink-0" aria-hidden />
      <span className="flex-1">{sentence}.</span>

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
