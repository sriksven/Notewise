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
  /** The engine's version. Separately versioned from this frontend, so it is reported. */
  version: string;
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
  /**
   * Which catalogue this belongs to.
   *
   * One engine-side manager tracks both, but progress is streamed by per-catalogue routes — so a
   * caller reading {@link ApiClient.downloads} must check this before watching an entry. Watching
   * a `speaker` download through the transcription route answers 400.
   */
  kind: "transcription" | "speaker";
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
  /** When it was moved to the trash, or null while it is live. */
  deleted_at: string | null;
}

/** One piece of source material behind a grounded answer. */
export interface Citation {
  /** Its number in the answer's `[n]` references. */
  n: number;
  kind: "meeting" | "note" | "ticket";
  id: string;
  title: string;
}

export interface GroundedAnswer {
  text: string;
  model: string;
  citations: Citation[];
  /**
   * Whether there was any material behind the answer at all.
   *
   * False only when nothing was found — which is worth telling the user about, since retrieval
   * is by word and a rewording may find it.
   */
  grounded: boolean;
}

/** A sink this build contains, whether or not it has been turned on. */
export interface AvailableConnector {
  id: string;
  display_name: string;
  /** Whether it keeps data on this machine. The fact worth showing before it is enabled. */
  is_local: boolean;
  target_label: string;
  target_hint: string;
  description: string;
  connected: boolean;
}

export interface FailedDelivery {
  id: string;
  connector_id: string;
  node_kind: string;
  node_id: string;
  attempts: number;
  last_error: string | null;
}

/** The semantic index's state. */
export interface IndexStatus {
  state: "idle" | "running" | "done" | "failed";
  /** The embedding model these vectors belong to. */
  model: string;
  /** Whether the local embedder answered when last asked. */
  available: boolean;
  total: number;
  done: number;
  chunks: number;
  /** Vectors from another model, which can never be compared against the current one. */
  stale_from_other_models: number;
  error: string | null;
  started_at: string | null;
  finished_at: string | null;
}

export interface AgentStep {
  n: number;
  /** The tool it used, or `think` when it produced no usable action. */
  action: string;
  /** Its own one-line reason, when it gave one. */
  reason: string | null;
  observation: string;
}

