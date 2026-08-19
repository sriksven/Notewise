import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, Route, Trash2, Wand2 } from "lucide-react";

import {
  api,
  ApiError,
  type BackendInfo,
  type RoutingExplain,
  type RoutingRule,
  type RoutingRules,
} from "../lib/api";

/**
 * Which model answers which kind of request.
 *
 * # Why this screen exists
 *
 * The engine has routed per request for a while and nothing could write a rule, so the feature was
 * real and unreachable. Editing SQLite by hand is not a user interface.
 *
 * # Why it leads with the dry run
 *
 * Routing spends money without asking each time. The one question that decides whether someone
 * trusts it or switches it off is "why did that cost anything", and the engine answers it for a
 * hypothetical request without sending one. That answer is the first thing on the screen, not a
 * detail behind a menu.
 *
 * # Why rules are not edited freely here
 *
 * Adding a condition builder for six predicate types is a large surface for a feature most people
 * will use with one rule. This screen installs the starting policy, shows what is in force, lets a
 * rule be removed, and explains any request — the operations that answer "is it on, what is it
 * doing, and how do I stop it". Composing an arbitrary rule stays an API call until there is
 * evidence anyone wants to.
 */
export function RoutingSettings() {
  const [state, setState] = useState<RoutingRules | null>(null);
  const [backends, setBackends] = useState<BackendInfo[]>([]);
  const [explain, setExplain] = useState<RoutingExplain | null>(null);
  const [task, setTask] = useState("summarize");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const [rules, list] = await Promise.all([api.routingRules(), api.backends()]);
      setState(rules);
      // No filtering here: `/v1/backends` already excludes the mock backend, and re-deciding
      // which backends a user may pick would put that judgement in two places.
      setBackends(list.backends);
    } catch {
      // A policy that will not load is not worth a banner over the whole settings screen.
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const ask = useCallback(
    async (which: string) => {
      try {
        setExplain(await api.explainRouting({ task: which }));
      } catch (e) {
        setError(e instanceof ApiError ? e.message : "could not explain that request");
      }
    },
    [],
  );

  // Re-ask whenever the policy or the chosen task changes: a decision shown next to a policy it no
  // longer reflects is worse than no decision.
  useEffect(() => {
    if (state) void ask(task);
  }, [state, task, ask]);

  async function install(backend: string) {
    setBusy(true);
    setError(null);
    try {
      setState(await api.installDefaultRouting(backend));
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "could not install the policy");
    } finally {
      setBusy(false);
    }
  }

  async function remove(name: string) {
    if (!state) return;
    setBusy(true);
    setError(null);
    try {
      const kept = state.rules.filter((r) => r.name !== name);
      setState(await api.saveRoutingRules(kept));
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "could not save the policy");
    } finally {
      setBusy(false);
    }
  }

  const rules = state?.rules ?? [];
  const active = state?.active ?? [];

  return (
    <section className="mt-10">
      <h2 className="mb-1 flex items-center gap-2 text-[13px] font-semibold text-ink">
        <Route className="h-3.5 w-3.5" /> Model routing
      </h2>
      <p className="mb-4 max-w-2xl text-[12.5px] leading-relaxed text-ink-muted">
        Send heavy work to a better model and keep everything else local. With no rules, every
        request goes to the backend above — which is exactly what happens today.
      </p>

      {error && (
        <div className="mb-3 flex items-start gap-2 rounded-lg border border-amber-500/30 bg-amber-500/10 p-3 text-[12.5px] text-amber-200">
          <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          <span>{error}</span>
        </div>
      )}

      {rules.length === 0 ? (
        <div className="rounded-lg border border-hairline p-4">
          <p className="text-[12.5px] text-ink-muted">
            No rules. Pick where summaries should go and everything else stays where it is.
          </p>
          <div className="mt-3 flex flex-wrap gap-2">
            {backends.map((b) => (
              <button
                key={b.kind}
                type="button"
                disabled={busy}
                onClick={() => void install(b.kind)}
                className="btn-ghost px-3 py-1.5 text-[12px] disabled:opacity-50"
              >
                <Wand2 className="mr-1.5 inline h-3 w-3" />
                Summaries to {b.label}
              </button>
            ))}
          </div>
        </div>
      ) : (
        <ul className="space-y-2">
          {rules.map((rule) => (
            <li
              key={rule.name}
              className="flex items-start justify-between gap-3 rounded-lg border border-hairline p-3"
            >
              <div>
                <p className="text-[13px] font-medium text-ink">
                  {rule.name}
                  {!active.includes(rule.name) && (
                    // Stored but not built. Almost always a missing API key, and a screen that
                    // showed it as working would be lying about what will happen.
                    <span className="ml-2 rounded bg-amber-500/15 px-1.5 py-0.5 text-[10.5px] font-semibold text-amber-200">
                      not in force
                    </span>
                  )}
                </p>
                <p className="mt-0.5 text-[12px] text-ink-muted">
                  {describe(rule)} → {rule.model ? `${rule.backend} (${rule.model})` : rule.backend}
                </p>
              </div>
              <button
                type="button"
                disabled={busy}
                onClick={() => void remove(rule.name)}
                aria-label={`Remove ${rule.name}`}
                className="btn-ghost px-2 py-1 text-[12px] disabled:opacity-50"
              >
                <Trash2 className="h-3.5 w-3.5" />
              </button>
            </li>
          ))}
        </ul>
      )}

      <div className="mt-4 rounded-lg border border-hairline p-3">
        <p className="text-[12px] font-medium text-ink">Where would a request go?</p>
        <div className="mt-2 flex flex-wrap items-center gap-2">
          {["summarize", "chat", "extract_decisions", "extract_action_items"].map((t) => (
            <button
              key={t}
              type="button"
              onClick={() => setTask(t)}
              className={`rounded-md px-2 py-1 text-[11.5px] ${
                task === t ? "bg-accent/20 text-accent-soft" : "text-ink-muted hover:text-ink"
              }`}
            >
              {t.replace(/_/g, " ")}
            </button>
          ))}
        </div>
        <p className="mt-2 font-mono text-[12px] text-ink">
          {explain ? explain.decision : "…"}
        </p>
      </div>
    </section>
  );
}

/**
 * A rule's conditions in words.
 *
 * Deliberately partial: an unrecognised predicate reports itself by key rather than being dropped,
 * so a rule written through the API is never displayed as matching less than it does.
 */
function describe(rule: RoutingRule): string {
  if (rule.when.length === 0) return "every request";

  return rule.when
    .map((p) => {
      if ("task" in p) return p.task.join(" or ");
      if ("input_tokens_over" in p) return `over ~${p.input_tokens_over} tokens`;
      if ("input_tokens_under" in p) return `under ~${p.input_tokens_under} tokens`;
      if ("text_contains" in p) return `mentions ${p.text_contains.join(" or ")}`;
      if ("hour_between" in p) return `between ${p.hour_between[0]}:00 and ${p.hour_between[1]}:00`;
      if ("local_backend_healthy" in p)
        return p.local_backend_healthy ? "local model up" : "local model down";
      return Object.keys(p as object).join(", ");
    })
    .join(" and ");
}
