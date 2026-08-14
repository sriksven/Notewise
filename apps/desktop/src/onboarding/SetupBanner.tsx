import { AlertCircle, X } from "lucide-react";

import type { Step } from "./readiness";

interface SetupBannerProps {
  regressed: Step[];
  onDismiss: () => void;
}

/**
 * Something setup established has since broken.
 *
 * A banner rather than the wizard: a stopped Ollama or a revoked grant is a small problem,
 * and demoting an established user to a welcome screen over it would be absurd.
 */
export function SetupBanner({ regressed, onDismiss }: SetupBannerProps) {
  if (regressed.length === 0) return null;

  const names = regressed.map((step) => step.title.toLowerCase()).join(" and ");

  return (
    <div
      role="status"
      className="flex items-center gap-2 border-b border-amber-200 bg-amber-50 px-4 py-2 text-[13px] text-amber-900"
    >
      <AlertCircle size={15} className="shrink-0" aria-hidden />
      <span className="flex-1">
        Recording may not work: {names} {regressed.length === 1 ? "needs" : "need"} attention.
        Open Settings to fix it.
      </span>
      <button
        type="button"
        onClick={onDismiss}
        aria-label="Dismiss"
        className="shrink-0 rounded p-0.5 transition hover:bg-amber-100"
      >
        <X size={14} aria-hidden />
      </button>
    </div>
  );
}
