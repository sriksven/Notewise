import { useEffect, useRef, useState } from "react";
import {
  CircleCheck,
  Link2,
  Users,
  Repeat,
  Gavel,
  HelpCircle,
  Loader2,
  PanelRightClose,
  Sparkles,
  X,
} from "lucide-react";

import { type AmbiguityKind, type ClarifyingQuestion, type Summary } from "../lib/api";
import type { Route } from "../lib/router";
import { ActionItems } from "./ActionItems";
import { Decisions } from "./Decisions";
import { Participants } from "./Participants";
import { RelatedItems } from "./RelatedItems";
import { MeetingBrief } from "./MeetingBrief";

interface Props {
  /** Null when no meeting is selected — the panel says so rather than showing an empty shell. */
  meetingId: string | null;
  /**
   * The latest summary, read only to label the button.
   *
   * Not the source of the lists below any more: a summary's decisions and action items belong
   * to that one run, and both outlive it.
   */
  summary: Summary | null;
  questions: ClarifyingQuestion[];
  /**
   * Why there is nothing to show, from the engine.
   *
   * The suggester is gated — it needs a couple of hundred characters of recent transcript
   * and holds a ninety-second cooldown — and an empty panel that does not say so is
   * indistinguishable from a broken one.
   */
  questionsReason?: string | null;
  isRecording: boolean;
  /** Whether this meeting has anything to summarize yet. */
  hasTranscript: boolean;
  summarizing: boolean;
  /** Bumped after a summary run, so newly extracted decisions and action items appear. */
  summaryOutputToken: number;
  /** Jump to another meeting — the previous instance of a recurring series. */
  onOpenMeeting: (id: string) => void;
  /** Anywhere else: a participant's page, reached from the roster above. */
  onNavigate: (route: Route) => void;
  onSummarize: () => void;
  onDismissQuestion: (question: ClarifyingQuestion) => void;
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
  contradiction: "bg-danger text-danger-text border-danger-line",
  unassigned_action: "bg-warn text-warn-text border-warn-line",
};

/** How long a question stays up after it arrives. The engine's `ClarifierConfig::staleness_ms`. */
export const STALE_AFTER_MS = 5 * 60_000;

/** How many questions show at once. The engine's `ClarifierConfig::max_pending`. */
export const MAX_SHOWN = 3;

/** One question's identity across polls: the same words about the same moment. */
export function questionKey(question: ClarifyingQuestion): string {
  return `${question.at_ms}:${question.question}`;
}

/**
 * The questions still worth showing: newest first, none stale, at most {@link MAX_SHOWN}.
 *
 * The window appends every question it receives and keeps them until the recording ends, so
 * without this an hour-long meeting grew one long list, oldest at the top — the question about
 * what was just said sat below the fold, under ones about things said half an hour ago. The engine
 * defines a cap and a staleness for exactly this and nothing on the live path applied them.
 *
 * Staleness counts from when a question *arrived*, kept in `firstSeen`: during a live recording
 * the wall clock and the meeting advance together, and arrival is the moment the user could
 * first have asked it.
 */
export function currentQuestions(
  questions: ClarifyingQuestion[],
  firstSeen: Map<string, number>,
  now: number,
): ClarifyingQuestion[] {
  return questions
    .filter((question) => now - (firstSeen.get(questionKey(question)) ?? now) < STALE_AFTER_MS)
    .sort((a, b) => b.at_ms - a.at_ms)
    .slice(0, MAX_SHOWN);
}

/** {@link currentQuestions}, kept current as time passes. */
function useCurrentQuestions(questions: ClarifyingQuestion[]): ClarifyingQuestion[] {
  const firstSeen = useRef(new Map<string, number>());
  // Re-rendered on a slow tick while anything is showing, so a question expires on its own
  // rather than whenever something else happens to redraw the panel.
  const [, setTick] = useState(0);
  useEffect(() => {
    if (questions.length === 0) return;
    const id = setInterval(() => setTick((tick) => tick + 1), 30_000);
    return () => clearInterval(id);
  }, [questions.length]);

  const now = Date.now();
  for (const question of questions) {
    const key = questionKey(question);
    if (!firstSeen.current.has(key)) firstSeen.current.set(key, now);
  }
  return currentQuestions(questions, firstSeen.current, now);
}

function Section({
  icon,
  title,
  count,
  children,
}: {
  icon: React.ReactNode;
  title: string;
  count?: number;
  children: React.ReactNode;
}) {
  return (
    <section className="border-b border-hairline px-4 py-3 last:border-b-0">
      <h3 className="mb-2 flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wide text-ink-faint">
        {icon}
        {title}
        {count !== undefined && count > 0 && (
          <span className="ml-auto rounded-full bg-overlay px-1.5 py-px text-[10px] font-medium tabular-nums text-ink-muted">
            {count}
          </span>
        )}
      </h3>
      {children}
    </section>
  );
}

function Empty({ children }: { children: React.ReactNode }) {
  return <p className="text-[12px] leading-relaxed text-ink-faint">{children}</p>;
}

/**
 * What the meeting means, beside what was said.
 *
 * The point of the whole app in one column: questions worth asking while there is still time to
 * ask them, and the decisions and commitments that came out. These used to live on a separate
 * screen reached from the navigation rail, which meant nobody saw a decision until they went
 * looking for it — and never during the meeting, when it could still be corrected.
 *
 * Nothing here interrupts. No toast, no sound, no focus steal. A panel a user can ignore is one
 * they will leave open; anything that demands attention gets closed in the first meeting and
 * then the feature does not exist.
 */
