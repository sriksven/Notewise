/**
 * A read-only client for the Notewise engine.
 *
 * # Read-only is enforced here, not assumed
 *
 * Every function in this module issues a `GET`. There is no `POST`, `PUT`, `PATCH` or
 * `DELETE` — not disabled, not gated behind a flag: absent. A dashboard is for looking at a
 * workspace, and the way a read-only surface stays read-only is by not containing the code
 * that would write.
 *
 * # Where it points
 *
 * Same-origin, through Vite's proxy in development and through whatever serves the built
 * files otherwise. `API_BASE` exists so this can later be pointed at a hosted API without
 * every call site changing — see the README for why that day is not today.
 */

const API_BASE = "";

export interface Health {
  status: string;
  version: string;
  schema_version: number;
  ai_local: boolean;
  ai_model: string;
  can_record: boolean;
  recording_meeting_id: string | null;
}

export interface Meeting {
  id: string;
  project_id: string | null;
  title: string;
  source: string;
  started_at: string;
  ended_at: string | null;
}

export interface Note {
  id: string;
  title: string;
  body: string;
  created_at: string;
  updated_at: string;
  deleted_at: string | null;
}

export interface ActionItem {
  id: string;
  text: string;
  owner: string | null;
  due_at: string | null;
  status?: string;
  meeting_id?: string;
}

export interface Decision {
  id: string;
  text: string;
  reasoning: string | null;
}

export interface Ticket {
  id: string;
  title: string;
  description: string | null;
  status: string;
  owner: string | null;
  due_at: string | null;
}

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

async function get<T>(path: string): Promise<T> {
  let response: Response;
  try {
    response = await fetch(`${API_BASE}${path}`, {
      headers: { accept: "application/json" },
    });
  } catch {
    throw new ApiError("Cannot reach the Notewise engine. Is it running?", 0);
  }

  if (!response.ok) {
    const body = await response.json().catch(() => null);
    throw new ApiError(body?.error ?? `Request failed (${response.status})`, response.status);
  }

  return response.json() as Promise<T>;
}

export const api = {
  health: () => get<Health>("/health"),
  meetings: (limit = 500) => get<Meeting[]>(`/v1/meetings?limit=${limit}`),
  notes: (limit = 500) => get<Note[]>(`/v1/notes?limit=${limit}`),
  tickets: () => get<Ticket[]>("/v1/tickets"),
  actionItems: (meetingId: string) => get<ActionItem[]>(`/v1/meetings/${meetingId}/action-items`),
  decisions: (meetingId: string) => get<Decision[]>(`/v1/meetings/${meetingId}/decisions`),
};
