import { useCallback, useEffect, useRef, useState } from "react";

import { startNotificationDelivery } from "./lib/notifications";
import { AlertCircle } from "lucide-react";

import { IntelPanel } from "./components/IntelPanel";
import { MeetingLibrary } from "./components/MeetingLibrary";
import { MeetingNotes } from "./components/MeetingNotes";
import { RecordDock } from "./components/RecordDock";
import { Sidebar } from "./components/Sidebar";
import { TopBar } from "./components/TopBar";
import { TranscriptView } from "./components/TranscriptView";
import { WorkspaceHeader, type Tab } from "./components/WorkspaceHeader";
import { OPEN_SETTINGS_EVENT } from "./onboarding/SetupGate";
import { AboutView } from "./views/AboutView";
import { AgentView } from "./views/AgentView";
import { JobsView } from "./views/JobsView";
import { ChatView } from "./views/ChatView";
import { ConnectorsView } from "./views/ConnectorsView";
import { HelpView } from "./views/HelpView";
import { HomeView } from "./views/HomeView";
import { LibraryView } from "./views/LibraryView";
import { RecordView } from "./views/RecordView";
import { JoinPrompt } from "./components/JoinPrompt";
import { SettingsView } from "./views/SettingsView";
import { NotesView } from "./views/NotesView";
import { SummaryView } from "./views/SummaryView";
import { TasksView } from "./views/TasksView";
import { TicketsView } from "./views/TicketsView";
import { TrashView } from "./views/TrashView";
import {
  api,
  ApiError,
  type ClarifyingQuestion,
  type Health,
  type Meeting,
  type Segment,
  type Speaker,
} from "./lib/api";
import { useSummary } from "./lib/useSummary";
import { useRoute } from "./lib/router";
import { requestSearchFocus, useShortcuts } from "./lib/shortcuts";
import { useTheme } from "./lib/useTheme";

/** How often to ask the engine for clarifying questions while recording. */
const QUESTION_POLL_MS = 30_000;

/**
 * Tabs whose own text field sits where the floating record dock does.
 *
 * The dock is hidden on these and the stop button moves into the header instead, so recording
 * stays stoppable in one press without a button overlapping a place people type.
 */
const DOCKLESS_TABS: Tab[] = ["ask", "notes"];

/** The most recent meeting that was never ended, if there is one. */
function meetingsStillOpen(meetings: Meeting[]): string | null {
  return meetings.find((meeting) => meeting.ended_at === null)?.id ?? null;
}

/**
 * The window.
 *
 * A sidebar and one destination at a time. On a meeting, that destination is itself four
 * columns: which meeting (library), the meeting (workspace), and what it means (intelligence).
 * The last of those is the point — decisions, commitments and the questions still worth asking
 * sit beside the transcript as it arrives, rather than behind a screen nobody visits until the
 * meeting is over.
 */
