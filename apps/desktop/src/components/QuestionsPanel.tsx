import { HelpCircle, X } from "lucide-react";
import type { AmbiguityKind, ClarifyingQuestion } from "../lib/api";

interface Props {
  questions: ClarifyingQuestion[];
  onDismiss: (question: ClarifyingQuestion) => void;
  onClose: () => void;
}

/**
 * Labels chosen to read as a reason, not a taxonomy — the user sees "no owner", not
 * "unassigned_action". The enum exists so this panel can group and filter; the words here
 * exist so someone glancing mid-meeting understands instantly.
 */
const LABEL: Record<AmbiguityKind, string> = {
  vague_reference: "unclear reference",
  unquantified: "no number given",
  unassigned_action: "no owner",
  missing_deadline: "no deadline",
  undefined_term: "undefined term",
  contradiction: "conflicts with earlier",
  unstated_rationale: "reason not stated",
};

/**
 * The highest-cost kinds get colour; the rest stay grey.
 *
 * If everything is highlighted nothing is, and a panel that looks urgent constantly gets
 * closed — which is the failure mode this whole feature has to avoid.
 */
const TONE: Partial<Record<AmbiguityKind, string>> = {
  contradiction: "bg-red-50 text-red-700 border-red-200",
  unassigned_action: "bg-amber-50 text-amber-700 border-amber-200",
};

/**
 * Suggested questions during a live meeting.
 *
 * Read-only and dismissible. It deliberately does not interrupt — no toast, no sound, no
 * focus steal. A panel a user can ignore is one they will leave open; anything that demands
 * attention gets closed in the first meeting and then the feature does not exist.
 */
export function QuestionsPanel({ questions, onDismiss, onClose }: Props) {
  return (
    <aside className="chrome flex w-72 shrink-0 flex-col border-l border-hairline bg-rail">
      <div className="flex items-center justify-between px-4 pb-2 pt-4">
        <h2 className="flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wide text-neutral-400">
          <HelpCircle size={13} aria-hidden />
          Worth asking
        </h2>
        <button
          type="button"
          onClick={onClose}
          aria-label="Hide suggested questions"
          className="flex h-6 w-6 items-center justify-center rounded text-neutral-400
                     transition hover:bg-neutral-100 hover:text-neutral-700"
        >
          <X size={14} aria-hidden />
        </button>
      </div>

      {questions.length === 0 ? (
        <p className="px-4 text-[12px] leading-relaxed text-neutral-400">
          Nothing to flag yet. Suggestions appear when something said is likely to be
          ambiguous later.
        </p>
      ) : (
        <ul className="flex-1 space-y-2 overflow-y-auto px-3 pb-4">
          {questions.map((question, index) => (
            <li
              key={`${question.at_ms}-${index}`}
              className="group rounded-lg border border-hairline bg-white p-3"
            >
              <div className="mb-1.5 flex items-start justify-between gap-2">
                <span
                  className={`rounded-full border px-1.5 py-0.5 text-[10px] font-medium ${
                    TONE[question.kind] ??
                    "border-neutral-200 bg-neutral-50 text-neutral-500"
                  }`}
                >
                  {LABEL[question.kind]}
                </span>
                <button
                  type="button"
                  onClick={() => onDismiss(question)}
                  aria-label="Dismiss this question"
                  className="shrink-0 text-neutral-300 opacity-0 transition
                             hover:text-neutral-600 group-hover:opacity-100"
                >
                  <X size={13} aria-hidden />
                </button>
              </div>

              <p className="text-[13px] font-medium leading-snug text-neutral-900">
                {question.question}
              </p>

              {question.about && (
                <p className="mt-1.5 border-l-2 border-neutral-200 pl-2 text-[11px] italic leading-snug text-neutral-500">
                  “{question.about}”
                </p>
              )}
            </li>
          ))}
        </ul>
      )}
    </aside>
  );
}
