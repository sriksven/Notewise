/**
 * Client for the local Notewise engine.
 *
 * Talks to the loopback REST API served by `notewise serve`. Requests go through
 * Vite's proxy in development, so everything is same-origin and there is no CORS
 * handling to get wrong.
 */

export interface Health {
  status: string;
  schema_version: number;
  /** Whether the AI backend keeps data on this machine. Shown in the UI. */
  ai_local: boolean;
  ai_model: string;
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

export interface SearchHit {
  kind: string;
  id: string;
  title: string;
  snippet: string;
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

  search: (query: string, limit = 25) =>
    request<SearchHit[]>(
      `/v1/search?q=${encodeURIComponent(query)}&limit=${limit}`,
    ),
};