export default function App() {
  // The address is the state. Which meeting is open and which tab is showing live in the
  // URL, so Back works, a window reload lands where it was, and a meeting can be linked to.
  const { route, navigate } = useRoute();
  const [panelOpen, setPanelOpen] = useState(true);
  const theme = useTheme();

  const tab: Tab = route.name === "meeting" ? route.tab : "transcript";
  const selectedId = route.name === "meeting" ? route.id : null;

  const [health, setHealth] = useState<Health | null>(null);
  const [meetings, setMeetings] = useState<Meeting[]>([]);
  const [segments, setSegments] = useState<Segment[]>([]);
  /** The distinct voices in the open meeting, so they can be named. */
  const [speakers, setSpeakers] = useState<Speaker[]>([]);
  const [questions, setQuestions] = useState<ClarifyingQuestion[]>([]);
  /**
   * Why the engine had nothing to suggest, in its own words.
   *
   * It always returned this and the window always threw it away, so an empty panel looked
   * identical whether the feature was thinking, gated, or broken.
   */
  const [questionsReason, setQuestionsReason] = useState<string | null>(null);

  /**
   * The meeting the engine is actually capturing audio into.
   *
   * Only ever set from `health.recording_meeting_id`. Deliberately not inferred from a meeting
   * with no `ended_at`: an engine that cannot record still has open meetings — one created
   * through the API, or one a crash left dangling — and calling those "recording" put a red
   * indicator on screen claiming a microphone was live on a build with no capture compiled in.
   */
  const [recordingId, setRecordingId] = useState<string | null>(null);
  /**
   * A meeting that has not been ended, on an engine that cannot record.
   *
   * Distinct from the above and never shown as capture. It exists so such a meeting can still
   * be closed — it is a meeting in progress, not a recording in progress.
   */
  const [openMeetingId, setOpenMeetingId] = useState<string | null>(null);
  const [startedAt, setStartedAt] = useState<number | null>(null);
  /** Which input the engine is capturing from, shown so a wrong device is caught early. */
  const [device, setDevice] = useState<string | null>(null);
  /** Chosen input device, or null for the system default. Applies to the next recording. */
  const [preferredDevice, setPreferredDevice] = useState<string | null>(null);
  /** Chosen spoken language, or null to let the model detect it. */
  const [language, setLanguage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [summarizing, setSummarizing] = useState(false);
  /**
   * Bumped after a summary run so the decision and action-item lists reload.
   *
   * A counter rather than a boolean: two summaries in a row must both trigger a refresh, and
   * a flag that is already true the second time would not.
   */
  const [summaryOutputToken, setSummaryOutputToken] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const isRecording = recordingId !== null;
  /** The meeting the record button acts on — capturing, or merely still open. */
  const activeMeetingId = recordingId ?? openMeetingId;
  const selectedMeeting = meetings.find((m) => m.id === selectedId) ?? null;
  const summaryState = useSummary(selectedId);

  const report = useCallback((e: unknown) => {
    setError(e instanceof ApiError ? e.message : "Something went wrong.");
  }, []);

  const refresh = useCallback(async () => {
    try {
      const [nextHealth, nextMeetings] = await Promise.all([api.health(), api.meetings(50)]);
      setHealth(nextHealth);
      setMeetings(nextMeetings);
      setError(null);

      // Recording state comes from the engine, not local state: the window can be
      // reloaded while a meeting is still running.
      //
      // `recording_meeting_id` is the only thing that means "a microphone is open". A meeting
      // with no `ended_at` means something weaker — created through the API, or left dangling
      // by a crash — and is tracked separately so it can be closed without the UI announcing a
      // recording that is not happening.
      const live = nextHealth.recording_meeting_id;
      const open = live ?? meetingsStillOpen(nextMeetings);

      setRecordingId(live);
      setOpenMeetingId(live ? null : open);

      if (open) {
        const meeting = nextMeetings.find((m) => m.id === open);
        if (meeting) setStartedAt(new Date(meeting.started_at).getTime());
      } else {
        setStartedAt(null);
      }

      // Deliberately no redirect to the live meeting. That used to fire whenever the app was
      // on home, which meant home could not be visited during a recording — and it read the
      // route from a stale closure to decide. The sidebar carries a permanent "go live"
      // control instead, which is a way back rather than a hijack.

      if (live) {
        // Only asked for while something is actually recording, so an idle app makes two
        // requests per refresh rather than three.
        api
          .recordingStatus()
          .then((status) => setDevice(status.device))
          .catch(() => setDevice(null));
      } else {
        setDevice(null);
      }
    } catch (e) {
      report(e);
    }
  }, [report]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Drain the notification queue for as long as the app is open. The engine has queued desktop
  // notifications since the comms layer landed and nothing has ever shown one — it cannot, having
  // no way to raise an OS notification. See `lib/notifications`.
  useEffect(() => startNotificationDelivery(), []);

  // The setup banner lives above this tree and cannot navigate on its own.
  useEffect(() => {
    const open = () => navigate({ name: "settings" });
    window.addEventListener(OPEN_SETTINGS_EVENT, open);
    return () => window.removeEventListener(OPEN_SETTINGS_EVENT, open);
  }, []);

  // Load the open transcript, polling while that meeting records.
  //
  // The record page has no meeting in the address bar but does want the live one — that is the
  // whole point of the screen, watching words arrive. So the id being read is the selected
  // meeting, falling back to whatever is recording while that page is showing.
  const transcriptId = selectedId ?? (route.name === "record" ? activeMeetingId : null);

  useEffect(() => {
    if (!transcriptId) {
      setSegments([]);
      setSpeakers([]);
      return;
    }

    let cancelled = false;
    const load = async () => {
      try {
        // Together, so the names in the transcript and the list behind them cannot disagree —
        // a rename popover offering to merge into a speaker who is no longer there would be
        // the visible form of that drift.
        const [next, roster] = await Promise.all([
          api.transcript(transcriptId),
          api.speakers(transcriptId),
        ]);
        if (cancelled) return;
        setSegments(next);
        setSpeakers(roster.speakers);
      } catch (e) {
        if (!cancelled) report(e);
      }
    };

    void load();
    if (transcriptId !== recordingId) return;

    const id = setInterval(load, 1000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [transcriptId, recordingId, report]);

  /**
   * Put a name to a voice — or fold two of them together.
   *
   * Rethrows rather than reporting, because the popover keeps the typed name on screen and can
   * say what went wrong in place. Sending a failure to the global error banner would clear the
   * popover and lose what the user typed.
   */
  const renameSpeaker = useCallback(
    async (from: string | null, to: string) => {
      if (!transcriptId) return;

      const result = await api.renameSpeaker(transcriptId, from, to);
      setSpeakers(result.speakers);
      setSegments(await api.transcript(transcriptId));

      setNotice(
        result.merged
          ? `Merged into ${to} — ${result.segments_changed} line(s) reattributed.`
          : `Renamed to ${to} across ${result.segments_changed} line(s).`,
      );
    },
    [transcriptId],
  );

  // Ask for clarifying questions while recording.
  //
  // Polled rather than pushed, and the engine gates on its own cooldown, so an over-eager
  // interval here cannot become an over-eager panel. Failures are swallowed: a suggestion
  // that did not arrive is not worth an error banner during someone's meeting.
  useEffect(() => {
    if (!recordingId) return;

    let cancelled = false;
    const ask = async () => {
      try {
        const result = await api.questions(recordingId);
        if (cancelled) return;
        setQuestionsReason(result.reason ?? null);
        if (result.questions.length === 0) return;

        setQuestions((current) => {
          const seen = new Set(current.map((q) => q.question));
          return [...current, ...result.questions.filter((q) => !seen.has(q.question))];
        });
      } catch (e) {
        // Not an error banner over someone's meeting — but the panel should say something
        // rather than sit blank as though nothing were wrong.
        if (!cancelled) {
          setQuestionsReason(
            e instanceof ApiError ? e.message : "The engine could not be reached.",
          );
        }
      }
    };

    void ask();
    const id = setInterval(ask, QUESTION_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [recordingId]);

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
      if (activeMeetingId) {
        if (recordingId && health?.can_record) {
          const stopped = await api.stopRecording();
          setNotice(
            `Recording stopped — ${stopped.segments} segment(s)` +
              (stopped.speakers > 0 ? `, ${stopped.speakers} speaker(s).` : "."),
          );
        } else {
          // Not a recording, just a meeting nobody closed. Ending it is bookkeeping.
          await api.endMeeting(activeMeetingId);
          setNotice("Meeting closed.");
        }
        setRecordingId(null);
        setOpenMeetingId(null);
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
          // The engine creates the meeting as part of starting, so this is set in
          // practice; the type allows null because the same shape reports "not recording".
          if (started.meeting_id) select(started.meeting_id);
          setStartedAt(Date.now());
          setDevice(started.device);
          setNotice(
            `Recording from ${started.device ?? "the default input"} using ${
              started.model ?? "the default model"
            }.`,
          );
        } else {
          const meeting = await api.createMeeting(title);
          // Open, not recording — this engine has no capture to start.
          setOpenMeetingId(meeting.id);
          select(meeting.id);
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

  /** Run the model, then re-read the stored summary so every view of it agrees. */
  /// Correct a mis-transcribed line, and reflect it without refetching the whole transcript.
  ///
  /// The engine drops the segment's stale embedding in the same transaction, so the next indexing
  /// pass rebuilds it — nothing here has to know that.
  const correctSegment = async (segmentId: string, text: string) => {
    await api.setSegmentText(segmentId, text);
    setSegments((current) =>
      current.map((s) => (s.id === segmentId ? { ...s, text } : s)),
    );
  };

  /// Rename the open meeting, updating the list so the sidebar agrees immediately.
  const renameMeeting = async (title: string) => {
    if (!selectedId) return;
    await api.setMeetingTitle(selectedId, title);
    setMeetings((current) =>
      current.map((m) => (m.id === selectedId ? { ...m, title } : m)),
    );
  };

  const summarize = async () => {
    if (!selectedId) return;
    setSummarizing(true);
    setError(null);
    try {
      const result = await api.summarize(selectedId);
      await summaryState.reload();
      setSummaryOutputToken((n) => n + 1);
      setNotice(
        `Summarized with ${result.model} — ${result.decisions} decision(s), ` +
          `${result.action_items} action item(s).`,
      );
    } catch (e) {
      report(e);
    } finally {
      setSummarizing(false);
    }
  };

  /**
   * Transcribe a file already on this machine.
   *
   * A path rather than a file upload: the engine runs on this machine, so uploading would copy
   * gigabytes through HTTP to reach a file that is already there.
   */
  /**
   * Transcribe a file the user picks.
   *
   * A hidden `<input type="file">` driven by the menu item, because a browser picker is the only
   * one available without a native dialog and the IPC that comes with it. It hands over bytes and
   * never a path, so the file is uploaded — over loopback, which is a local copy.
   *
   * This replaced a `window.prompt` asking for an absolute path typed from memory.
   */
  const fileInput = useRef<HTMLInputElement>(null);

  const importAudio = () => fileInput.current?.click();

  const onFileChosen = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    // Reset immediately so choosing the same file twice in a row still fires a change event.
    event.target.value = "";
    if (!file) return;

    setBusy(true);
    setError(null);
    setNotice(
      `Transcribing ${file.name} — this runs at about 25x realtime, so a one-hour recording ` +
        `takes a couple of minutes.`,
    );
    try {
      const result = await api.importUpload(file, language ?? undefined);
      select(result.meeting_id);
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

  /**
   * Move a meeting to the trash and leave the page it was on.
   *
   * Reversible, so no dialog beyond the header's own two-press confirm. Navigating away
   * matters: staying on a meeting that has just been deleted shows a transcript the rest of
   * the app can no longer find.
   */
  const deleteMeeting = async (id: string) => {
    try {
      await api.deleteMeeting(id);
      setNotice("Meeting moved to the trash.");
      navigate({ name: "library" });
      await refresh();
    } catch (e) {
      report(e);
    }
  };

  const exportMeeting = (variant: "full" | "brief") => {
    if (!selectedId) return;
    // Navigating lets the browser handle the download, preserving the filename from
    // Content-Disposition that a JS-built blob would lose.
    window.location.href = api.exportUrl(selectedId, variant);
  };

  const select = (id: string) => navigate({ name: "meeting", id, tab: "transcript" });
  const setTab = (next: Tab) =>
    selectedId && navigate({ name: "meeting", id: selectedId, tab: next });

  // Kept current every render so a keyboard handler bound once still sees today's state.
  const latest = useRef({ busy, toggle: toggleRecording });
  latest.current = { busy, toggle: toggleRecording };

  useShortcuts({
    /**
     * Go to a search field, wherever one is.
     *
     * The meeting library's box searches what was *said*, which is the useful one, so ⌘K goes
     * there when a meeting is open. Otherwise it lands on the library page, whose box filters
     * titles. Both are dispatched the same event; whichever is mounted answers it.
     */
    onSearch: useCallback(() => {
      if (route.name !== "meeting" && route.name !== "library") {
        navigate({ name: "library" });
      }
      // The request stands for a moment, so the field picks it up whether it is already on
      // screen or arrives with the screen the line above just navigated to.
      requestSearchFocus();
    }, [route.name, navigate]),

    onNewNote: useCallback(async () => {
      try {
        const note = await api.createNote({ title: "Untitled", body: "" });
        navigate({ name: "notes", id: note.id });
      } catch (e) {
        report(e);
      }
    }, [navigate, report]),

    /**
     * Start or stop, through a ref rather than a captured function.
     *
     * `toggleRecording` closes over health, the active meeting and the chosen device, and is
     * rebuilt every render. Capturing it in a `useCallback` would freeze whichever version
     * existed when the dependencies last changed, and the shortcut would then act on state
     * that had since moved — the same stale-closure fault that made the app redirect away
     * from home based on a route it had read minutes earlier.
     */
    onToggleRecording: useCallback(() => {
      if (!latest.current.busy) void latest.current.toggle();
    }, []),
  });

  /** Only a meeting page gets the library column and the intelligence panel beside it. */
  const inWorkspace = route.name === "meeting";

  return (
    <div className="flex h-full overflow-hidden">
      {/* Offscreen rather than hidden: a `display:none` input cannot be opened by a click
          in some webviews, and this has to work in the one Tauri ships. */}
      <input
        ref={fileInput}
        type="file"
        accept="audio/*,.wav,.mp3,.m4a,.flac,.ogg,.aac,.webm"
        onChange={(event) => void onFileChosen(event)}
        className="sr-only"
        tabIndex={-1}
        aria-hidden
      />

      <Sidebar
        view={route.name}
        onNavigate={navigate}
        isRecording={isRecording}
        onGoLive={() => {
          if (recordingId) select(recordingId);
        }}
      />

      {inWorkspace && (
        <MeetingLibrary
          meetings={meetings}
          selectedId={selectedId}
          recordingId={recordingId}
          onSelect={select}
        />
      )}

      <div className="flex min-w-0 flex-1 flex-col">
        <TopBar
          health={health}
          isRecording={isRecording}
          device={preferredDevice}
          onDeviceChange={setPreferredDevice}
          language={language}
          onLanguageChange={setLanguage}
          onBackendChange={() => void refresh()}
          onError={setError}
        />

        <div className="relative flex min-h-0 flex-1">
          <main className="flex min-w-0 flex-1 flex-col">
            {error && (
              <div
                role="alert"
                className="flex items-center gap-2 border-b border-warn-line bg-warn px-4 py-2 text-[13px] text-warn-text"
              >
                <AlertCircle size={15} className="shrink-0" aria-hidden />
                {error}
              </div>
            )}

            {notice && (
              <div className="border-b border-hairline bg-overlay px-4 py-2 text-[13px] text-ink-muted">
                {notice}
              </div>
            )}

            {inWorkspace && (
              <>
                <WorkspaceHeader
                  meeting={selectedMeeting}
                  onRename={renameMeeting}
                  segments={segments}
                  tab={tab}
                  onTabChange={setTab}
                  isRecording={isRecording && selectedId === recordingId}
                  // Only where the dock is not: two stop buttons on one screen invite the
                  // second press that starts a new recording.
                  onStop={DOCKLESS_TABS.includes(tab) && !busy ? toggleRecording : undefined}
                  panelHidden={!panelOpen}
                  onShowPanel={() => setPanelOpen(true)}
                  onDelete={selectedId ? () => void deleteMeeting(selectedId) : undefined}
                />

                {tab === "transcript" && (
                  <TranscriptView
                    segments={segments}
                    isRecording={isRecording && selectedId === recordingId}
                    hasMeeting={selectedId !== null}
                    speakers={speakers}
                    onRenameSpeaker={renameSpeaker}
                    onCorrectSegment={correctSegment}
                    audioMeetingId={selectedId}
                  />
                )}
                {tab === "summary" && (
                  <SummaryView
                    meetingId={selectedId}
                    summary={summaryState.summary}
                    loading={summaryState.loading}
                    error={summaryState.error}
                    hasTranscript={segments.length > 0}
                    summarizing={summarizing}
                    onSummarize={() => void summarize()}
                    onReload={() => {
                      void summaryState.reload();
                      setSummaryOutputToken((n) => n + 1);
                    }}
                  />
                )}
                {tab === "notes" && (
                  <MeetingNotes
                    meetingId={selectedId}
                    meetingTitle={selectedMeeting?.title ?? null}
                    isRecording={isRecording && selectedId === recordingId}
                    onNavigate={navigate}
                  />
                )}
                {tab === "ask" && (
                  <ChatView
                    meetingId={selectedId}
                    meetingTitle={selectedMeeting?.title ?? null}
                    hasTranscript={segments.length > 0}
                  />
                )}
              </>
            )}

            {/* A meeting the extension or the calendar noticed. Above everything, because it is
                time-limited in a way nothing else on screen is — and it renders nothing at all when
                there is no offer, which is almost always. */}
            <JoinPrompt
              canRecord={health?.can_record ?? false}
              onStarted={(id) => navigate({ name: "meeting", id, tab: "transcript" })}
            />

            {route.name === "home" && (
              <HomeView
                meetings={meetings}
                isRecording={isRecording}
                canRecord={health?.can_record ?? false}
                onNavigate={navigate}
                onStartRecording={() => {
                  navigate({ name: "record" });
                  void toggleRecording();
                }}
                onImport={importAudio}
              />
            )}

            {route.name === "record" && (
              <RecordView
                health={health}
                isRecording={isRecording}
                openMeeting={openMeetingId !== null}
                startedAt={startedAt}
                busy={busy}
                liveDevice={device}
                preferredDevice={preferredDevice}
                onDeviceChange={setPreferredDevice}
                language={language}
                segments={segments}
                onToggle={toggleRecording}
                onImport={importAudio}
                onNavigate={navigate}
                recordingId={activeMeetingId}
                listDevices={api.devices}
              />
            )}

            {route.name === "library" && (
              <LibraryView
                meetings={meetings}
                recordingId={recordingId}
                canRecord={health?.can_record ?? false}
                onNavigate={navigate}
                onImport={importAudio}
              />
            )}

            {route.name === "notes" && (
              <NotesView noteId={route.id} onNavigate={navigate} />
            )}
            {route.name === "tasks" && (
              <TasksView meetings={meetings} onNavigate={navigate} />
            )}
            {route.name === "tickets" && <TicketsView />}
            {route.name === "trash" && <TrashView />}
            {route.name === "agent" && <AgentView onNavigate={navigate} />}
            {route.name === "jobs" && <JobsView />}
            {route.name === "connectors" && <ConnectorsView />}
            {route.name === "help" && (
              <HelpView section={route.section ?? "docs"} onNavigate={navigate} />
            )}
            {route.name === "settings" && (
              <SettingsView
                theme={theme.theme}
                onModeChange={theme.setMode}
                onAccentChange={theme.setAccent}
                onNavigate={navigate}
              />
            )}
            {route.name === "about" && <AboutView health={health} />}
          </main>

          {inWorkspace && panelOpen && (
            <IntelPanel
              meetingId={selectedId}
              summary={summaryState.summary}
              summaryOutputToken={summaryOutputToken}
              onOpenMeeting={select}
              questions={selectedId === recordingId ? questions : []}
              questionsReason={selectedId === recordingId ? questionsReason : null}
              isRecording={isRecording && selectedId === recordingId}
              hasTranscript={segments.length > 0}
              summarizing={summarizing}
              onSummarize={() => void summarize()}
              onDismissQuestion={(question) =>
                setQuestions((current) => current.filter((q) => q !== question))
              }
              onClose={() => setPanelOpen(false)}
            />
          )}

          {/* Not on Ask or Notes: the dock floats bottom-centre, which is exactly where those
              tabs put their text field, and a button sitting on top of one is not a control. */}
          {inWorkspace && !DOCKLESS_TABS.includes(tab) && (
            <RecordDock
              isRecording={isRecording}
              openMeeting={openMeetingId !== null}
              startedAt={startedAt}
              busy={busy}
              canRecord={health?.can_record ?? false}
              device={device}
              onToggle={toggleRecording}
              onImport={importAudio}
              canImport={(health?.can_record ?? false) && !isRecording}
              onExport={exportMeeting}
              canExport={selectedId !== null}
            />
          )}
        </div>
      </div>
    </div>
  );
}
