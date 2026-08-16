import { Cpu, Lock, Network } from "lucide-react";

interface WelcomeStepProps {
  onNext: () => void;
}

const CLAIMS = [
  { icon: Lock, text: "Recordings and transcripts stay on this machine" },
  { icon: Network, text: "Meetings become notes, tickets, and drafts — linked" },
  { icon: Cpu, text: "Works offline. No account, no cloud required" },
];

export function WelcomeStep({ onNext }: WelcomeStepProps) {
  return (
    <div className="flex flex-col items-center text-center">
      <h1 className="text-[32px] font-semibold tracking-tight text-ink">
        Welcome to Notewise
      </h1>
      <p className="mt-2 text-[15px] text-ink-muted">
        Record, transcribe, and understand your meetings — on your own hardware.
      </p>

      <ul className="mt-8 w-full max-w-md divide-y divide-hairline overflow-hidden rounded-xl border border-hairline bg-surface text-left">
        {CLAIMS.map(({ icon: Icon, text }) => (
          <li key={text} className="flex items-center gap-3 px-4 py-3.5">
            <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-overlay">
              <Icon size={15} className="text-ink-muted" aria-hidden />
            </span>
            <span className="text-[13px] text-ink">{text}</span>
          </li>
        ))}
      </ul>

      <button
        type="button"
        onClick={onNext}
        className="mt-8 w-full max-w-md rounded-lg bg-accent px-6 py-3 text-[14px]
                   font-medium text-white transition hover:bg-accent-hover"
      >
        Get started
      </button>

      <p className="mt-3 text-[12px] text-ink-faint">
        Setup takes a few minutes, mostly downloading.
      </p>
    </div>
  );
}
