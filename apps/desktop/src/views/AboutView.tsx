import { CheckCircle2, CircleDashed, PenLine } from "lucide-react";
import type { Health } from "../lib/api";

interface Props {
  health: Health | null;
}

/**
 * What works and what does not.
 *
 * Stating the gaps in the app rather than only in a README: a user who discovers that system
 * audio does not work by recording a call and getting half a transcript is worse off than one
 * who was told up front.
 */
function capabilities(health: Health | null): Array<{
  label: string;
  done: boolean;
  note?: string;
}> {
  return [
  // Read from the engine rather than hard-coded: capture is a compile-time feature and also
  // needs a file-backed database, so the same source can produce a build that records and one
  // that cannot. Claiming it works when it does not is exactly the failure this list exists to
  // prevent.
  {
    label: "Microphone recording",
    done: health?.can_record ?? false,
    note: health
      ? health.can_record
        ? undefined
        : "not available in this build"
      : "engine unreachable",
  },
  { label: "Local transcription (Whisper)", done: true, note: "GPU-accelerated on Apple silicon" },
  { label: "Speaker separation", done: true, note: "inferred from pauses, not voices" },
  { label: "Summaries, decisions, action items", done: true },
  { label: "Markdown export", done: true },
  { label: "Full-text search across the workspace", done: true },
  { label: "Agent access over MCP", done: true, note: "read-only" },
  { label: "System audio capture", done: false, note: "needs a signed app and screen-audio permission" },
  { label: "Notes editor and tickets", done: false },
  { label: "Cloud sync", done: false, note: "opt-in, a later phase" },
  ];
}

export function AboutView({ health }: Props) {
  const CAPABILITIES = capabilities(health);

  return (
    <div className="flex-1 overflow-y-auto px-8 py-6">
      <div className="mx-auto max-w-2xl space-y-7">
        <header className="flex items-center gap-3">
          <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-record/10 text-record">
            <PenLine size={20} strokeWidth={2.2} aria-hidden />
          </div>
          <div>
            <h1 className="text-[20px] font-semibold tracking-tight">Notewise</h1>
            <p className="text-[12px] text-neutral-500">
              Local-first meeting intelligence
            </p>
          </div>
        </header>

        <section className="rounded-lg border border-hairline bg-rail px-4 py-3">
          <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-1.5 text-[12px]">
            <dt className="text-neutral-500">Engine</dt>
            <dd className="text-neutral-800">
              {health ? `reachable, schema v${health.schema_version}` : "not reachable"}
            </dd>

            <dt className="text-neutral-500">AI backend</dt>
            <dd className="text-neutral-800">{health?.ai_model ?? "—"}</dd>

            <dt className="text-neutral-500">Recording</dt>
            <dd className={health?.can_record ? "text-emerald-700" : "text-neutral-500"}>
              {health
                ? health.can_record
                  ? "microphone capture available"
                  : "not available in this build"
                : "—"}
            </dd>

            <dt className="text-neutral-500">Transcripts</dt>
            <dd className={health?.ai_local ? "text-emerald-700" : "text-amber-700"}>
              {health
                ? health.ai_local
                  ? "processed on this machine"
                  : "sent to the configured provider"
                : "—"}
            </dd>
          </dl>
        </section>

        <section>
          <h2 className="mb-2 text-[13px] font-semibold text-neutral-900">What works today</h2>
          <ul className="space-y-1.5">
            {CAPABILITIES.map((capability) => (
              <li key={capability.label} className="flex items-start gap-2 text-[13px]">
                {capability.done ? (
                  <CheckCircle2 size={15} className="mt-0.5 shrink-0 text-emerald-600" aria-hidden />
                ) : (
                  <CircleDashed size={15} className="mt-0.5 shrink-0 text-neutral-300" aria-hidden />
                )}
                <span className={capability.done ? "text-neutral-800" : "text-neutral-400"}>
                  {capability.label}
                  {capability.note && (
                    <span className="text-neutral-400"> — {capability.note}</span>
                  )}
                </span>
              </li>
            ))}
          </ul>
        </section>

        <section>
          <h2 className="mb-1 text-[13px] font-semibold text-neutral-900">Licensing</h2>
          <p className="text-[12px] leading-relaxed text-neutral-500">
            The engine and apps are MIT licensed. Hosted cloud services are under a separate
            source-available licence. Everything needed to run Notewise entirely on your own
            machine is MIT.
          </p>
        </section>
      </div>
    </div>
  );
}
