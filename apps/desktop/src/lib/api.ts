/**
 * Client for the local Notewise engine.
 *
 * Talks to the loopback REST API served by `notewise serve`. Requests go through
 * Vite's proxy in development, so everything is same-origin and there is no CORS
 * handling to get wrong.
 */

import type { PermissionReadiness, SetupReadiness } from "../onboarding/readiness";

// Re-exported so callers can keep importing every API type from one place. The shapes are
// defined beside the pure logic that reasons about them, which has no other dependency.
export type { PermissionReadiness, SetupReadiness };

export interface Health {
  status: string;
  schema_version: number;
  /** Whether the AI backend keeps data on this machine. Shown in the UI. */
  ai_local: boolean;
  ai_model: string;
  /**
   * Whether this engine can capture audio.
   *
   * Capture is a compile-time feature and also needs a file-backed database, so the UI has no
   * way to infer it. Offering a record button that silently does nothing is worse than saying
   * plainly that this build cannot record.
   */
  can_record: boolean;
  /** Set when a recording is already running, so a reloaded window recovers the live state. */
  recording_meeting_id: string | null;
}

export interface DownloadState {
  model: string;
  downloaded_bytes: number;
  total_bytes: number;
  percent: number;
  status: "downloading" | "done" | "failed";
  error: string | null;
}

export interface Decision {
  id: string;
  text: string;
  reasoning: string | null;
}

export interface ActionItem {
  id: string;
  text: string;
  owner: string | null;
  due_at: string | null;
  /**
   * Absent on the items nested inside a `Summary`, which the summarize endpoint returns
   * before they have been read back. The dedicated action-item endpoints always set it.
   */
  status?: string;
  meeting_id?: string;
}

export interface Note {
  id: string;
  title: string;
  body: string;
  created_at: string;
  updated_at: string;
}

export interface Person {
  id: string;
  display_name: string;
  email: string | null;
  has_voice_print: boolean;
}

export interface MeetingSeries {
  id: string;
  title: string;
}

/** What a recurring meeting is still carrying from earlier instances. */
export interface Brief {
  series: MeetingSeries | null;
  previous_meeting_id: string | null;
  unfinished_business: ActionItem[];
  recent_decisions: Decision[];
}

export interface Summary {
  id: string;
  text: string;
  model: string;
  created_at: string;
  decisions: Decision[];
  action_items: ActionItem[];
}

export interface DeviceInfo {
  name: string;
  is_default: boolean;
  sample_rate: number;
  channels: number;
}

export interface LanguageOption {
  code: string;
  label: string;
}

export interface RecordingStatus {
  recording: boolean;
  meeting_id: string | null;
  device: string | null;
  model: string | null;
  language: string | null;
  can_record: boolean;
}

export interface RecordingStopped {
  meeting_id: string;
  segments: number;
  speakers: number;
  audio_ms: number;
}

export interface Meeting {
  id: string;
  project_id: string | null;
  title: string;
  source: string;
  started_at: string;
  ended_at: string | null;
}

export interface Segment {
  id: string;
  meeting_id: string;
  speaker: string | null;
  text: string;
  start_ms: number;
  end_ms: number;
  confidence: number | null;
}

export interface ModelInfo {
  name: string;
  size: string;
  bytes: number;
  approx_ram_mb: number;
  multilingual: boolean;
  installed: boolean;
  recommended: boolean;
  /** What choosing this size buys and costs, in a sentence. */
  tradeoff: string;
  /** What the presence or absence of the `.en` suffix means. */
  language_note: string;
}

export interface BackendInfo {
  kind: string;
  label: string;
  is_local: boolean;
  requires_api_key: boolean;
  requires_endpoint: boolean;
  /** Whether this backend can be asked which models it currently holds. */
  lists_models: boolean;
  /** Whether a key is available — from the keychain or the environment. Never the key. */
  has_key: boolean;
}

