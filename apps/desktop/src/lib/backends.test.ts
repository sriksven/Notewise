import { describe, expect, it } from "vitest";

import { backendBlocker } from "./backends";

const backend = (overrides: Partial<Parameters<typeof backendBlocker>[0]> = {}) => ({
  requires_api_key: false,
  has_key: true,
  requires_endpoint: false,
  ...overrides,
});

describe("backendBlocker", () => {
  it("lets a local backend through", () => {
    expect(backendBlocker(backend())).toBeNull();
  });

  // The bug: the top bar read `requires_api_key` alone, so a provider whose key had been saved
  // in Settings stayed greyed out there with "needs an API key".
  it("lets a hosted provider through once its key is saved", () => {
    expect(backendBlocker(backend({ requires_api_key: true, has_key: true }))).toBeNull();
  });

  it("blocks a hosted provider with no key", () => {
    expect(backendBlocker(backend({ requires_api_key: true, has_key: false }))).toBe("needs_key");
  });

  it("blocks a backend that needs an endpoint, whatever its key", () => {
    expect(backendBlocker(backend({ requires_endpoint: true }))).toBe("needs_endpoint");
  });
});
