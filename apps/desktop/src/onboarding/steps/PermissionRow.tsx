import { Check, Loader2, MinusCircle, type LucideIcon } from "lucide-react";

import type { PermissionReadiness } from "../readiness";

interface PermissionRowProps {
  icon: LucideIcon;
  title: string;
  description: string;
  readiness: PermissionReadiness;
  busy: boolean;
  onEnable: () => void;
  onOpenSettings: () => void;
}

export function PermissionRow({
  icon: Icon,
  title,
  description,
  readiness,
  busy,
  onEnable,
  onOpenSettings,
}: PermissionRowProps) {
  const { status, detail } = readiness;

  return (
    <div className="flex items-start gap-3 bg-surface px-4 py-3.5">
      <span className="mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-overlay">
        <Icon size={16} className="text-ink-muted" />
      </span>

      <div className="min-w-0 flex-1">
        <div className="text-[14px] font-medium text-ink">{title}</div>
        <p className="mt-0.5 text-[12px] text-ink-muted">
          {status === "unavailable" ? detail : description}
        </p>
        {status === "denied" && (
          // The engine's explanation when it has one. A refusal is not always a decline —
          // macOS reports the same "no" when it declined to register the app at all, and the
          // generic line sends that user to a list their app is missing from.
          <p className="mt-1 text-[12px] leading-snug text-warn-text">
            {detail ?? "Declined. Grant it in System Settings, then re-check."}
          </p>
        )}
      </div>

      {status === "granted" && (
        <span className="flex shrink-0 items-center gap-1 text-[12px] text-ok-text">
          <Check size={14} aria-hidden />
          Granted
        </span>
      )}

      {/* Not blocking, and says so. There is no action behind an unavailable capability, so
          offering a button would only invite someone to keep pressing it. */}
      {status === "unavailable" && (
        <span className="flex shrink-0 items-center gap-1 text-[12px] text-ink-faint">
          <MinusCircle size={14} aria-hidden />
          Not available
        </span>
      )}

      {(status === "not_requested" || status === "denied") && (
        <div className="flex shrink-0 gap-1.5">
          {status === "denied" && (
            <button
              type="button"
              onClick={onOpenSettings}
              className="rounded-full border border-hairline px-3 py-1.5 text-[12px]
                         text-ink transition hover:bg-overlay"
            >
              Open Settings
            </button>
          )}

          <button
            type="button"
            onClick={onEnable}
            disabled={busy}
            className="flex items-center gap-1.5 rounded-full border border-hairline px-3 py-1.5
                       text-[12px] text-ink transition hover:bg-overlay
                       disabled:opacity-50"
          >
            {busy && <Loader2 size={13} className="animate-spin" aria-hidden />}
            {status === "denied" ? "Re-check" : "Enable"}
          </button>
        </div>
      )}
    </div>
  );
}
