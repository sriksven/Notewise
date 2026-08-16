import { useState } from "react";
import { ArrowLeft, Loader2 } from "lucide-react";

import { api, ApiError } from "../lib/api";
import {
  canFinish,
  firstUnsatisfied,
  skipConsequences,
  stepsFor,
  type SetupReadiness,
  type StepId,
} from "./readiness";
import { Stepper } from "./Stepper";
import { BackendStep } from "./steps/BackendStep";
import { ModelStep } from "./steps/ModelStep";
import { PermissionsStep } from "./steps/PermissionsStep";
import { WelcomeStep } from "./steps/WelcomeStep";

interface SetupFlowProps {
  readiness: SetupReadiness;
  /** Re-fetch readiness from the engine. */
  refresh: () => Promise<void>;
  onFinished: () => void;
}

export function SetupFlow({ readiness, refresh, onFinished }: SetupFlowProps) {
  // Session-local, not persisted. Storing a step index is what forces an onboarding state
  // machine to guard every write against its own auto-save; the cost of not storing it is one
  // extra click after a relaunch, which is the cheaper trade.
  const [step, setStep] = useState<StepId>("welcome");
  const [finishing, setFinishing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const steps = stepsFor(readiness);
  const order = steps.map((s) => s.id);
  const index = order.indexOf(step);
  const isLast = index === order.length - 1;
  const ready = canFinish(readiness);

  const skipping = skipConsequences(readiness);

  const finish = async () => {
    setFinishing(true);
    setError(null);
    try {
      // The engine refuses an unsatisfied completion unless it is asked for deliberately, so
      // the intent has to be passed along rather than inferred there.
      await api.completeSetup(!ready);
      onFinished();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not finish setup.");
      setFinishing(false);
    }
  };

  return (
    <div className="flex h-full flex-col overflow-y-auto bg-overlay">
      <div className="mx-auto flex w-full max-w-2xl flex-1 flex-col px-6 py-12">
        {step !== "welcome" && (
          <div className="mb-10">
            <Stepper steps={steps} current={step} />
          </div>
        )}

        <div className="flex flex-1 flex-col justify-center">
          {step === "welcome" && (
            // Next jumps to the first thing actually outstanding, so a relaunch mid-setup
            // costs one click rather than three.
            <WelcomeStep onNext={() => setStep(firstUnsatisfied(readiness))} />
          )}
          {step === "model" && (
            <ModelStep satisfied={readiness.steps.model.satisfied} onChanged={refresh} />
          )}
          {step === "backend" && (
            <BackendStep satisfied={readiness.steps.backend.satisfied} onChanged={refresh} />
          )}
          {step === "permissions" && (
            <PermissionsStep readiness={readiness.steps.permissions} onChanged={refresh} />
          )}
        </div>

        {error && (
          <div
            role="alert"
            className="mx-auto mt-6 w-full max-w-md rounded-lg border border-warn-line bg-warn px-3 py-2 text-[13px] text-warn-text"
          >
            {error}
          </div>
        )}

        {step !== "welcome" && (
          <div className="mx-auto mt-10 w-full max-w-md">
            <div className="flex items-center gap-3">
              <button
                type="button"
                onClick={() => setStep(order[Math.max(0, index - 1)])}
                className="flex items-center gap-1.5 rounded-lg px-3 py-3 text-[13px]
                           text-ink-muted transition hover:text-ink"
              >
                <ArrowLeft size={14} aria-hidden />
                Back
              </button>

              {isLast ? (
                // Never disabled. A permission the user declined, or cannot grant, would
                // otherwise leave the only way into the app greyed out on a screen with no
                // other exit — a first launch that ends in a dead end.
                <button
                  type="button"
                  onClick={() => void finish()}
                  disabled={finishing}
                  className="flex flex-1 items-center justify-center gap-2 rounded-lg bg-accent
                             px-6 py-3 text-[14px] font-medium text-white transition
                             hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
                >
                  {finishing && <Loader2 size={15} className="animate-spin" aria-hidden />}
                  {ready ? "Finish setup" : "Continue anyway"}
                </button>
              ) : (
                <button
                  type="button"
                  onClick={() => setStep(order[index + 1])}
                  className="flex-1 rounded-lg bg-accent px-6 py-3 text-[14px] font-medium
                             text-white transition hover:bg-accent-hover"
                >
                  Continue
                </button>
              )}
            </div>

            {isLast && skipping.length > 0 && (
              <p className="mt-3 text-center text-[12px] leading-relaxed text-ink-muted">
                You can go in without finishing — but {skipping.join(", and ")}. Everything else
                works, and Settings can pick this up later.
              </p>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
