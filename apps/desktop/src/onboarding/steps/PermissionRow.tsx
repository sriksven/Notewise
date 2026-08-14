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
    <div className="flex items-start gap-3 bg-white px-4 py-3.5">
      <span className="mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-neutral-100">
        <Icon size={16} className="text-neutral-600" />
      </span>

      <div className="min-w-0 flex-1">
        <div className="text-[14px] font-medium text-neutral-900">{title}</div>
        <p className="mt-0.5 text-[12px] text-neutral-500">
          {status === "unavailable" ? detail : description}
        </p>
        {status === "denied" && (
          <p className="mt-1 text-[12px] text-amber-800">
            Declined. Grant it in System Settings, then re-check.
          </p>
        )}
      </div>

      {status === "granted" && (
        <span className="flex shrink-0 items-center gap-1 text-[12px] text-emerald-600">
          <Check size={14} aria-hidden />
          Granted
        </span>
      )}

      {/* Not blocking, and says so. There is no action behind an unavailable capability, so
          offering a button would only invite someone to keep pressing it. */}
      {status === "unavailable" && (
        <span className="flex shrink-0 items-center gap-1 text-[12px] text-neutral-400">
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
                         text-neutral-700 transition hover:bg-neutral-50"
            >
              Open Settings
            </button>
          )}

          <button
            type="button"
            onClick={onEnable}
            disabled={busy}
            className="flex items-center gap-1.5 rounded-full border border-hairline px-3 py-1.5
                       text-[12px] text-neutral-700 transition hover:bg-neutral-50
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
