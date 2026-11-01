import { renderToStaticMarkup } from "react-dom/server";
import { beforeAll, describe, expect, it, vi } from "vitest";

import type { GroundedAnswer } from "../lib/api";
import type { AskThread as Thread } from "../lib/ask";
import { AskThread } from "./AskThread";

function shown(answer: GroundedAnswer): string {
  const thread: Thread = {
    turns: [{ question: "What am I supposed to be doing this week?", answer }],
    draft: "",
    setDraft: () => {},
    busy: false,
    error: null,
    send: async () => {},
    reset: () => {},
  };
  return renderToStaticMarkup(
    <AskThread
      thread={thread}
      tone="workspace"
      intro="intro"
      placeholder="Ask"
      onNavigate={() => {}}
    />,
  );
}

// Rendered to markup, where layout effects cannot run, and React says so on every render. Only
// that message is silenced; any other error still prints.
beforeAll(() => {
  const original = console.error;
  vi.spyOn(console, "error").mockImplementation((...args: unknown[]) => {
    if (!String(args[0]).includes("useLayoutEffect does nothing on the server")) original(...args);
  });
});

function count(haystack: string, needle: string): number {
  return haystack.split(needle).length - 1;
}

describe("an answer in the thread", () => {
  /**
   * The engine refuses a question nothing matches without asking a model, and says so itself. The
   * thread used to put its own "Nothing in your workspace matched this" above that reply, so the
   * same thing was said twice, under a model name that had not run.
   */
  it("shows a refusal once, in the engine's words, with no model under it", () => {
    const html = shown({
      text: "Nothing in this workspace matches that question.",
      model: "llama3.1:8b",
      citations: [],
      grounded: false,
      considered: 0,
    });

    expect(count(html, "Nothing in this workspace matches that question.")).toBe(1);
    expect(html).not.toMatch(/Nothing in your workspace matched/);
    expect(html).not.toContain("llama3.1:8b");
  });

  it("shows a real answer with its sources and the model that wrote it", () => {
    const html = shown({
      text: "Send the pricing deck by Wednesday [1].",
      model: "llama3.1:8b",
      citations: [{ n: 1, kind: "app", page: "tasks", title: "Open tasks" }],
      grounded: true,
    });

    expect(html).toContain("Send the pricing deck by Wednesday [1].");
    expect(html).toContain("Open tasks");
    expect(html).toContain("llama3.1:8b");
  });
});
