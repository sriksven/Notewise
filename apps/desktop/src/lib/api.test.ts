import { afterEach, describe, expect, it, vi } from "vitest";

import { api, ApiError } from "./api";

/**
 * The client's handling of what the engine sends back.
 *
 * Driven through the real `api` object with `fetch` stubbed, so these exercise exactly the code a
 * screen calls rather than a copy of it.
 */

const realFetch = globalThis.fetch;

function respond(response: Response) {
  globalThis.fetch = vi.fn(async () => response) as typeof fetch;
}

afterEach(() => {
  globalThis.fetch = realFetch;
});

describe("request", () => {
  // Two engine routes answer `204 No Content`: removing an API key and disconnecting a connector.
  // `response.json()` on an empty body throws, so both reported failure after succeeding — the
  // key was gone and the screen said "Could not remove the key.", and the refresh that would have
  // shown it gone never ran.
  it("treats 204 No Content as success with no body", async () => {
    respond(new Response(null, { status: 204 }));
    await expect(api.deleteApiKey("anthropic")).resolves.toBeUndefined();

    respond(new Response(null, { status: 204 }));
    await expect(api.disconnectConnector("vault")).resolves.toBeUndefined();
  });

  it("still parses a JSON body", async () => {
    respond(Response.json({ deleted: true }));
    await expect(api.deleteTicket("t1")).resolves.toEqual({ deleted: true });
  });

  it("turns an engine error into an ApiError carrying its code", async () => {
    respond(Response.json({ error: "no ticket t1", code: "not_found" }, { status: 404 }));
    const failure = await api.deleteTicket("t1").catch((e: unknown) => e);
    expect(failure).toBeInstanceOf(ApiError);
    expect(failure).toMatchObject({ message: "no ticket t1", code: "not_found", status: 404 });
  });

  it("names an unreachable engine rather than saying 'Failed to fetch'", async () => {
    globalThis.fetch = vi.fn(async () => {
      throw new TypeError("Failed to fetch");
    }) as typeof fetch;
    await expect(api.health()).rejects.toMatchObject({ code: "engine_unreachable" });
  });
});