export function IntelPanel({
  meetingId,
  summary,
  questions,
  questionsReason,
  isRecording,
  hasTranscript,
  summarizing,
  summaryOutputToken,
  onOpenMeeting,
  onNavigate,
  onSummarize,
  onDismissQuestion,
  onClose,
}: Props) {
  const shown = useCurrentQuestions(questions);

  return (
    <aside
      aria-label="Meeting intelligence"
      className="chrome flex w-[300px] shrink-0 flex-col border-l border-hairline bg-rail"
    >
      <div className="flex items-center justify-between border-b border-hairline px-4 py-2.5">
        <span className="text-[12px] font-semibold text-ink">Intelligence</span>
        <button
          type="button"
          onClick={onClose}
          aria-label="Hide the intelligence panel"
          title="Hide"
          className="flex h-6 w-6 items-center justify-center rounded text-ink-faint
                     transition hover:bg-overlay hover:text-ink"
        >
          <PanelRightClose size={14} aria-hidden />
        </button>
      </div>

      {!meetingId ? (
        <p className="px-4 py-3 text-[12px] leading-relaxed text-ink-faint">
          Select a meeting, or start recording, and what it means shows up here.
        </p>
      ) : (
        <div className="min-h-0 flex-1 overflow-y-auto">
          {/* First, because who was there is context for every other section — a decision means
              something different depending on who was in the room to make it. */}
          <Section icon={<Users size={13} aria-hidden />} title="Who was there">
            <Participants meetingId={meetingId} onNavigate={onNavigate} />
          </Section>

          <Section
            icon={<HelpCircle size={13} aria-hidden />}
            title="Worth asking"
            count={shown.length}
          >
            {shown.length === 0 ? (
              <Empty>
                {!isRecording
                  ? "Suggestions are made while a meeting is running, when there is still time to ask."
                  : (questionsReason ??
                    "Listening. A suggestion appears when something said is likely to be ambiguous later.")}
              </Empty>
            ) : (
              <ul className="space-y-2">
                {shown.map((question) => (
                  <li
                    // Not the index: dismissing one would shift every key after it, and React
                    // would hand the next card the dismissed one's hover and focus state.
                    key={questionKey(question)}
                    className="group rounded-lg border border-hairline bg-surface p-2.5"
                  >
                    <div className="mb-1.5 flex items-start justify-between gap-2">
                      <span
                        className={`rounded-full border px-1.5 py-0.5 text-[10px] font-medium ${
                          TONE[question.kind] ??
                          "border-hairline bg-overlay text-ink-muted"
                        }`}
                      >
                        {LABEL[question.kind]}
                      </span>
                      <button
                        type="button"
                        onClick={() => onDismissQuestion(question)}
                        aria-label="Dismiss this question"
                        // Shown on keyboard focus too: hover-only made Tab land on an invisible
                        // control.
                        className="shrink-0 text-ink-faint opacity-0 transition
                                   hover:text-ink focus-visible:opacity-100 group-hover:opacity-100"
                      >
                        <X size={13} aria-hidden />
                      </button>
                    </div>

                    <p className="text-[13px] font-medium leading-snug text-ink">
                      {question.question}
                    </p>

                    {question.about && (
                      <p className="mt-1.5 border-l-2 border-hairline pl-2 text-[11px] italic leading-snug text-ink-muted">
                        “{question.about}”
                      </p>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </Section>

          {/* Not driven by `summary` either, and for the same reason as action items below:
              `summary.decisions` is one summary's list, so regenerating a summary hid every
              decision the previous run found while they stayed in the graph. */}
          <Section icon={<Gavel size={13} aria-hidden />} title="Decisions">
            <Decisions meetingId={meetingId} refreshToken={summaryOutputToken} />
          </Section>

          {/* Not driven by `summary`. Action items outlive the summary that proposed them —
              a user can add one by hand before summarizing, and regenerating a summary no
              longer takes the old ones with it — so this reads the meeting, not the summary. */}
          {/* Above the meeting's own output on purpose: what was already owed is context for
              what is being decided now, and a brief read after the fact is just a report. */}
          <Section icon={<Repeat size={13} aria-hidden />} title="Carried over">
            <MeetingBrief meetingId={meetingId} onOpenMeeting={onOpenMeeting} />
          </Section>

          <Section icon={<CircleCheck size={13} aria-hidden />} title="Action items">
            <ActionItems meetingId={meetingId} refreshToken={summaryOutputToken} />
          </Section>

          {/* Last of the reading sections: what this meeting is attached to is the widest context
              and the least urgent, so it sits below what the meeting itself produced. */}
          <Section icon={<Link2 size={13} aria-hidden />} title="Linked">
            <RelatedItems meetingId={meetingId} onNavigate={onNavigate} />
          </Section>

          {/* The one action in this panel. Placed under the sections it fills in, so what it
              produces is visible before it is pressed. */}
          <div className="px-4 py-3">
            <button
              type="button"
              onClick={onSummarize}
              disabled={summarizing || !hasTranscript || isRecording}
              title={
                isRecording
                  ? "Stop recording first — a summary of half a meeting goes stale immediately"
                  : hasTranscript
                    ? "Runs on the configured backend"
                    : "Needs a transcript first"
              }
              className="flex w-full items-center justify-center gap-1.5 rounded-lg border
                         border-hairline bg-surface px-3 py-2 text-[12.5px] font-medium
                         text-ink transition hover:bg-overlay
                         disabled:cursor-not-allowed disabled:opacity-50"
            >
              {summarizing ? (
                <>
                  <Loader2 size={13} className="animate-spin" aria-hidden />
                  Summarizing
                </>
              ) : (
                <>
                  <Sparkles size={13} aria-hidden />
                  {summary ? "Summarize again" : "Summarize"}
                </>
              )}
            </button>
          </div>
        </div>
      )}
    </aside>
  );
}
