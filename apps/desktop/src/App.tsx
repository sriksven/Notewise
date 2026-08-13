import { useCallback, useEffect, useState } from "react";
import { AlertCircle, HelpCircle } from "lucide-react";

import { MeetingList } from "./components/MeetingList";
import { QuestionsPanel } from "./components/QuestionsPanel";
import { RecordDock } from "./components/RecordDock";
import { Sidebar, type View } from "./components/Sidebar";
import { TopBar } from "./components/TopBar";
import { TranscriptView } from "./components/TranscriptView";
import { AboutView } from "./views/AboutView";
import { CalendarView } from "./views/CalendarView";
import { ChatView } from "./views/ChatView";
import { SettingsView } from "./views/SettingsView";
import {
  api,
  ApiError,
  type ClarifyingQuestion,
  type Health,
  type Meeting,
  type Segment,
} from "./lib/api";

/** How often to ask the engine for clarifying questions while recording. */
const QUESTION_POLL_MS = 30_000;

export default function App() {
  const [view, setView] = useState<View>("home");
  const [panelOpen, setPanelOpen] = useState(false);
  const [questionsOpen, setQuestionsOpen] = useState(true);

  const [health, setHealth] = useState<Health | null>(null);
  const [meetings, setMeetings] = useState<Meeting[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [segments, setSegments] = useState<Segment[]>([]);
  const [questions, setQuestions] = useState<ClarifyingQuestion[]>([]);

  const [recordingId, setRecordingId] = useState<string | null>(null);
  const [startedAt, setStartedAt] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const isRecording = recordingId !== null;
  const selectedMeeting = meetings.find((m) => m.id === selectedId) ?? null;

  const report = useCallback((e: unknown) => {
    setError(e instanceof ApiError ? e.message : "Something went wrong.");
  }, []);

  const refresh = useCallback(async () => {
    try {
      const [nextHealth, nextMeetings] = await Promise.all([
        api.health(),
        api.meetings(50),
      ]);
      setHealth(nextHealth);
      setMeetings(nextMeetings);
      setError(null);

      // Recording state comes from the engine, not local state: the window can be
      // reloaded while a meeting is still running.
      const live = nextMeetings.find((m) => m.ended_at === null);
      if (live) {
        setRecordingId(live.id);
        setStartedAt(new Date(live.started_at).getTime());
        setSelectedId((current) => current ?? live.id);
      }
    } catch (e) {
      report(e);
    }
  }, [report]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Load the selected transcript, polling while that meeting records.
  useEffect(() => {
    if (!selectedId) {
      setSegments([]);
      return;
    }

    let cancelled = false;
    const load = async () => {
      try {
        const next = await api.transcript(selectedId);
        if (!cancelled) setSegments(next);
      } catch (e) {
        if (!cancelled) report(e);
      }
    };

    void load();
    if (selectedId !== recordingId) return;

    const id = setInterval(load, 1000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [selectedId, recordingId, report]);

  // Ask for clarifying questions while recording.
  //
  // Polled rather than pushed, and the engine gates on its own cooldown, so an over-eager
  // interval here cannot become an over-eager panel. Failures are swallowed: a suggestion
  // that did not arrive is not worth an error banner during someone's meeting.
  useEffect(() => {
    if (!recordingId || !questionsOpen) return;

    let cancelled = false;
    const ask = async () => {
      try {
        const result = await api.questions(recordingId);
        if (cancelled || result.questions.length === 0) return;

        setQuestions((current) => {
          const seen = new Set(current.map((q) => q.question));
          return [...current, ...result.questions.filter((q) => !seen.has(q.question))];
        });
      } catch {
        // Deliberately silent.
      }
    };

    void ask();
    const id = setInterval(ask, QUESTION_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [recordingId, questionsOpen]);

  const toggleRecording = async () => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      if (recordingId) {
        await api.endMeeting(recordingId);
        setRecordingId(null);
        setStartedAt(null);
      } else {
        const started = new Date();
        const meeting = await api.createMeeting(
          `Meeting ${started.toLocaleString([], {
            month: "short",
            day: "numeric",
            hour: "numeric",
            minute: "2-digit",
          })}`,
        );
        setRecordingId(meeting.id);
        setSelectedId(meeting.id);
        setStartedAt(new Date(meeting.started_at).getTime());
        setQuestions([]);
      }
      await refresh();
    } catch (e) {
      report(e);
    } finally {
      setBusy(false);
    }
  };

  const summarize = async () => {
    if (!selectedId) return;
    setBusy(true);
    setError(null);
    try {
      const result = await api.summarize(selectedId);
      setNotice(
        `Summarized with ${result.model} — ${result.decisions} decision(s), ` +
          `${result.action_items} action item(s).`,
      );
    } catch (e) {
      report(e);
    } finally {
      setBusy(false);
    }
  };

  const exportMeeting = (variant: "full" | "brief") => {
    if (!selectedId) return;
    // Navigating lets the browser handle the download, preserving the filename from
    // Content-Disposition that a JS-built blob would lose.
    window.location.href = api.exportUrl(selectedId, variant);
  };

  const showsTranscript = view === "home" || view === "record";

  return (
    <div className="flex h-full overflow-hidden">
      <Sidebar view={view} onChange={setView} isRecording={isRecording} />

      <div className="flex min-w-0 flex-1 flex-col">
        <TopBar
          health={health}
          panelOpen={panelOpen}
          onTogglePanel={() => setPanelOpen((open) => !open)}
        />

        <div className="relative flex min-h-0 flex-1">
          {panelOpen && showsTranscript && (
            <MeetingList
              meetings={meetings}
              selectedId={selectedId}
              onSelect={setSelectedId}
            />
          )}

          <main className="flex min-w-0 flex-1 flex-col">
            {error && (
              <div
                role="alert"
                className="flex items-center gap-2 border-b border-amber-200 bg-amber-50 px-4 py-2 text-[13px] text-amber-900"
              >
                <AlertCircle size={15} className="shrink-0" aria-hidden />
                {error}
              </div>
            )}

            {notice && (
              <div className="border-b border-hairline bg-neutral-50 px-4 py-2 text-[13px] text-neutral-600">
                {notice}
              </div>
            )}

            {showsTranscript && (
              <TranscriptView segments={segments} isRecording={isRecording} />
            )}
            {view === "calendar" && (
              <CalendarView
                meetings={meetings}
                selectedId={selectedId}
                onSelect={(id) => {
                  setSelectedId(id);
                  setView("home");
                }}
              />
            )}
            {view === "chat" && (
              <ChatView
                meetingId={selectedId}
                meetingTitle={selectedMeeting?.title ?? null}
                hasTranscript={segments.length > 0}
              />
            )}
            {view === "settings" && <SettingsView />}
            {view === "about" && <AboutView health={health} />}
          </main>

          {showsTranscript && questionsOpen && isRecording && (
            <QuestionsPanel
              questions={questions}
              onDismiss={(question) =>
                setQuestions((current) => current.filter((q) => q !== question))
              }
              onClose={() => setQuestionsOpen(false)}
            />
          )}

          {/* Re-open affordance, so dismissing the panel is not a one-way door. */}
          {showsTranscript && !questionsOpen && isRecording && (
            <button
              type="button"
              onClick={() => setQuestionsOpen(true)}
              aria-label="Show suggested questions"
              title="Show suggested questions"
              className="absolute right-3 top-3 flex h-7 w-7 items-center justify-center
                         rounded-full border border-hairline bg-white text-neutral-500
                         shadow-sm transition hover:text-neutral-900"
            >
              <HelpCircle size={14} aria-hidden />
            </button>
          )}

          {showsTranscript && (
            <RecordDock
              isRecording={isRecording}
              startedAt={startedAt}
              busy={busy}
              onToggle={toggleRecording}
              onSummarize={summarize}
              canSummarize={selectedId !== null && !isRecording && segments.length > 0}
              onExport={exportMeeting}
              canExport={selectedId !== null}
            />
          )}
        </div>
      </div>
    </div>
  );
}
