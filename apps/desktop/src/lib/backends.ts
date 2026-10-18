import type { BackendInfo } from "./api";

/**
 * What stops a backend being switched to right now, or `null` when nothing does.
 *
 * One answer for every place that offers a backend. `requires_api_key` is a fact about the
 * *provider* — Anthropic always needs a key — while `has_key` is whether one is available now,
 * from the keychain or the environment. The top bar used to gate on the first, so a provider
 * stayed greyed out with "needs an API key" after the key had been saved in Settings, which
 * gated on the second. The same menu item meant two different things on two screens.
 *
 * An endpoint is still a blocker whenever one is required: nothing in this window can supply
 * one, so `requires_endpoint` is the whole answer.
 */
export function backendBlocker(
  backend: Pick<BackendInfo, "requires_api_key" | "has_key" | "requires_endpoint">,
): "needs_key" | "needs_endpoint" | null {
  if (backend.requires_api_key && !backend.has_key) return "needs_key";
  if (backend.requires_endpoint) return "needs_endpoint";
  return null;
}
