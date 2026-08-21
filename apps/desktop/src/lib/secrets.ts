/**
 * Reading credentials out of a text box.
 *
 * A stdio MCP server takes environment variables and an HTTP one takes headers, and those are
 * written differently everywhere else in the world — `KEY=value` in a shell, `Name: value` on the
 * wire. Making someone translate one into the other is a way to get it wrong silently, so both are
 * accepted and the shape is inferred per line.
 *
 * Kept here rather than in the settings screen so the parsing is testable without a browser.
 */

/**
 * Parse `KEY=value` or `Name: value` lines into an object.
 *
 * Blank lines and `#` comments are skipped. A line with no separator, or one starting with the
 * separator, is skipped rather than producing an empty key — a credential stored under `""` is a
 * credential nobody can find again.
 */
export function parseSecrets(text: string): Record<string, string> {
  const out: Record<string, string> = {};

  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;

    // The earlier separator wins, so `Authorization: Bearer a=b` keeps its whole value and
    // `TOKEN=https://x` is not split at the colon in the URL.
    const equals = trimmed.indexOf("=");
    const colon = trimmed.indexOf(":");
    const at = equals === -1 ? colon : colon === -1 ? equals : Math.min(equals, colon);
    if (at <= 0) continue;

    const key = trimmed.slice(0, at).trim();
    const value = trimmed.slice(at + 1).trim();
    if (key) out[key] = value;
  }

  return out;
}
