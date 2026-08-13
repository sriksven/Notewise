import { Globe, Mic, Settings2, ChevronRight, ShieldCheck, ShieldAlert } from "lucide-react";
import type { Health } from "../lib/api";

interface Props {
  health: Health | null;
  onTogglePanel: () => void;
  panelOpen: boolean;
}

/**
 * The three configuration pills, plus the panel toggle.
 *
 * These sit above the content rather than in a settings screen because model,
 * input device, and language are decisions a user revisits per meeting — burying
 * them costs more than the header space.
 */
export function TopBar({ health, onTogglePanel, panelOpen }: Props) {
  return (
    <header className="chrome relative flex h-14 shrink-0 items-center justify-center border-b border-hairline px-3">
      <button
        type="button"
        onClick={onTogglePanel}
        aria-label={panelOpen ? "Hide meeting list" : "Show meeting list"}
        aria-expanded={panelOpen}
        title={panelOpen ? "Hide meeting list" : "Show meeting list"}
        className="absolute left-3 flex h-7 w-7 items-center justify-center rounded-full border border-hairline
                   bg-white text-neutral-500 transition hover:bg-neutral-50 hover:text-neutral-900"
      >
        <ChevronRight
          size={15}
          className={`transition-transform duration-200 ${panelOpen ? "rotate-180" : ""}`}
          aria-hidden
        />
      </button>

      <div className="flex items-center gap-2">
        <button type="button" className="pill">
          <Settings2 size={14} aria-hidden />
          {/* Falls back to a label rather than an empty pill before health loads. */}
          {health?.ai_model ?? "Model"}
        </button>

        <button type="button" className="pill">
          <Mic size={14} aria-hidden />
          Devices
        </button>

        <button type="button" className="pill">
          <Globe size={14} aria-hidden />
          Language
        </button>
      </div>

      {/* Where a user's audio goes is the product's central claim, so it is stated
          in the chrome rather than left in a settings screen to be trusted. */}
      {health && (
        <div
          className="absolute right-3 flex items-center gap-1.5 text-[12px] text-neutral-500"
          title={
            health.ai_local
              ? "Transcripts are processed on this machine"
              : `Transcripts are sent to ${health.ai_model}`
          }
        >
          {health.ai_local ? (
            <ShieldCheck size={14} className="text-emerald-600" aria-hidden />
          ) : (
            <ShieldAlert size={14} className="text-amber-600" aria-hidden />
          )}
          <span className="hidden sm:inline">
            {health.ai_local ? "Local" : "Cloud"}
          </span>
        </div>
      )}
    </header>
  );
}