export type AmbiguityKind =
  | "vague_reference"
  | "unquantified"
  | "unassigned_action"
  | "missing_deadline"
  | "undefined_term"
  | "contradiction"
  | "unstated_rationale";

export interface ClarifyingQuestion {
  question: string;
  about: string;
  kind: AmbiguityKind;
  at_ms: number;
}

export interface Ticket {
  id: string;
  title: string;
  description: string | null;
  status: string;
  owner: string | null;
  due_at: string | null;
}

export interface SearchHit {
  kind: string;
  id: string;
  title: string;
  snippet: string;
  /**
   * The meeting a transcript hit was said in, or null for kinds that belong to no meeting.
   *
   * `id` is the matching row, which is usually not something the UI can open. This is what a
   * result navigates to.
   */
  meeting_id: string | null;
}

export interface RelatedNode {
  kind: string;
  id: string;
  distance: number;
  via: string;
}

/** An error carrying the engine's stable machine-readable code. */
export class ApiError extends Error {
  constructor(
    message: string,
    readonly code: string,
    readonly status: number,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  let response: Response;
  try {
    response = await fetch(path, {
      ...init,
      headers: { "content-type": "application/json", ...init?.headers },
    });
  } catch {
    // A dead engine is the single most likely failure in a desktop app, and
    // "Failed to fetch" tells a user nothing actionable.
    throw new ApiError(
      "Cannot reach the Notewise engine. Is it running?",
      "engine_unreachable",
      0,
    );
  }

  if (!response.ok) {
    const body = await response.json().catch(() => null);
    throw new ApiError(
      body?.error ?? `Request failed (${response.status})`,
      body?.code ?? "unknown",
      response.status,
    );
  }

  return response.json() as Promise<T>;
}

export const api = {
  health: () => request<Health>("/health"),

  meetings: (limit = 50) => request<Meeting[]>(`/v1/meetings?limit=${limit}`),

  createMeeting: (title: string, source = "combined") =>
    request<Meeting>("/v1/meetings", {
      method: "POST",
      body: JSON.stringify({ title, source }),
    }),

  endMeeting: (id: string) =>
    request<Meeting>(`/v1/meetings/${id}/end`, { method: "POST" }),

  transcript: (id: string) => request<Segment[]>(`/v1/meetings/${id}/transcript`),

  appendSegments: (
    id: string,
    segments: Array<{
      text: string;
      start_ms: number;
      end_ms: number;
      speaker?: string | null;
    }>,
  ) =>
    request<{ appended: number; ids: string[] }>(
      `/v1/meetings/${id}/transcript`,
      { method: "POST", body: JSON.stringify(segments) },
    ),

  summarize: (id: string) =>
    request<{
      summary_id: string;
      text: string;
      model: string;
      decisions: number;
      action_items: number;
    }>(`/v1/meetings/${id}/summarize`, { method: "POST" }),

  related: (id: string, depth = 2) =>
    request<RelatedNode[]>(`/v1/meetings/${id}/related?depth=${depth}`),

  /**
   * URL for a Markdown export.
   *
   * Returned as a URL rather than fetched: the browser's own download handling gets the
   * filename from Content-Disposition, which a blob built in JS would lose.
   */
  exportUrl: (id: string, variant: "full" | "brief" | "transcript" = "full") =>
    `/v1/meetings/${id}/export?variant=${variant}`,

  questions: (id: string) =>
    request<{ questions: ClarifyingQuestion[]; reason?: string }>(
      `/v1/meetings/${id}/questions`,
      { method: "POST", body: JSON.stringify({}) },
    ),

  chat: (id: string, messages: Array<{ role: string; content: string }>) =>
    request<{ text: string; model: string }>(`/v1/meetings/${id}/chat`, {
      method: "POST",
      body: JSON.stringify({ messages }),
    }),

  backends: () =>
    request<{
      backends: BackendInfo[];
      /** `kind` matches an entry in `backends`, so the active one can be named. */
      active: { kind: string; model: string; is_local: boolean };
    }>("/v1/backends"),

  models: () =>
    request<{ models: ModelInfo[]; directory: string }>("/v1/models"),

  /**
   * Which models a local backend currently holds.
   *
   * Never rejects for a stopped daemon — `available: false` with a reason is the normal
   * answer, because the engine's default model id is a guess and the whole point of this call
   * is to replace that guess with the exact tags installed on this machine.
   */
  backendModels: (kind: string) =>
    request<{ models: string[]; available: boolean; reason: string | null }>(
      `/v1/backends/${encodeURIComponent(kind)}/models`,
    ),

  /**
   * Start a download. Returns as soon as it has started, not when it finishes.
   *
   * `large-v3` is 3.1 GB — a request held open that long is indistinguishable from a hang, and
   * a retry would start a second copy. Follow it with {@link watchDownload}.
   */
  downloadModel: (name: string) =>
    request<DownloadState>(`/v1/models/${encodeURIComponent(name)}/download`, {
      method: "POST",
    }),

  downloads: () => request<DownloadState[]>("/v1/downloads"),

  /**
   * What first-run setup still needs.
   *
   * Never prompts — permission status is read without opening a device, so calling this on
   * mount cannot raise an OS dialog before the user has pressed anything.
   */
  setup: () => request<SetupReadiness>("/v1/setup"),

  /**
   * Mark setup finished.
   *
   * Rejects with a 409 when a required step is unsatisfied, so a UI bug cannot let anyone past
   * the gate by accident. `skip` is the deliberate override, for a user who cannot satisfy a
   * step — it records completion and reports back what was left unresolved. Completing twice
   * returns the original timestamp.
   */
  completeSetup: (skip = false) =>
    request<{ completed_at: string; skipped: string[] }>(
      `/v1/setup/complete${skip ? "?skip=true" : ""}`,
      { method: "POST" },
    ),

  /** Ask the OS for a capability. May raise a permission dialog. */
  requestPermission: (kind: "microphone" | "system_audio") =>
    request<PermissionReadiness>(`/v1/permissions/${kind}`, { method: "POST" }),

  /**
   * Stream a download's progress.
   *
   * Returns a cancel function. The stream is closed automatically on the terminal event, so a
   * caller that only wants progress does not have to remember to tear it down — but a caller
   * that navigates away mid-download does.
   */
  watchDownload: (
    name: string,
    onProgress: (state: DownloadState) => void,
    onDone: (state: DownloadState) => void,
    onError: (message: string) => void,
  ): (() => void) => {
    const source = new EventSource(
      `/v1/models/${encodeURIComponent(name)}/download`,
    );

    const finish = (state: DownloadState) => {
      source.close();
      onDone(state);
    };

    source.addEventListener("progress", (event) =>
      onProgress(JSON.parse((event as MessageEvent).data) as DownloadState),
    );
    source.addEventListener("done", (event) =>
      finish(JSON.parse((event as MessageEvent).data) as DownloadState),
    );
    source.addEventListener("failed", (event) => {
      const state = JSON.parse((event as MessageEvent).data) as DownloadState;
      source.close();
      onError(state.error ?? "The download failed.");
    });

    // EventSource retries by itself, so this fires on transient drops too. Only treat it as
    // fatal once the browser has actually given up.
    source.onerror = () => {
      if (source.readyState === EventSource.CLOSED) {
        onError("Lost contact with the engine during the download.");
      }
    };

    return () => source.close();
  },

  /** The stored summary, or null when the meeting has not been summarized. */
  summary: (id: string) =>
    request<{ summary: Summary | null }>(`/v1/meetings/${id}/summary`),

  recordingStatus: () => request<RecordingStatus>("/v1/recording"),

  devices: () =>
    request<{ devices: DeviceInfo[]; available: boolean; error?: string }>(
      "/v1/devices",
    ),

  languages: () => request<{ languages: LanguageOption[] }>("/v1/languages"),

  /**
   * Interface preferences, kept by the engine.
   *
   * Not `localStorage`: the shell binds port 0, so the window's origin changes every launch
   * and anything stored per-origin is gone by the next one.
   */
  preferences: () => request<Record<string, unknown>>("/v1/preferences"),

  /** Whether voices are remembered between meetings, and how many are stored. */
  voiceprints: () =>
    request<{ enabled: boolean; stored: number }>("/v1/voiceprints"),

  /** Turning it off also erases what is stored — the engine does both. */
  setVoiceprintsEnabled: (enabled: boolean) =>
    request<{ enabled: boolean; stored: number }>("/v1/voiceprints", {
      method: "POST",
      body: JSON.stringify({ enabled }),
    }),

  forgetVoiceprints: () =>
    request<{ erased: number }>("/v1/voiceprints", { method: "DELETE" }),

  setPreferences: (prefs: Record<string, unknown>) =>
    request<Record<string, unknown>>("/v1/preferences", {
      method: "POST",
      body: JSON.stringify(prefs),
    }),

  /**
   * Save a provider API key.
   *
   * Goes to the OS keychain and is never readable back — the listing reports only whether a
   * key exists. Switches to the backend on success, so adding a key is one action.
   */
  setApiKey: (kind: string, key: string) =>
    request<{ kind: string; has_key: boolean; model: string }>(
      `/v1/backends/${encodeURIComponent(kind)}/key`,
      { method: "POST", body: JSON.stringify({ key }) },
    ),

  deleteApiKey: (kind: string) =>
    request<void>(`/v1/backends/${encodeURIComponent(kind)}/key`, { method: "DELETE" }),

  /**
   * Switch the active AI backend.
   *
   * No API key is sent: the engine reads keys from its own environment, so a key never
   * travels over HTTP — not even on loopback — and never lands in a log.
   */
  switchBackend: (kind: string, model?: string, endpoint?: string) =>
    request<{ kind: string; model: string; is_local: boolean }>("/v1/backend", {
      method: "POST",
      body: JSON.stringify({ kind, model, endpoint }),
    }),

  /** Transcribe a file already on this machine into a new meeting. */
  importAudio: (options: {
    path: string;
    title?: string;
    model?: string;
    language?: string;
  }) =>
    request<{
      meeting_id: string;
      segments: number;
      speakers: number;
      audio_ms: number;
    }>("/v1/import", { method: "POST", body: JSON.stringify(options) }),

  /**
   * Start capturing. The engine creates the meeting as part of this call, so there is never a
   * meeting that exists with nothing recording into it.
   */
  startRecording: (
    options: {
      title?: string;
      device?: string;
      model?: string;
      language?: string;
    } = {},
  ) =>
    request<RecordingStatus>("/v1/recording", {
      method: "POST",
      body: JSON.stringify(options),
    }),

  /** Stop capturing. Resolves once the tail of the transcript is flushed and diarized. */
  stopRecording: () =>
    request<RecordingStopped>("/v1/recording", { method: "DELETE" }),

  tickets: () => request<Ticket[]>("/v1/tickets"),

  createTicket: (input: {
    title: string;
    description?: string;
    owner?: string;
    meeting_id?: string;
  }) =>
    request<Ticket>("/v1/tickets", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  /**
   * Partial edit. An omitted field is left alone, never blanked — pass `clear_owner` or
   * `clear_due_at` to actually remove one. Getting this wrong wipes fields the user never
   * touched, so the asymmetry is deliberate on both sides of the wire.
   */
  updateTicket: (
    id: string,
    patch: {
      title?: string;
      description?: string;
      owner?: string;
      status?: string;
      clear_owner?: boolean;
      clear_due_at?: boolean;
    },
  ) =>
    request<Ticket>(`/v1/tickets/${id}`, {
      method: "PATCH",
      body: JSON.stringify(patch),
    }),

  deleteTicket: (id: string) =>
    request<{ deleted: boolean }>(`/v1/tickets/${id}`, { method: "DELETE" }),

  /**
   * Every action item from a meeting.
   *
   * Meeting-scoped rather than summary-scoped, so items typed by hand and items whose
   * summary was regenerated both appear. `summary.action_items` is the narrower view.
   */
  actionItems: (meetingId: string) =>
    request<ActionItem[]>(`/v1/meetings/${meetingId}/action-items`),

  createActionItem: (
    meetingId: string,
    input: { text: string; owner?: string; due_at?: string },
  ) =>
    request<ActionItem>(`/v1/meetings/${meetingId}/action-items`, {
      method: "POST",
      body: JSON.stringify(input),
    }),

  updateActionItem: (
    id: string,
    patch: {
      status?: string;
      owner?: string;
      due_at?: string;
      clear_owner?: boolean;
      clear_due_at?: boolean;
    },
  ) =>
    request<ActionItem>(`/v1/action-items/${id}`, {
      method: "PATCH",
      body: JSON.stringify(patch),
    }),

  deleteActionItem: (id: string) =>
    request<{ deleted: boolean }>(`/v1/action-items/${id}`, {
      method: "DELETE",
    }),

  /**
   * Turn an action item into a ticket. The item is kept — it is the record that the
   * meeting produced this work. Calling twice returns the existing ticket.
   */
  promoteActionItem: (id: string) =>
    request<Ticket>(`/v1/action-items/${id}/promote`, { method: "POST" }),

  decisions: (meetingId: string) =>
    request<Decision[]>(`/v1/meetings/${meetingId}/decisions`),

  createDecision: (
    meetingId: string,
    input: { text: string; reasoning?: string },
  ) =>
    request<Decision>(`/v1/meetings/${meetingId}/decisions`, {
      method: "POST",
      body: JSON.stringify(input),
    }),

  notes: (limit = 50) => request<Note[]>(`/v1/notes?limit=${limit}`),

  note: (id: string) => request<Note>(`/v1/notes/${id}`),

  createNote: (input: { title: string; body: string }) =>
    request<Note>("/v1/notes", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  updateNote: (id: string, title: string, body: string) =>
    request<Note>(`/v1/notes/${id}`, {
      method: "PUT",
      body: JSON.stringify({ title, body }),
    }),

  deleteNote: (id: string) =>
    request<{ deleted: boolean }>(`/v1/notes/${id}`, { method: "DELETE" }),

  people: () => request<Person[]>("/v1/people"),

  participants: (meetingId: string) =>
    request<Person[]>(`/v1/meetings/${meetingId}/participants`),

  addParticipant: (
    meetingId: string,
    input: { person_id?: string; display_name?: string; role?: string },
  ) =>
    request<Person>(`/v1/meetings/${meetingId}/participants`, {
      method: "POST",
      body: JSON.stringify(input),
    }),

  personMeetings: (personId: string) =>
    request<Meeting[]>(`/v1/people/${personId}/meetings`),

  /** Thread this meeting into a series. With no arguments, threads on its own title. */
  assignSeries: (
    meetingId: string,
    input: { series_id?: string; title?: string; clear?: boolean } = {},
  ) =>
    request<{ series: MeetingSeries | null }>(
      `/v1/meetings/${meetingId}/series`,
      { method: "POST", body: JSON.stringify(input) },
    ),

  /**
   * What a recurring meeting is still carrying. Empty for a one-off, which is an ordinary
   * state rather than an error.
   */
  brief: (meetingId: string) =>
    request<Brief>(`/v1/meetings/${meetingId}/brief`),

  search: (query: string, limit = 25) =>
    request<SearchHit[]>(
      `/v1/search?q=${encodeURIComponent(query)}&limit=${limit}`,
    ),
};
