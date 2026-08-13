import { useCallback, useEffect, useState } from "react";
import { AlertCircle } from "lucide-react";

import { MeetingList } from "./components/MeetingList";
import { RecordDock } from "./components/RecordDock";
import { Sidebar, type View } from "./components/Sidebar";
import { TopBar } from "./components/TopBar";
import { TranscriptView } from "./components/TranscriptView";
import { api, ApiError, type Health, type Meeting, type Segment } from "./lib/api";

export default function App() {
  const [view, setView] = useState<View>("home");
  const [panelOpen, setPanelOpen] = useState(false);

  const [health, setHealth] = useState<Health | null>(null);
  const [meetings, setMeetings] = useState<Meeting[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [segments, setSegments] = useState<Segment[]>([]);

  const [recordingId, setRecordingId] = useState<string | null>(null);
  const [startedAt, setStartedAt] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const isRecording = recordingId !== null;

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

      // Recover recording state from the engine rather than from local state:
      // the UI can be reloaded while a meeting is still running.
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

  // Load the selected transcript, and poll it while that meeting is recording.
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

    // Polling rather than a socket: the engine has no push channel yet, and one
    // request a second against loopback is not worth a protocol for.
    const id = setInterval(load, 1000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [selectedId, recordingId, report]);

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
          {panelOpen && (
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

            <TranscriptView segments={segments} isRecording={isRecording} />
          </main>

          <RecordDock
            isRecording={isRecording}
            startedAt={startedAt}
            busy={busy}
            onToggle={toggleRecording}
            onSummarize={summarize}
            canSummarize={selectedId !== null && !isRecording && segments.length > 0}
          />
        </div>
      </div>
    </div>
  );
}
