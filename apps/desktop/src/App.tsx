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
import { SummaryView } from "./views/SummaryView";
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
  /** Which input the engine is capturing from, shown so a wrong device is caught early. */
  const [device, setDevice] = useState<string | null>(null);
  /** Chosen input device, or null for the system default. Applies to the next recording. */
  const [preferredDevice, setPreferredDevice] = useState<string | null>(null);
  /** Chosen spoken language, or null to let the model detect it. */
  const [language, setLanguage] = useState<string | null>(null);
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
      //
      // `recording_meeting_id` is authoritative when the engine can capture. The open-meeting
      // fallback is only a guess — a meeting left open by a crash has no `ended_at` either, and
      // treating that as live would leave the UI stuck showing a recording that is not running.
      const liveId =
        nextHealth.recording_meeting_id ??
        (nextHealth.can_record
          ? null
          : (nextMeetings.find((m) => m.ended_at === null)?.id ?? null));

      if (liveId) {
        const live = nextMeetings.find((m) => m.id === liveId);
        setRecordingId(liveId);
        if (live) setStartedAt(new Date(live.started_at).getTime());
        setSelectedId((current) => current ?? liveId);

        // Only asked for while something is actually recording, so an idle app makes two
        // requests per refresh rather than three.
        if (nextHealth.recording_meeting_id) {
          api
            .recordingStatus()
            .then((status) => setDevice(status.device))
            .catch(() => setDevice(null));
        }
      } else {
        setRecordingId(null);
        setStartedAt(null);
        setDevice(null);
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

  /**
   * Start or stop capture.
   *
   * Capture belongs to the engine, not this window: `POST /v1/recording` opens the microphone
   * and creates the meeting in one call. That keeps a reload from orphaning a running recording
   * and lets the CLI see the same state.
   *
   * When the engine reports it cannot record, this falls back to creating a meeting with no
   * audio — still useful for pasted or imported transcripts — and says so, rather than
   * pretending the microphone is live.
   */
  const toggleRecording = async () => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      if (recordingId) {
        if (health?.can_record) {
          const stopped = await api.stopRecording();
          setNotice(
            `Recording stopped — ${stopped.segments} segment(s)` +
              (stopped.speakers > 0 ? `, ${stopped.speakers} speaker(s).` : "."),
          );
        } else {
          await api.endMeeting(recordingId);
        }
        setRecordingId(null);
        setStartedAt(null);
        setDevice(null);
      } else {
        const title = `Meeting ${new Date().toLocaleString([], {
          month: "short",
          day: "numeric",
          hour: "numeric",
          minute: "2-digit",
        })}`;

        if (health?.can_record) {
          const started = await api.startRecording({
            title,
            device: preferredDevice ?? undefined,
            language: language ?? undefined,
          });
          setRecordingId(started.meeting_id);
          setSelectedId(started.meeting_id);
          setStartedAt(Date.now());
          setDevice(started.device);
          setNotice(
            `Recording from ${started.device ?? "the default input"} using ${
              started.model ?? "the default model"
            }.`,
          );
        } else {
          const meeting = await api.createMeeting(title);
          setRecordingId(meeting.id);
          setSelectedId(meeting.id);
          setStartedAt(new Date(meeting.started_at).getTime());
          setNotice(
            "This engine cannot capture audio, so the meeting was created without it. " +
              "Import a transcript, or run a build with the record and whisper features.",
          );
        }
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
      setView("summary");
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

  /**
   * Transcribe a file already on this machine.
   *
   * A path rather than a file upload: the engine runs on this machine, so uploading would copy
   * gigabytes through HTTP to reach a file that is already there.
   */
  const importAudio = async () => {
    const path = window.prompt(
      "Path to a 32-bit float WAV file on this machine:",
    );
    if (!path?.trim()) return;

    setBusy(true);
    setError(null);
    setNotice("Transcribing — this runs at about 25x realtime.");
    try {
      const result = await api.importAudio({
        path: path.trim(),
        language: language ?? undefined,
      });
      setSelectedId(result.meeting_id);
      setNotice(
        `Imported ${Math.round(result.audio_ms / 1000)}s of audio — ` +
          `${result.segments} segment(s), ${result.speakers} speaker(s).`,
      );
      await refresh();
    } catch (e) {
      report(e);
      setNotice(null);
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
          isRecording={isRecording}
          device={preferredDevice}
          onDeviceChange={setPreferredDevice}
          language={language}
          onLanguageChange={setLanguage}
          onBackendChange={() => void refresh()}
          onError={setError}
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
            {view === "summary" && (
              <SummaryView
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
              canRecord={health?.can_record ?? false}
              device={device}
              onToggle={toggleRecording}
              onSummarize={summarize}
              canSummarize={selectedId !== null && !isRecording && segments.length > 0}
              onExport={exportMeeting}
              canExport={selectedId !== null}
              onImport={importAudio}
              canImport={(health?.can_record ?? false) && !isRecording}
            />
          )}
        </div>
      </div>
    </div>
  );
}