export interface AgentRun {
  id: string;
  task: string;
  status: "running" | "done" | "failed";
  steps: AgentStep[];
  note_id: string | null;
  note_title: string | null;
  result: string | null;
  error: string | null;
  started_at: string;
  finished_at: string | null;
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
  /** When it was moved to the trash, or null while it is live. */
  deleted_at: string | null;
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

/** Whether acoustic speaker separation will run, and what is stopping it if not. */
export interface DiarizationStatus {
  mode: "off" | "acoustic";
  model: string;
  retain_minutes: number;
  /** Whether this build has the feature compiled in at all. */
  supported: boolean;
  model_installed: boolean;
  /** True only when all three conditions hold. */
  effective: boolean;
  /** Why it will not run. `null` when it will. */
  blocked_by: string | null;
}

export interface SpeakerModel {
  name: string;
  bytes: number;
  approx_mb: number;
  installed: boolean;
  selected: boolean;
  recommended: boolean;
  tradeoff: string;
}

/** One distinct voice in a meeting, with enough weight to judge it by. */
export interface Speaker {
  /** `null` for segments nothing ever labelled. Nameable like any other. */
  label: string | null;
  segments: number;
  speaking_ms: number;
  first_at_ms: number;
  /** A diarizer label rather than a person's name — the UI offers to fix these. */
  anonymous: boolean;
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

/**
 * One condition on a request, in the shape the engine serializes.
 *
 * A tagged union rather than a flat object with optional fields, because that is what
 * `ai_router::Predicate` is — and flattening it here would mean this file deciding which
 * combinations are legal, which the engine already validates.
 */
export type RoutingPredicate =
  | { task: string[] }
  | { input_tokens_over: number }
  | { input_tokens_under: number }
  | { text_contains: string[] }
  | { hour_between: [number, number] }
  | { local_backend_healthy: boolean };

export interface RoutingRule {
  name: string;
  /** All must hold. Empty matches every request, which makes any rule below it unreachable. */
  when: RoutingPredicate[];
  backend: string;
  model?: string;
  endpoint?: string;
  redaction?: string;
}

export interface RoutingRules {
  rules: RoutingRule[];
  /**
   * Names the running engine actually built, in evaluation order.
   *
   * A stored rule missing from here failed to construct — usually a lost API key — and is not in
   * force. Showing only `rules` would present it as working.
   */
  active: string[];
}

export interface RoutingExplainQuery {
  task?: string;
  estimated_tokens?: number;
  text?: string;
  hour_of_day?: number;
}

export interface RoutingExplain {
  /** Human-readable: the rule that matched and the provider it reaches. */
  decision: string;
  task: string;
  estimated_tokens: number;
  hour_of_day: number;
}

export interface MergeResult {
  applied: boolean;
  summary: string;
  meetings: number;
  transcript_segments: number;
  notes: number;
  people_added: number;
  people_merged: number;
  skipped_conflicts: number;
}

export interface PendingNotification {
  id: string;
  /** What triggered it, matching graph node naming: `meeting`, `action_item`, and so on. */
  source_kind: string;
  source_id: string;
  body: string;
  created_at: string;
}

export interface EmailDraft {
  id: string;
  meeting_id: string | null;
  subject: string;
  body: string;
  recipients: string[];
  /** `draft` | `approved` | `sent` | `discarded`. Nothing here ever sets `sent`. */
  status: string;
  /** Which tone produced this variant. */
  variant: string | null;
  created_at: string;
}

export interface SummaryTemplate {
  id: string;
  name: string;
  prompt: string;
  /** Seeded. Editable, but not deletable — so the delete control is hidden rather than failing. */
  is_builtin: boolean;
}

export interface Job {
  id: string;
  name: string;
  prompt: string;
  cron: string;
  timezone: string;
  enabled: boolean;
  catch_up: boolean;
  timeout_secs: number;
  /** When it would next fire, or null if the expression no longer parses. */
  next_fire: string | null;
}

export interface JobRun {
  id: string;
  /** `running` | `completed` | `failed` | `timed_out` | `skipped`. */
  status: string;
  trace: { n: number; action: string; reason: string | null; observation: string }[] | null;
  note_id: string | null;
  /** External tool calls proposed. Always zero until there is a way to confirm one. */
  proposals: number;
  error: string | null;
  started_at: string;
  finished_at: string | null;
}

export interface ExtractionStatus {
  enabled: boolean;
  /** Meetings never read for facts. */
  unprocessed: number;
  would_run: boolean;
  /** Why not, when it would not. */
  blocked_by: string | null;
}

/** What a candidate fact became. */
export interface ExtractionDecision {
  text: string;
  verdict: "kept" | "duplicate" | "third_party" | "secret" | "unusable";
  reason: string | null;
}

/**
 * What a run did, and what it decided not to do.
 *
 * The decisions are the useful part: "why does it not remember that" and "why does it think that"
 * are the two questions this feature generates, and a trace is the only honest answer to either.
 */
export interface ExtractionReport {
  skipped: string | null;
  meetings_read: number;
  proposed: number;
  kept: number;
  decisions: ExtractionDecision[];
}

export interface MemoryItem {
  id: string;
  scope: string;
  project_id: string | null;
  text: string;
  /** `manual` or `extracted`, so what you wrote is distinguishable from what was inferred. */
  origin: string;
  source_meeting_id: string | null;
  created_at: string;
}

/**
 * An external MCP server, as configured.
 *
 * Two switches, both off by default: `enabled` for the server and a row in `enabled_tools` per
 * tool. A server that is added and forgotten reaches nothing.
 */
export interface McpServerInfo {
  id: string;
  name: string;
  transport: "stdio" | "http";
  command: string | null;
  args: string[];
  url: string | null;
  enabled: boolean;
  auto_start: boolean;
  /** Whether a process or session exists right now. Servers start on first use, not at launch. */
  running: boolean;
  /** Which of its tools are allowed. Independent of `enabled`, so turning it off loses nothing. */
  enabled_tools: string[];
  /** Whether credentials are held for it. The values are never returned. */
  has_secrets: boolean;
}

export interface McpTool {
  name: string;
  description: string | null;
  /** The tool's own JSON Schema, as it published it. */
  input_schema: unknown;
  enabled: boolean;
}

export interface McpDiscovery {
  tools: McpTool[];
  /**
   * Why the list is empty, when it is. A field rather than a failed request, so a server that
   * will not start shows its reason instead of looking like a server with no tools.
   */
  error: string | null;
  running: boolean;
}

/**
 * One proposed, confirmed, or completed external call.
 *
 * `unknown` is not a synonym for `failed`. It means the call timed out and may have taken effect,
 * which is the difference between "try again" and "check the other system first".
 */
export interface ToolExecution {
  id: string;
  action_item_id: string | null;
  server_id: string;
  tool_name: string;
  /** Exactly what will be sent. Shown verbatim, because that is what the confirmation is for. */
  arguments: Record<string, unknown> | string;
  status: "proposed" | "confirmed" | "succeeded" | "failed" | "unknown" | "rejected";
  result: string | null;
  proposed_at: string;
  executed_at: string | null;
  outcome_unknown: boolean;
}

export interface ToolProposalResult {
  /** The stored proposal. Nothing has been sent. */
  execution: ToolExecution | null;
  /** Why there is none. Shown verbatim. */
  declined: string | null;
  /** How many tools the model was shown, so "it ignored my tool" has an answer. */
  tools_considered: number;
}

/** One OS permission the assistant needs, and what to do about it. */
export interface AssistantPermission {
  capability: "microphone" | "accessibility" | "screen_recording" | "input_monitoring";
  /** Spelled the way System Settings spells it, so the row is findable. */
  label: string;
  status: "granted" | "denied" | "not_determined" | "unknown";
  /** Present when it is not granted. Names the pane and says a restart is needed. */
  how_to_grant: string | null;
  settings_url: string;
}

export interface AssistantCapabilities {
  /** Whether this build can capture and transcribe. */
  can_dictate: boolean;
  /** Whether this build can put text into another application. */
  can_insert: boolean;
  reason: string | null;
  /** The dictation hotkey. Its own field so a client written before the panel existed still works. */
  hotkey: string;
  /** Every feature's binding, including the one above. */
  hotkeys: Array<{ feature: string; hotkey: string }>;
  mode: "raw" | "cleaned";
  permissions: AssistantPermission[];
}

export interface DictationStatus {
  supported: boolean;
  reason: string | null;
  listening: boolean;
  started_at: string | null;
  mode: "raw" | "cleaned" | null;
}

/**
 * How dictated text got where it went.
 *
 * `clipboard_restored: false` means the clipboard was borrowed and could not be put back — worth
 * telling the user at the time rather than letting them find out at their next paste.
 */
export type Insertion =
  | "accessibility"
  | { clipboard: { clipboard_restored: boolean } }
  | { refused: { reason: string } };

export interface Dictated {
  text: string;
  /** What was heard before cleaning, when cleaning changed it. */
  raw_text: string | null;
  mode: "raw" | "cleaned";
  insertion: Insertion | null;
  /** One sentence for the user, when there is something worth saying. */
  note: string | null;
  duration_ms: number;
}

/** What to do to a piece of highlighted text. */
export type AssistantAct =
  | "rewrite"
  | "shorten"
  | "expand"
  | "fix_grammar"
  | "formalise"
  | "summarise"
  | "explain"
  | { translate: { language: string } };

export interface AssistantAction {
  action: AssistantAct;
  label: string;
  /** Whether choosing this replaces the selection or produces something new. */
  replaces: boolean;
}

export interface ScreenContext {
  app: string | null;
  window_title: string | null;
  selection: string | null;
  focused_text: string | null;
  /** Read from pixels rather than from the application. May contain recognition errors. */
  recognised_text: string | null;
}

export interface ScreenAnswer {
  text: string;
  model: string;
  /** Whether there was any screen context behind the answer. */
  grounded: boolean;
  /** Exactly what was put in front of the model, so "what did you send" is answerable. */
  context: ScreenContext | null;
  context_prompt: string;
}

export interface SelectionInfo {
  text: string | null;
  /** Whether the target will accept a replacement. */
  replaceable: boolean;
  length: number;
}

export interface ActResult {
  action: AssistantAct;
  original: string;
  result: string;
  model: string;
  insertion: Insertion | null;
  note: string | null;
}

/** Why nothing is being suggested — the question inline completion generates most. */
export type CompletionDecision =
  | "ask"
  | "still_typing"
  | "idle"
  | "too_short"
  | "too_long"
  | "too_soon";

export interface Completion {
  /** Already spaced correctly for insertion at the caret. */
  suggestion: string | null;
  decision: CompletionDecision;
  model: string | null;
  text: string;
}

export interface TypingActivity {
  running: boolean;
  last_keystroke_ms: number | null;
  keystrokes: number;
}

/**
 * A meeting that appears to have started, and has not been answered.
 *
 * Held in the engine's memory rather than stored: the whole lifetime of one is the few minutes at
 * the start of a meeting, and an offer to record something that finished yesterday is worse than
 * none.
 */
export interface JoinOffer {
  id: string;
  /** What the recording would be called. */
  title: string;
  platform: "meet" | "zoom" | "teams" | "unknown";
  /** The queued notification, so showing the offer in-app can mark it delivered rather than
   *  letting the user be told the same thing twice. */
  notification_id: string | null;
  created_at: string;
  expires_in_secs: number;
}

/** What meeting detection can currently see, and what it cannot. */
export interface DetectionStatus {
  sources: Array<{
    source: "extension" | "calendar" | "native";
    /** Null means it has never reported — for the extension, that is the whole answer. */
    last_seen_at: string | null;
  }>;
  offers: number;
  calendar_connected: boolean;
  grace_secs: number;
  /** Stated rather than left to be discovered: what this cannot detect. */
  blind_spot: string;
}

/**
 * A vault file edited outside Notewise.
 *
 * The mirror pauses for it rather than overwriting, which is the promise the vault makes — and until
 * now the pause was silent.
 */
export interface VaultDivergence {
  id: string;
  path: string;
  /** What a person recognises. */
  file_name: string;
  detected_at: string;
  meeting_id: string | null;
  meeting_title: string | null;
  /** What you wrote, so the choice is made looking at it. Null when the file cannot be read. */
  current_content: string | null;
}

export interface MirrorResult {
  outcome: "written" | "diverged" | "unavailable";
  path: string | null;
  divergence_id: string | null;
  message: string;
}

export interface DivergenceResolved {
  resolution: "kept" | "overwritten" | "copied_to_note";
  /** Where your writing went, when it was kept as a note. */
  note_id: string | null;
  mirror: MirrorResult | null;
  message: string;
}

export const api = {
  health: () => request<Health>("/health"),

  /** Write a meeting to the connected vault folder. Refuses rather than overwriting an edit. */
  mirrorMeeting: (meetingId: string) =>
    request<MirrorResult>(`/v1/meetings/${meetingId}/mirror`, { method: "POST" }),

  /** Vault files edited outside Notewise and not yet answered. */
  vaultDivergences: () => request<VaultDivergence[]>("/v1/vault/divergences"),

  /**
   * Settle one. `copy_to_note` is the only choice that loses nothing — it saves what you wrote as a
   * note before refreshing the file.
   */
  resolveDivergence: (id: string, resolution: "keep" | "overwrite" | "copy_to_note") =>
    request<DivergenceResolved>(`/v1/vault/divergences/${id}/resolve`, {
      method: "POST",
      body: JSON.stringify({ resolution }),
    }),

  /** What meeting detection can see. */
  detectionStatus: () => request<DetectionStatus>("/v1/signals/join"),

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

  speakers: (id: string) =>
    request<{ speakers: Speaker[] }>(`/v1/meetings/${id}/speakers`),

  /**
   * Rename a speaker — which is also how two are merged.
   *
   * Passing a `to` that another speaker already has folds the two together. That is the fix
   * for a diarizer having split one person in two, and the server reports `merged` so the UI
   * can say which of the two things happened.
   *
   * `from: null` names the segments nothing ever labelled.
   */
  renameSpeaker: (id: string, from: string | null, to: string) =>
    request<{ segments_changed: number; merged: boolean; speakers: Speaker[] }>(
      `/v1/meetings/${id}/speakers/rename`,
      { method: "POST", body: JSON.stringify({ from, to }) },
    ),

  // ---------------------------------------------------------------- jobs

  jobs: () => request<Job[]>("/v1/jobs"),

  createJob: (job: {
    name: string;
    prompt: string;
    cron: string;
    timezone?: string;
    catch_up?: boolean;
  }) => request<Job>("/v1/jobs", { method: "POST", body: JSON.stringify(job) }),

  deleteJob: (id: string) => request<{ deleted: boolean }>(`/v1/jobs/${id}`, { method: "DELETE" }),

  setJobEnabled: (id: string, enabled: boolean) =>
    request<Job>(`/v1/jobs/${id}/enabled`, {
      method: "PUT",
      body: JSON.stringify({ enabled }),
    }),

  jobRuns: (id: string) => request<JobRun[]>(`/v1/jobs/${id}/runs`),

  /** Run a job now, regardless of its schedule. Refused while one is already going. */
  runJob: (id: string) =>
    request<{ run_id: string }>(`/v1/jobs/${id}/run`, { method: "POST", body: "{}" }),

  /** Check a cron expression before saving it, rather than discovering it is wrong at 3am. */
  previewSchedule: (cron: string, timezone?: string) =>
    request<{ timezone: string; next: string[] }>("/v1/jobs/preview", {
      method: "POST",
      body: JSON.stringify({ cron, timezone }),
    }),

  // ---------------------------------------------------------------- memories

  memories: () =>
    request<{
      memories: MemoryItem[];
      global_used: number;
      global_cap: number;
      project_cap: number;
    }>("/v1/memories"),

  createMemory: (text: string, scope?: "global" | "project", project_id?: string) =>
    request<MemoryItem>("/v1/memories", {
      method: "POST",
      body: JSON.stringify({ text, scope, project_id }),
    }),

  updateMemory: (id: string, text: string) =>
    request<MemoryItem>(`/v1/memories/${id}`, { method: "PUT", body: JSON.stringify({ text }) }),

  deleteMemory: (id: string) =>
    request<{ deleted: boolean }>(`/v1/memories/${id}`, { method: "DELETE" }),

  /** Whether the app reads meetings for durable facts, and why a run would not happen. */
  extractionStatus: () => request<ExtractionStatus>("/v1/memories/extraction"),

  setExtractionEnabled: (enabled: boolean) =>
    request<{ enabled: boolean }>("/v1/memories/extraction", {
      method: "PUT",
      body: JSON.stringify({ enabled }),
    }),

  /** Read recent meetings now, ignoring the gates. Returns what it decided about each candidate. */
  runExtraction: () => request<ExtractionReport>("/v1/memories/extract", { method: "POST" }),

  /** Whether a meeting has retained audio to play, and how large it is. */
  audioInfo: (meetingId: string) =>
    request<{ available: boolean; bytes: number }>(`/v1/meetings/${meetingId}/audio/info`),

  /**
   * URL the player loads.
   *
   * Not a `request` call: the browser fetches this itself so it can issue range requests and seek,
   * which is the whole point of the endpoint.
   */
  audioUrl: (meetingId: string) => `/v1/meetings/${meetingId}/audio`,

  /** How long retained audio is kept, and how much there is. */
  audioRetention: () =>
    request<{
      policy: string;
      retained: number;
      bytes: number;
      can_enable: boolean;
      blocked_by: string | null;
    }>("/v1/audio/retention"),

  setAudioRetention: (policy: string) =>
    request<{ policy: string; retained: number; bytes: number }>("/v1/audio/retention", {
      method: "PUT",
      body: JSON.stringify({ policy }),
    }),

  /** Named prompts for summarising. Built-ins first, then the user's own. */
  summaryTemplates: () => request<SummaryTemplate[]>("/v1/summary-templates"),

  createSummaryTemplate: (name: string, prompt: string) =>
    request<SummaryTemplate>("/v1/summary-templates", {
      method: "POST",
      body: JSON.stringify({ name, prompt }),
    }),

  updateSummaryTemplate: (id: string, name: string, prompt: string) =>
    request<SummaryTemplate>(`/v1/summary-templates/${id}`, {
      method: "PUT",
      body: JSON.stringify({ name, prompt }),
    }),

  deleteSummaryTemplate: (id: string) =>
    request<{ deleted: boolean }>(`/v1/summary-templates/${id}`, { method: "DELETE" }),

  /** Rename a meeting. */
  setMeetingTitle: (id: string, title: string) =>
    request<{ title: string }>(`/v1/meetings/${id}/title`, {
      method: "PUT",
      body: JSON.stringify({ title }),
    }),

  /**
   * Correct a mis-transcribed line.
   *
   * The engine refuses an empty string: blanking a line is a different operation from correcting
   * one, and it would leave a gap with no record anything was there.
   */
  setSegmentText: (id: string, text: string) =>
    request<{ text: string }>(`/v1/segments/${id}/text`, {
      method: "PUT",
      body: JSON.stringify({ text }),
    }),

  summarize: (id: string) =>
    request<{
      summary_id: string;
      text: string;
      model: string;
      decisions: number;
      action_items: number;
    }>(`/v1/meetings/${id}/summarize`, { method: "POST" }),

  /** Summarize using a template's prompt instead of the backend's default instruction. */
  summarizeWithTemplate: (id: string, templateId: string) =>
    request<{
      summary: Summary;
      decisions: unknown[];
      action_items: unknown[];
    }>(`/v1/meetings/${id}/summarize?template=${encodeURIComponent(templateId)}`, {
      method: "POST",
    }),

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

  /** The routing policy: what is stored, and which rules the running engine actually built. */
  routingRules: () => request<RoutingRules>("/v1/routing/rules"),

  /** Replace the policy. The engine validates before storing and names any rule that could never run. */
  saveRoutingRules: (rules: RoutingRule[]) =>
    request<RoutingRules>("/v1/routing/rules", {
      method: "PUT",
      body: JSON.stringify({ rules }),
    }),

  /**
   * Where a request like this would go, without sending it.
   *
   * The answer to "why did that cost anything", which is the question that decides whether someone
   * trusts routing or turns it off.
   */
  explainRouting: (query: RoutingExplainQuery) =>
    request<RoutingExplain>("/v1/routing/explain", {
      method: "POST",
      body: JSON.stringify(query),
    }),

  /** Install the starting policy: heavy work to a chosen backend, everything else to the default. */
  installDefaultRouting: (backend: string, model?: string) =>
    request<RoutingRules>("/v1/routing/default", {
      method: "POST",
      body: JSON.stringify({ quality_backend: backend, quality_model: model }),
    }),

  /**
   * Fold another workspace into this one.
   *
   * `dryRun` is required rather than optional so a call site cannot omit it and mutate by
   * accident. The engine also defaults it to true, but a caller should have to say which it meant.
   */
  mergeWorkspace: (from: string, dryRun: boolean) =>
    request<MergeResult>("/v1/workspace/merge", {
      method: "POST",
      body: JSON.stringify({ from, dry_run: dryRun }),
    }),

  /** Desktop notifications the engine has queued and nothing has shown yet. */
  pendingNotifications: () => request<PendingNotification[]>("/v1/notifications/pending"),

  /** Record that one was actually shown. Called after display, never before. */
  markNotificationDelivered: (id: string) =>
    request<{ delivered: boolean }>(`/v1/notifications/${id}/delivered`, { method: "POST" }),

  /** Follow-up drafts for a meeting. */
  emailDrafts: (meetingId: string) =>
    request<EmailDraft[]>(`/v1/meetings/${meetingId}/emails`),

  /** Draft one or more follow-ups. Tones are the variants to generate. */
  draftEmails: (meetingId: string, tones: string[], sender?: string, audience?: string) =>
    request<EmailDraft[]>(`/v1/meetings/${meetingId}/emails`, {
      method: "POST",
      body: JSON.stringify({ tones, sender, audience }),
    }),

  /**
   * Mark a draft approved.
   *
   * Approving is not sending. There is no send endpoint anywhere in this product, so this records
   * that a human read it and was happy — nothing leaves the machine as a result.
   */
  approveEmailDraft: (id: string) =>
    request<EmailDraft>(`/v1/emails/${id}/approve`, { method: "POST" }),

  discardEmailDraft: (id: string) => request<void>(`/v1/emails/${id}`, { method: "DELETE" }),

  /**
   * Where to download a draft as a file any mail client can open.
   *
   * A URL rather than a fetch: the browser saving a file is the whole point, and reading the bytes
   * into memory to hand them straight back would be a round trip for nothing. The route out for
   * anybody with no mailbox connected, which is everybody on a first run.
   */
  emailDraftFileUrl: (id: string) => `/v1/emails/${id}/eml`,

  diarization: () => request<DiarizationStatus>("/v1/diarization"),

  /**
   * Change the setting.
   *
   * Turning it on before the model has downloaded is allowed on purpose — the setting is intent,
   * and `blocked_by` explains what is still missing.
   */
  setDiarization: (patch: {
    mode?: "off" | "acoustic";
    model?: string;
    retain_minutes?: number;
  }) =>
    request<DiarizationStatus>("/v1/diarization", {
      method: "PUT",
      body: JSON.stringify(patch),
    }),

  speakerModels: () =>
    request<{ models: SpeakerModel[]; directory: string; supported: boolean }>(
      "/v1/speaker-models",
    ),

  downloadSpeakerModel: (name: string) =>
    request<{ model: string; percent: number; status: string }>(
      `/v1/speaker-models/${encodeURIComponent(name)}/download`,
      { method: "POST" },
    ),

  removeSpeakerModel: (name: string) =>
    request<{ removed: string }>(
      `/v1/speaker-models/${encodeURIComponent(name)}`,
      { method: "DELETE" },
    ),

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

  /**
   * Transcribe a file the user picked, by sending its bytes.
   *
   * A browser file picker never reveals where a file came from, so the path endpoint below
   * cannot be driven from this window. Uploading is the price of a working "choose a file"
   * button that needs no native dialog — and over loopback it is a local copy.
   */
  importUpload: (file: File, language?: string) =>
    request<{
      meeting_id: string;
      segments: number;
      speakers: number;
      audio_ms: number;
    }>("/v1/import/upload", {
      method: "POST",
      headers: {
        // `content-type` is deliberately not JSON here; `request` sets it, so it is overridden.
        "content-type": "application/octet-stream",
        "x-notewise-filename": encodeURIComponent(file.name),
        ...(language ? { "x-notewise-language": language } : {}),
      },
      body: file,
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

  /**
   * Forget a decision the model got wrong.
   *
   * The engine has allowed this since decisions became first-class and nothing offered it, which
   * left a wrong decision on the meeting permanently — and a wrong decision is worse than a missing
   * one, because it reads as a record of what the room agreed.
   */
  deleteDecision: (id: string) =>
    request<void>(`/v1/decisions/${id}`, { method: "DELETE" }),

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

  /**
   * Create a note attached to a meeting.
   *
   * The link is a graph edge, not a column: a note outlives the meeting it was taken in and
   * may reference several.
   */
  createMeetingNote: (meetingId: string, input: { title: string; body: string }) =>
    request<Note>("/v1/notes", {
      method: "POST",
      body: JSON.stringify({ ...input, references_meeting: meetingId }),
    }),

  meetingNotes: (meetingId: string) =>
    request<Note[]>(`/v1/meetings/${meetingId}/notes`),

  /**
   * Move a note to the trash. Recoverable with {@link restoreNote}.
   *
   * Returns the note, now carrying `deleted_at`, so a caller can offer an undo without a
   * second round trip.
   */
  deleteNote: (id: string) =>
    request<Note>(`/v1/notes/${id}`, { method: "DELETE" }),

  restoreNote: (id: string) =>
    request<Note>(`/v1/notes/${id}/restore`, { method: "POST" }),

  /** Notes and meetings, kept apart: restoring a meeting brings a transcript back with it. */
  trash: () => request<{ notes: Note[]; meetings: Meeting[] }>("/v1/trash"),

  /**
   * Move a meeting to the trash.
   *
   * Recoverable with {@link restoreMeeting}. Refused while that meeting is being recorded —
   * deleting it out from under the capture pipeline is a crash with extra steps.
   */
  deleteMeeting: (id: string) =>
    request<Meeting>(`/v1/meetings/${id}`, { method: "DELETE" }),

  restoreMeeting: (id: string) =>
    request<Meeting>(`/v1/meetings/${id}/restore`, { method: "POST" }),

  /** Destroy a meeting for good, with its transcript, summaries, decisions and action items. */
  purgeMeeting: (id: string) =>
    request<Meeting>(`/v1/meetings/${id}?purge=true`, { method: "DELETE" }),

  /** Destroy one note for good. There is no undo behind this. */
  purgeNote: (id: string) =>
    request<Note>(`/v1/notes/${id}?purge=true`, { method: "DELETE" }),

  emptyTrash: () =>
    request<{ deleted: number }>("/v1/trash", { method: "DELETE" }),

  /**
   * Ask a question about one note.
   *
   * `scope: "workspace"` also searches the rest of the workspace, so a note can be asked what
   * the meetings said about it. The note itself is always the first citation.
   */
  askNote: (
    id: string,
    messages: Array<{ role: string; content: string }>,
    scope: "note" | "workspace" = "note",
  ) =>
    request<GroundedAnswer>(`/v1/notes/${id}/chat`, {
      method: "POST",
      body: JSON.stringify({ messages, scope }),
    }),

  /** Ask the whole workspace. Answers come only from what retrieval found. */
  ask: (messages: Array<{ role: string; content: string }>) =>
    request<GroundedAnswer>("/v1/ask", {
      method: "POST",
      body: JSON.stringify({ messages }),
    }),

  /** Set the agent going. Returns immediately; poll with {@link agentRun}. */
  startAgentRun: (task: string) =>
    request<AgentRun>("/v1/agent/runs", {
      method: "POST",
      body: JSON.stringify({ task }),
    }),

  agentRun: (id: string) => request<AgentRun>(`/v1/agent/runs/${id}`),

  agentRuns: () => request<AgentRun[]>("/v1/agent/runs"),

  /** What the semantic index holds, and whether it can be built. */
  indexStatus: () => request<IndexStatus>("/v1/index"),

  /** Start an indexing pass. Returns immediately; poll {@link indexStatus}. */
  buildIndex: () => request<IndexStatus>("/v1/index", { method: "POST" }),

  /** Throw the index away. Mainly for vectors left by a model no longer in use. */
  clearIndex: () => request<{ removed: number }>("/v1/index", { method: "DELETE" }),

  /** Everything this build can deliver to, connected or not. */
  availableConnectors: () =>
    request<AvailableConnector[]>("/v1/connectors/available"),

  /**
   * Turn a connector on, or point an existing one somewhere else.
   *
   * `signing_secret` comes back exactly once, at first connect. The engine keeps it in the
   * keychain and cannot show it again.
   */
  connectConnector: (
    id: string,
    target: string,
    options: {
      /** A shared secret the connector needs — the Apps Script deployment key. Goes to the keychain. */
      key?: string;
      /** `calendar`, `mail`, or both. Mail is opt-in. */
      scopes?: string[];
    } = {},
  ) =>
    request<{ id: string; signing_secret: string | null }>(
      `/v1/connectors/${encodeURIComponent(id)}`,
      { method: "POST", body: JSON.stringify({ target, ...options }) },
    ),

  /**
   * Begin a Microsoft sign-in. Returns the page to open; the engine catches the redirect.
   *
   * Notewise ships no app registration of its own, so a client id from the tenant is required —
   * the request says so rather than failing at Microsoft with an error about an unknown app.
   */
  startMicrosoftSignIn: (input: { client_id?: string; scopes?: string[] } = {}) =>
    request<{ authorize_url: string; redirect_uri: string }>(
      "/v1/connectors/microsoft/signin",
      { method: "POST", body: JSON.stringify(input) },
    ),

  microsoftSignInStatus: () =>
    request<{ state: "idle" | "pending" | "connected" | "failed"; error: string | null }>(
      "/v1/connectors/microsoft/signin",
    ),

  /** Pull every connected source now. Notewise also pulls on a timer; this is the impatient path. */
  syncConnectors: () =>
    request<{ pulled: number; upserted: number; failures: string[] }>("/v1/connectors/sync", {
      method: "POST",
    }),

  disconnectConnector: (id: string) =>
    request<void>(`/v1/connectors/${encodeURIComponent(id)}`, { method: "DELETE" }),

  /** Deliveries that failed. Surfaced, because an invisible queue failure loses work silently. */
  connectorFailures: () => request<FailedDelivery[]>("/v1/connectors/failures"),

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

  // -------------------------------------------------------------- external tools

  mcpServers: () => request<McpServerInfo[]>("/v1/mcp/servers"),

  /** Add a server. It arrives disabled, with none of its tools allowed. */
  addMcpServer: (input: {
    name: string;
    transport: "stdio" | "http";
    command?: string;
    args?: string[];
    url?: string;
    auto_start?: boolean;
    /** Environment for a stdio server, headers for an HTTP one. Goes to the keychain. */
    secrets?: Record<string, string>;
  }) =>
    request<McpServerInfo>("/v1/mcp/servers", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  deleteMcpServer: (id: string) =>
    request<{ deleted: boolean }>(`/v1/mcp/servers/${id}`, { method: "DELETE" }),

  setMcpServerEnabled: (id: string, enabled: boolean) =>
    request<McpServerInfo>(`/v1/mcp/servers/${id}/enabled`, {
      method: "PUT",
      body: JSON.stringify({ enabled }),
    }),

  setMcpServerAutoStart: (id: string, auto_start: boolean) =>
    request<McpServerInfo>(`/v1/mcp/servers/${id}/auto-start`, {
      method: "PUT",
      body: JSON.stringify({ auto_start }),
    }),

  /** Ask a server what it can do, starting it if it is allowed to start. */
  mcpServerTools: (id: string) => request<McpDiscovery>(`/v1/mcp/servers/${id}/tools`),

  /** Start a server pinned `auto_start: false`. */
  startMcpServer: (id: string) =>
    request<McpDiscovery>(`/v1/mcp/servers/${id}/start`, { method: "POST" }),

  stopMcpServer: (id: string) =>
    request<{ stopped: boolean }>(`/v1/mcp/servers/${id}/stop`, { method: "POST" }),

  enableMcpTool: (id: string, tool: string) =>
    request<{ enabled_tools: string[] }>(
      `/v1/mcp/servers/${id}/tools/${encodeURIComponent(tool)}`,
      { method: "PUT" },
    ),

  disableMcpTool: (id: string, tool: string) =>
    request<{ enabled_tools: string[] }>(
      `/v1/mcp/servers/${id}/tools/${encodeURIComponent(tool)}`,
      { method: "DELETE" },
    ),

  /** Ask a model for one tool call. Sends nothing — the result needs confirming. */
  proposeToolCall: (input: { action_item_id?: string; text?: string }) =>
    request<ToolProposalResult>("/v1/mcp/proposals", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  toolExecutions: (options: { pending?: boolean; limit?: number } = {}) => {
    const query = new URLSearchParams();
    if (options.pending) query.set("pending", "true");
    if (options.limit) query.set("limit", String(options.limit));
    const suffix = query.toString();
    return request<ToolExecution[]>(`/v1/mcp/executions${suffix ? `?${suffix}` : ""}`);
  },

  /** Approve a call and send it. The only path by which anything reaches an external server. */
  confirmToolCall: (id: string) =>
    request<ToolExecution>(`/v1/mcp/executions/${id}/confirm`, { method: "POST" }),

  rejectToolCall: (id: string) =>
    request<ToolExecution>(`/v1/mcp/executions/${id}/reject`, { method: "POST" }),

  /** Send a call that was confirmed and never went out. Not a retry: a failed call stays failed. */
  executeToolCall: (id: string) =>
    request<ToolExecution>(`/v1/mcp/executions/${id}/execute`, { method: "POST" }),

  // -------------------------------------------------------------- the desktop assistant

  assistant: () => request<AssistantCapabilities>("/v1/assistant"),

  setAssistantHotkey: (
    hotkey: string,
    mode?: "raw" | "cleaned",
    feature: "dictation" | "overlay" = "dictation",
  ) =>
    request<{ feature: string; hotkey: string; mode: string; warning: string | null }>(
      "/v1/assistant/hotkey",
      { method: "PUT", body: JSON.stringify({ hotkey, mode, feature }) },
    ),

  dictationStatus: () => request<DictationStatus>("/v1/dictation"),

  /** Start listening. Nothing is transcribed until it stops. */
  startDictation: (input: { mode?: "raw" | "cleaned" } = {}) =>
    request<DictationStatus>("/v1/dictation", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  /** Stop listening, transcribe, and put the words at the cursor. */
  stopDictation: () => request<Dictated>("/v1/dictation", { method: "DELETE" }),

  cancelDictation: () =>
    request<{ cancelled: boolean }>("/v1/dictation/cancel", { method: "POST" }),

  /** Ask about whatever is on screen. Reads the frontmost app's text as context. */
  askAboutScreen: (question: string, ignoreScreen = false) =>
    request<ScreenAnswer>("/v1/assistant/ask", {
      method: "POST",
      body: JSON.stringify({ question, ignore_screen: ignoreScreen }),
    }),

  assistantActions: () => request<AssistantAction[]>("/v1/assistant/actions"),

  currentSelection: () => request<SelectionInfo>("/v1/assistant/selection"),

  /** Transform a selection. Nothing is written back unless `replace` is set. */
  actOnSelection: (input: { action: AssistantAct; text?: string; replace?: boolean }) =>
    request<ActResult>("/v1/assistant/act", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  /** Ask for a continuation. Suggests only — accepting is a separate step. */
  suggestCompletion: (input: { text?: string; last_asked_ms?: number; force?: boolean }) =>
    request<Completion>("/v1/assistant/complete", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  /**
   * What the assistant would read from your screen right now.
   *
   * Exists so "what does it see" is answerable before the panel is used rather than after. Needs the
   * Accessibility permission, and refuses with the pane to open when it does not have it.
   */
  screenContext: () =>
    request<{ context: ScreenContext; prompt: string; empty: boolean }>(
      "/v1/assistant/context",
    ),

  typingActivity: () =>
    request<{ activity: TypingActivity; supported: boolean }>("/v1/assistant/typing"),

  /** Start watching for typing pauses. Needs Input Monitoring; never implicit. */
  startTypingMonitor: () =>
    request<{ activity: TypingActivity; supported: boolean }>("/v1/assistant/typing", {
      method: "POST",
    }),

  stopTypingMonitor: () =>
    request<{ activity: TypingActivity; supported: boolean }>("/v1/assistant/typing", {
      method: "DELETE",
    }),

  // -------------------------------------------------------------- meetings starting

  /** Meetings that appear to have started. An empty list is the normal state. */
  joinOffers: () => request<JoinOffer[]>("/v1/signals/join/offers"),

  /**
   * Take an offer up. Returns what to call the recording — it does not start one, because
   * starting one already has its own endpoint with its own device and model errors.
   */
  acceptJoinOffer: (id: string) =>
    request<{ title: string; calendar_event_id: string | null }>(
      `/v1/signals/join/offers/${id}/accept`,
      { method: "POST" },
    ),

  dismissJoinOffer: (id: string) =>
    request<{ dismissed: boolean }>(`/v1/signals/join/offers/${id}`, { method: "DELETE" }),

  search: (query: string, limit = 25) =>
    request<SearchHit[]>(
      `/v1/search?q=${encodeURIComponent(query)}&limit=${limit}`,
    ),
};
