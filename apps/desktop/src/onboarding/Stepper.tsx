import { Check } from "lucide-react";

import type { Step, StepId } from "./readiness";

interface StepperProps {
  steps: Step[];
  current: StepId;
}

/**
 * Dots and connectors across the top.
 *
 * Shows satisfaction, not visit history: a tick means the requirement is met, so backtracking
 * does not make a finished step appear to un-complete itself.
 */
export function Stepper({ steps, current }: StepperProps) {
  return (
    <ol className="flex items-center justify-center" aria-label="Setup progress">
      {steps.map((step, index) => {
        const isCurrent = step.id === current;
        const isDone = step.satisfied && !isCurrent;

        return (
          <li key={step.id} className="flex items-center">
            <div
              aria-current={isCurrent ? "step" : undefined}
              title={step.title}
              className={`flex h-8 w-8 items-center justify-center rounded-full text-[12px] font-medium transition ${
                isDone
                  ? "bg-accent text-accent-on"
                  : isCurrent
                    ? "bg-accent text-accent-on"
                    : "bg-hairline text-ink-muted"
              }`}
            >
              {isDone ? <Check size={15} aria-hidden /> : index + 1}
              <span className="sr-only">{step.title}</span>
            </div>

            {index < steps.length - 1 && (
              <span
                aria-hidden
                className={`mx-1.5 h-px w-10 transition ${
                  step.satisfied ? "bg-accent" : "bg-hairline"
                }`}
              />
            )}
          </li>
        );
      })}
    </ol>
  );
}
