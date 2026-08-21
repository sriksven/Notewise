import { describe, expect, it } from "vitest";

import { parseSecrets } from "./secrets";

describe("parseSecrets", () => {
  it("reads environment variables", () => {
    expect(parseSecrets("LINEAR_API_KEY=lin_abc")).toEqual({
      LINEAR_API_KEY: "lin_abc",
    });
  });

  it("reads headers", () => {
    expect(parseSecrets("Authorization: Bearer abc123")).toEqual({
      Authorization: "Bearer abc123",
    });
  });

  /** Otherwise a bearer token containing an equals sign would be cut in half. */
  it("keeps a whole value when it contains the other separator", () => {
    expect(parseSecrets("Authorization: Bearer a=b")).toEqual({
      Authorization: "Bearer a=b",
    });
  });

  /** And a URL in an environment variable would be split at the scheme's colon. */
  it("does not split a url at its scheme", () => {
    expect(parseSecrets("ENDPOINT=https://example.com/mcp")).toEqual({
      ENDPOINT: "https://example.com/mcp",
    });
  });

  it("reads several lines and ignores blanks and comments", () => {
    const parsed = parseSecrets(`
      # the token
      TOKEN=abc

      WORKSPACE=notewise
    `);
    expect(parsed).toEqual({ TOKEN: "abc", WORKSPACE: "notewise" });
  });

  /** A credential stored under an empty key is one nobody can find again. */
  it("skips a line with no key", () => {
    expect(parseSecrets("=orphan\n: also orphan")).toEqual({});
  });

  it("skips a line with no separator", () => {
    expect(parseSecrets("just some words")).toEqual({});
  });

  it("is empty for empty input", () => {
    expect(parseSecrets("")).toEqual({});
    expect(parseSecrets("   \n  ")).toEqual({});
  });

  /** A later line wins, which is what editing the box in place looks like. */
  it("takes the last value for a repeated key", () => {
    expect(parseSecrets("TOKEN=old\nTOKEN=new")).toEqual({ TOKEN: "new" });
  });
});
