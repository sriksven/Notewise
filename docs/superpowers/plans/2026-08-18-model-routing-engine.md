# Model Routing Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `Router` choose a backend per request from ordered rules, so cheap work goes to a local model and expensive work goes to a good one.

**Architecture:** Routing lives *inside* the existing `Router` rather than in a wrapper type, because `AppState` holds `Arc<Router>` — the concrete type — and a wrapper would break `Router`-only methods. `Router` gains a `routes: Vec<Route>` field; an empty vector is exactly today's behaviour, so no existing call site or test changes. Selection is a pure function over `RequestFacts` (task kind, estimated size, hour) with no model call and no network.

**Tech Stack:** Rust, `async-trait`, `serde` for rule serialization, `MockBackend` for tests. No new dependencies.

**Spec:** [2026-08-18-model-routing-rules-design.md](../specs/2026-08-18-model-routing-rules-design.md)

**Scope of this plan:** the `ai-router` engine only. Loading rules from settings, the explain endpoint, and the `LocalBackendHealthy` predicate (needs a probe cache) and `TranscriptMinutesOver` (needs meeting duration, which `TranscriptInput` does not carry) are a **second plan** — this one ends with a working, tested routing engine that ships with an empty policy.

---

## File Structure

| File | Responsibility |
|---|---|
| `core/crates/ai-router/src/policy.rs` (create) | `TaskKind`, `RequestFacts`, `Predicate`, `Route`, `RouteSpec`, selection. All pure. |
| `core/crates/ai-router/src/router.rs` (modify) | `routes` field, selection wiring into the five trait methods, conservative `is_local` / `effective_redaction`. |
| `core/crates/ai-router/src/lib.rs` (modify) | `mod policy;` and re-exports. |

Tests live beside the code in `#[cfg(test)] mod tests`, matching every other module in this crate. No separate test directory.

---

### Task 1: `TaskKind`

**Files:**
- Create: `core/crates/ai-router/src/policy.rs`
- Modify: `core/crates/ai-router/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `core/crates/ai-router/src/policy.rs` with only this:

```rust
//! Per-request backend selection.
//!
//! Everything here is pure. Selection has to be cheap enough to run before every model call, so
//! it looks only at facts already in hand — which trait method was invoked, how big the input is,
//! what time it is — and never at a network or a model.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_kinds_round_trip_through_their_wire_names() {
        for kind in TaskKind::ALL {
            assert_eq!(TaskKind::parse(kind.as_str()), Some(*kind), "{kind:?}");
        }
    }

    #[test]
    fn an_unknown_task_name_is_none_not_a_default() {
        // A rule stored against a task this build does not know must not silently match
        // something else.
        assert_eq!(TaskKind::parse("transcribe"), None);
    }
}
```

Add to `core/crates/ai-router/src/lib.rs` after `mod embed;`:

```rust
mod policy;
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p notewise-ai-router policy::`
Expected: FAIL to compile — `cannot find type TaskKind in this scope`.

- [ ] **Step 3: Write the minimal implementation**

Insert above the `#[cfg(test)]` block in `policy.rs`:

```rust
use serde::{Deserialize, Serialize};

/// Which kind of work a request is.
///
/// Derived from the `AiBackend` method being called, which makes it free and exact — the trait
/// already separates a two-word title from a ninety-minute summary, and that separation is most
/// of what routing exists to exploit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Summarize,
    ExtractDecisions,
    ExtractActionItems,
    Chat,
}

impl TaskKind {
    pub const ALL: &'static [TaskKind] = &[
        TaskKind::Summarize,
        TaskKind::ExtractDecisions,
        TaskKind::ExtractActionItems,
        TaskKind::Chat,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            TaskKind::Summarize => "summarize",
            TaskKind::ExtractDecisions => "extract_decisions",
            TaskKind::ExtractActionItems => "extract_action_items",
            TaskKind::Chat => "chat",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == s)
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p notewise-ai-router policy::`
Expected: PASS, 2 tests.

- [ ] **Step 5: Commit**

```bash
git add core/crates/ai-router/src/policy.rs core/crates/ai-router/src/lib.rs
git commit -m "feat(ai-router): name the kinds of work a request can be"
```

---

### Task 2: `RequestFacts` and token estimation

**Files:**
- Modify: `core/crates/ai-router/src/policy.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`:

```rust
    #[test]
    fn tokens_are_estimated_from_length_not_tokenized() {
        // Four characters per token is wrong in the third significant figure and right about
        // the only thing a predicate asks: is this a title or a transcript.
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens(&"x".repeat(4000)), 1000);
    }

    #[test]
    fn facts_for_a_transcript_count_title_text_and_context() {
        let facts = RequestFacts::for_transcript(
            TaskKind::Summarize,
            "Standup",
            &"x".repeat(400),
            Some("Platform"),
            9,
        );

        assert_eq!(facts.task, TaskKind::Summarize);
        assert!(facts.estimated_tokens >= 100, "{}", facts.estimated_tokens);
        assert_eq!(facts.hour_of_day, 9);
        assert!(facts.text.contains("Standup"));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p notewise-ai-router policy::`
Expected: FAIL to compile — `cannot find function estimate_tokens`.

- [ ] **Step 3: Write the minimal implementation**

Add to `policy.rs` above the test module:

```rust
/// Roughly how many tokens a string is, without a tokenizer.
///
/// Four bytes per token. An exact count needs a tokenizer per model family, and the predicates
/// this feeds exist to tell a title from a transcript — a decision no plausible tokenizer
/// disagreement changes.
pub fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// Everything selection is allowed to look at.
///
/// Built once per request by the caller that knows the shape of the input, so a `Predicate` never
/// has to care whether it is judging a transcript or a chat history.
#[derive(Debug, Clone)]
pub struct RequestFacts {
    pub task: TaskKind,
    pub estimated_tokens: usize,
    /// Local hour, 0..=23. Injected rather than read from the clock so selection stays pure.
    pub hour_of_day: u8,
    /// The text a keyword predicate searches: title and context for a transcript, the last user
    /// message for a chat. Lowercased once here so every predicate does not repeat it.
    pub text: String,
}

impl RequestFacts {
    pub fn for_transcript(
        task: TaskKind,
        title: &str,
        body: &str,
        context: Option<&str>,
        hour_of_day: u8,
    ) -> Self {
        let extra = context.unwrap_or_default();
        Self {
            task,
            estimated_tokens: estimate_tokens(title)
                + estimate_tokens(body)
                + estimate_tokens(extra),
            hour_of_day,
            text: format!("{title} {extra}").to_lowercase(),
        }
    }

    pub fn for_chat(context: &[String], last_user_message: &str, hour_of_day: u8) -> Self {
        let context_tokens: usize = context.iter().map(|c| estimate_tokens(c)).sum();
        Self {
            task: TaskKind::Chat,
            estimated_tokens: context_tokens + estimate_tokens(last_user_message),
            hour_of_day,
            text: last_user_message.to_lowercase(),
        }
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p notewise-ai-router policy::`
Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add core/crates/ai-router/src/policy.rs
git commit -m "feat(ai-router): describe a request in the terms routing can judge"
```

---

### Task 3: `Predicate` and its evaluation

**Files:**
- Modify: `core/crates/ai-router/src/policy.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`:

```rust
    fn facts(task: TaskKind, tokens: usize, hour: u8, text: &str) -> RequestFacts {
        RequestFacts {
            task,
            estimated_tokens: tokens,
            hour_of_day: hour,
            text: text.to_lowercase(),
        }
    }

    #[test]
    fn task_predicate_matches_any_listed_kind() {
        let p = Predicate::Task(vec![TaskKind::Summarize, TaskKind::Chat]);
        assert!(p.holds(&facts(TaskKind::Summarize, 0, 9, "")));
        assert!(p.holds(&facts(TaskKind::Chat, 0, 9, "")));
        assert!(!p.holds(&facts(TaskKind::ExtractDecisions, 0, 9, "")));
    }

    #[test]
    fn token_thresholds_are_exclusive_at_the_boundary() {
        let over = Predicate::InputTokensOver(100);
        assert!(!over.holds(&facts(TaskKind::Chat, 100, 9, "")));
        assert!(over.holds(&facts(TaskKind::Chat, 101, 9, "")));

        let under = Predicate::InputTokensUnder(100);
        assert!(!under.holds(&facts(TaskKind::Chat, 100, 9, "")));
        assert!(under.holds(&facts(TaskKind::Chat, 99, 9, "")));
    }

    #[test]
    fn keyword_matching_ignores_case() {
        let p = Predicate::TextContains(vec!["Board Review".into()]);
        assert!(p.holds(&facts(TaskKind::Summarize, 0, 9, "Q3 board review")));
        assert!(!p.holds(&facts(TaskKind::Summarize, 0, 9, "standup")));
    }

    #[test]
    fn an_hour_range_can_wrap_past_midnight() {
        // 22..=3 is a real thing a user will write, and a naive a <= h <= b never matches it.
        let overnight = Predicate::HourBetween(22, 3);
        assert!(overnight.holds(&facts(TaskKind::Chat, 0, 23, "")));
        assert!(overnight.holds(&facts(TaskKind::Chat, 0, 2, "")));
        assert!(!overnight.holds(&facts(TaskKind::Chat, 0, 12, "")));

        let daytime = Predicate::HourBetween(9, 17);
        assert!(daytime.holds(&facts(TaskKind::Chat, 0, 9, "")));
        assert!(daytime.holds(&facts(TaskKind::Chat, 0, 17, "")));
        assert!(!daytime.holds(&facts(TaskKind::Chat, 0, 18, "")));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p notewise-ai-router policy::`
Expected: FAIL to compile — `cannot find type Predicate`.

- [ ] **Step 3: Write the minimal implementation**

Add to `policy.rs`:

```rust
/// One condition on a request.
///
/// Every variant is answerable from [`RequestFacts`] alone. There is deliberately no
/// model-classified variant: it would mean a model call to decide which model to call, doubling
/// latency on the fast path to separate work that [`TaskKind`] already separates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Predicate {
    Task(Vec<TaskKind>),
    InputTokensOver(usize),
    InputTokensUnder(usize),
    /// Case-insensitive substring match against the request's title, context, or question.
    TextContains(Vec<String>),
    /// Inclusive local-hour range. `from > to` wraps past midnight.
    HourBetween(u8, u8),
}

impl Predicate {
    pub fn holds(&self, facts: &RequestFacts) -> bool {
        match self {
            Predicate::Task(kinds) => kinds.contains(&facts.task),
            Predicate::InputTokensOver(n) => facts.estimated_tokens > *n,
            Predicate::InputTokensUnder(n) => facts.estimated_tokens < *n,
            Predicate::TextContains(needles) => needles
                .iter()
                .any(|n| facts.text.contains(&n.to_lowercase())),
            Predicate::HourBetween(from, to) => {
                let h = facts.hour_of_day;
                if from <= to {
                    h >= *from && h <= *to
                } else {
                    // Wrapped: 22..=3 means 22, 23, 0, 1, 2, 3.
                    h >= *from || h <= *to
                }
            }
        }
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p notewise-ai-router policy::`
Expected: PASS, 8 tests.

- [ ] **Step 5: Commit**

```bash
git add core/crates/ai-router/src/policy.rs
git commit -m "feat(ai-router): predicates a request can be judged by, without a model"
```

---

### Task 4: `RouteSpec` and first-match-wins selection

**Files:**
- Modify: `core/crates/ai-router/src/policy.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`:

```rust
    fn spec(name: &str, when: Vec<Predicate>) -> RouteSpec {
        RouteSpec {
            name: name.into(),
            when,
        }
    }

    #[test]
    fn the_first_matching_route_wins_not_the_best() {
        // Order is the semantics. A user reading their rules top to bottom must be able to
        // predict the outcome, which a scoring system makes unanswerable.
        let routes = vec![
            spec("anything", vec![]),
            spec("summaries", vec![Predicate::Task(vec![TaskKind::Summarize])]),
        ];

        assert_eq!(
            select_index(&routes, &facts(TaskKind::Summarize, 0, 9, "")),
            Some(0)
        );
    }

    #[test]
    fn every_predicate_in_a_route_must_hold() {
        let routes = vec![spec(
            "big summaries",
            vec![
                Predicate::Task(vec![TaskKind::Summarize]),
                Predicate::InputTokensOver(1000),
            ],
        )];

        assert_eq!(
            select_index(&routes, &facts(TaskKind::Summarize, 5000, 9, "")),
            Some(0)
        );
        // Right task, too small.
        assert_eq!(
            select_index(&routes, &facts(TaskKind::Summarize, 10, 9, "")),
            None
        );
        // Big enough, wrong task.
        assert_eq!(
            select_index(&routes, &facts(TaskKind::Chat, 5000, 9, "")),
            None
        );
    }

    #[test]
    fn no_routes_means_no_selection() {
        assert_eq!(select_index(&[], &facts(TaskKind::Chat, 0, 9, "")), None);
    }

    #[test]
    fn a_route_with_no_predicates_matches_everything() {
        let routes = vec![spec("catch all", vec![])];
        assert_eq!(
            select_index(&routes, &facts(TaskKind::Chat, 0, 9, "")),
            Some(0)
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p notewise-ai-router policy::`
Expected: FAIL to compile — `cannot find type RouteSpec`.

- [ ] **Step 3: Write the minimal implementation**

Add to `policy.rs`:

```rust
/// A route's conditions, without its backend.
///
/// Separate from the constructed route so selection is testable without building five backends,
/// and so a rule set can be serialized without serializing a credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteSpec {
    pub name: String,
    /// All must hold. An empty list matches every request.
    pub when: Vec<Predicate>,
}

impl RouteSpec {
    pub fn matches(&self, facts: &RequestFacts) -> bool {
        self.when.iter().all(|p| p.holds(facts))
    }
}

/// Index of the first route whose conditions all hold.
///
/// First match rather than best match: ordering is the semantics, and "why did this go to the
/// expensive model" has to be answerable by reading the list from the top.
/// Used by rule-set validation and the explain endpoint, which hold an owned slice. `Router`
/// deliberately does not call this — see `Router::route_for` for why.
pub fn select_index(routes: &[RouteSpec], facts: &RequestFacts) -> Option<usize> {
    routes.iter().position(|r| r.matches(facts))
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p notewise-ai-router policy::`
Expected: PASS, 12 tests.

- [ ] **Step 5: Commit**

```bash
git add core/crates/ai-router/src/policy.rs
git commit -m "feat(ai-router): first-match-wins route selection"
```

---

### Task 5: Rule sets serialize, and reject their own footguns

**Files:**
- Modify: `core/crates/ai-router/src/policy.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`:

```rust
    #[test]
    fn a_rule_set_round_trips_through_json() {
        let original = vec![
            spec(
                "quality for summaries",
                vec![Predicate::Task(vec![TaskKind::Summarize])],
            ),
            spec("overnight", vec![Predicate::HourBetween(22, 3)]),
        ];

        let json = serde_json::to_string(&original).expect("serializes");
        let back: Vec<RouteSpec> = serde_json::from_str(&json).expect("deserializes");

        assert_eq!(back, original);
    }

    #[test]
    fn an_unreachable_route_is_reported() {
        // A catch-all followed by anything means the tail can never fire. Silently accepting it
        // produces a rule the user believes is active.
        let routes = vec![spec("catch all", vec![]), spec("never", vec![])];
        assert_eq!(
            unreachable_route(&routes),
            Some(1),
            "the route after a catch-all is dead"
        );

        let fine = vec![
            spec("summaries", vec![Predicate::Task(vec![TaskKind::Summarize])]),
            spec("catch all", vec![]),
        ];
        assert_eq!(unreachable_route(&fine), None);
    }

    #[test]
    fn a_contradictory_route_is_reported() {
        // Under 100 and over 1000 cannot both hold.
        let routes = vec![spec(
            "impossible",
            vec![
                Predicate::InputTokensOver(1000),
                Predicate::InputTokensUnder(100),
            ],
        )];
        assert_eq!(contradictory_route(&routes), Some(0));

        let fine = vec![spec(
            "mid",
            vec![
                Predicate::InputTokensOver(100),
                Predicate::InputTokensUnder(1000),
            ],
        )];
        assert_eq!(contradictory_route(&fine), None);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p notewise-ai-router policy::`
Expected: FAIL to compile — `cannot find function unreachable_route`.

- [ ] **Step 3: Write the minimal implementation**

Add to `policy.rs`:

```rust
/// Index of the first route that can never fire because an earlier one matches everything.
///
/// Worth detecting at save time. A rule listed below a catch-all looks active in the UI and never
/// runs, and the symptom — work going to the wrong model — points at the wrong rule.
pub fn unreachable_route(routes: &[RouteSpec]) -> Option<usize> {
    let catch_all = routes.iter().position(|r| r.when.is_empty())?;
    if catch_all + 1 < routes.len() {
        Some(catch_all + 1)
    } else {
        None
    }
}

/// Index of the first route whose token bounds cannot both hold.
///
/// Only the numeric bounds are checked. A general satisfiability check over every predicate pair
/// is more machinery than this earns, and these two are the pair a user actually inverts.
pub fn contradictory_route(routes: &[RouteSpec]) -> Option<usize> {
    routes.iter().position(|route| {
        let mut floor: Option<usize> = None;
        let mut ceiling: Option<usize> = None;
        for p in &route.when {
            match p {
                Predicate::InputTokensOver(n) => floor = Some(floor.map_or(*n, |f: usize| f.max(*n))),
                Predicate::InputTokensUnder(n) => {
                    ceiling = Some(ceiling.map_or(*n, |c: usize| c.min(*n)))
                }
                _ => {}
            }
        }
        matches!((floor, ceiling), (Some(f), Some(c)) if f + 1 >= c)
    })
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p notewise-ai-router policy::`
Expected: PASS, 15 tests.

- [ ] **Step 5: Commit**

```bash
git add core/crates/ai-router/src/policy.rs
git commit -m "feat(ai-router): catch rule sets that cannot do what they say"
```

---

### Task 6: `Router` holds routes, and an empty set changes nothing

**Files:**
- Modify: `core/crates/ai-router/src/router.rs:255-260` (the `Router` struct)
- Modify: `core/crates/ai-router/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add to the existing `mod tests` in `router.rs`:

```rust
    #[test]
    fn a_router_has_no_routes_until_given_some() {
        let router = Router::from_config(RouterConfig::mock()).expect("mock router");
        assert!(
            router.route_names().is_empty(),
            "an empty policy is today's behaviour and must be the default"
        );
    }

    #[test]
    fn routes_are_named_in_the_order_they_were_added() {
        let router = Router::from_config(RouterConfig::mock())
            .expect("mock router")
            .with_route(
                RouteSpec {
                    name: "first".into(),
                    when: vec![],
                },
                Box::new(MockBackend::new()),
                BackendKind::Mock,
                RedactionPolicy::Off,
            );

        assert_eq!(router.route_names(), vec!["first".to_string()]);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p notewise-ai-router router::`
Expected: FAIL to compile — `no method named route_names`.

- [ ] **Step 3: Write the minimal implementation**

In `router.rs`, add to the imports at the top:

```rust
use crate::policy::{RequestFacts, RouteSpec};
```

Replace the `Router` struct (currently at `router.rs:255-260`) with:

```rust
/// One configured route: its conditions, its backend, and that backend's own privacy settings.
#[derive(Debug)]
pub struct Route {
    spec: RouteSpec,
    backend: Box<dyn AiBackend>,
    kind: BackendKind,
    redaction: RedactionPolicy,
}

/// The interface every feature depends on.
#[derive(Debug)]
pub struct Router {
    backend: Box<dyn AiBackend>,
    kind: BackendKind,
    redaction: RedactionPolicy,
    /// Ordered. First match wins. Empty means every request goes to `backend`, which is exactly
    /// the behaviour before routing existed.
    routes: Vec<Route>,
}
```

Then find `Router::from_config` and add `routes: Vec::new(),` to the struct literal it returns, and add these methods to the `impl Router` block that already contains `with_redaction`:

```rust
    /// Add a route, evaluated after every route already added.
    pub fn with_route(
        mut self,
        spec: RouteSpec,
        backend: Box<dyn AiBackend>,
        kind: BackendKind,
        redaction: RedactionPolicy,
    ) -> Self {
        self.routes.push(Route {
            spec,
            backend,
            kind,
            redaction,
        });
        self
    }

    /// Route names, in evaluation order. For the settings UI and the explain endpoint.
    pub fn route_names(&self) -> Vec<String> {
        self.routes.iter().map(|r| r.spec.name.clone()).collect()
    }

    /// The backend a request with these facts would go to, and the route's name if it matched one.
    ///
    /// Borrowed rather than cloned: this runs before every model call.
    fn route_for(&self, facts: &RequestFacts) -> (&dyn AiBackend, Option<&str>) {
        // Iterate rather than reusing `policy::select_index`, which takes a `&[RouteSpec]` and
        // would mean cloning every spec on a path that runs before every model call. The
        // first-match rule is one line either way; the allocation is not.
        match self.routes.iter().find(|r| r.spec.matches(facts)) {
            Some(route) => (route.backend.as_ref(), Some(&route.spec.name)),
            None => (self.backend.as_ref(), None),
        }
    }

    /// Which route a request would take. Answers "why did this cost money".
    pub fn explain(&self, facts: &RequestFacts) -> String {
        match self.route_for(facts).1 {
            Some(name) => format!("route {name:?}"),
            None => "the default backend".to_string(),
        }
    }
```

In `lib.rs`, change the router re-export line to include the new types:

```rust
pub use policy::{
    contradictory_route, estimate_tokens, select_index, unreachable_route, Predicate, RequestFacts,
    RouteSpec, TaskKind,
};
pub use router::{BackendKind, Route, Router, RouterConfig};
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p notewise-ai-router`
Expected: PASS. All 174 pre-existing tests still pass — this is the point of an empty default.

- [ ] **Step 5: Commit**

```bash
git add core/crates/ai-router/src/router.rs core/crates/ai-router/src/lib.rs
git commit -m "feat(ai-router): give Router an ordered route list, empty by default"
```

---

### Task 7: Selection is used by every trait method

**Files:**
- Modify: `core/crates/ai-router/src/router.rs` (the `impl AiBackend for Router` block)

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `router.rs`:

```rust
    /// A backend that records nothing but reports a distinct model id, so a test can prove which
    /// one answered.
    fn named(id: &'static str) -> Box<dyn AiBackend> {
        Box::new(MockBackend::new().with_model_id(id))
    }

    #[tokio::test]
    async fn a_summary_takes_the_summary_route_and_chat_does_not() {
        let router = Router::with_backend(named("default"))
            .with_route(
                RouteSpec {
                    name: "quality".into(),
                    when: vec![Predicate::Task(vec![TaskKind::Summarize])],
                },
                named("quality"),
                BackendKind::Mock,
                RedactionPolicy::Off,
            );

        let summary = router
            .summarize(&TranscriptInput::new("t", "we agreed"))
            .await
            .expect("summarizes");
        assert_eq!(summary.model, "quality");

        let answer = router
            .chat(&ChatRequest::new(vec![ChatMessage::user("hi")]))
            .await
            .expect("chats");
        assert_eq!(
            answer.model, "default",
            "chat does not match the summary route and must fall through"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p notewise-ai-router router::a_summary_takes`
Expected: FAIL — either `no method named with_model_id`, or the assertion fails with `model == "default"` because selection is not wired in yet.

- [ ] **Step 3: Write the minimal implementation**

First give `MockBackend` a settable id. In `core/crates/ai-router/src/backends/mock.rs`, add to its `impl` block:

```rust
    /// Override the reported model id, so a test with several mocks can prove which answered.
    pub fn with_model_id(mut self, id: impl Into<String>) -> Self {
        self.model_id = id.into();
        self
    }
```

If `MockBackend` has no `model_id` field, add `model_id: String` to the struct and initialise it to `"mock".to_string()` in `new()`, and return `&self.model_id` from its `model_id()` method.

Then in `router.rs`, rewrite the five forwarding methods in `impl AiBackend for Router` to select first. `hour_of_day` comes from the local clock here, at the edge, so `RequestFacts` stays pure:

```rust
    async fn summarize(&self, input: &TranscriptInput) -> Result<SummaryOutput> {
        let facts = RequestFacts::for_transcript(
            TaskKind::Summarize,
            &input.title,
            &input.text,
            input.context.as_deref(),
            local_hour(),
        );
        self.route_for(&facts).0.summarize(input).await
    }

    async fn extract_decisions(&self, input: &TranscriptInput) -> Result<Vec<ExtractedDecision>> {
        let facts = RequestFacts::for_transcript(
            TaskKind::ExtractDecisions,
            &input.title,
            &input.text,
            input.context.as_deref(),
            local_hour(),
        );
        self.route_for(&facts).0.extract_decisions(input).await
    }

    async fn extract_action_items(
        &self,
        input: &TranscriptInput,
    ) -> Result<Vec<ExtractedActionItem>> {
        let facts = RequestFacts::for_transcript(
            TaskKind::ExtractActionItems,
            &input.title,
            &input.text,
            input.context.as_deref(),
            local_hour(),
        );
        self.route_for(&facts).0.extract_action_items(input).await
    }

    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse> {
        let last = request
            .messages
            .last()
            .map(|m| m.content.as_str())
            .unwrap_or_default();
        let facts = RequestFacts::for_chat(&request.context, last, local_hour());
        self.route_for(&facts).0.chat(request).await
    }
```

Add near the top of `router.rs`:

```rust
/// The local hour, read at the edge so [`RequestFacts`] stays pure and testable.
fn local_hour() -> u8 {
    use chrono::Timelike;
    chrono::Local::now().hour() as u8
}
```

Add `chrono = { workspace = true }` to `core/crates/ai-router/Cargo.toml` under `[dependencies]` if it is not already there.

**Do not change** `probe`, `installed_models`, `model_id`, or `is_local` in this task — they are Task 8.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p notewise-ai-router`
Expected: PASS, including the new test and all pre-existing ones.

- [ ] **Step 5: Commit**

```bash
git add core/crates/ai-router/src/router.rs core/crates/ai-router/src/backends/mock.rs core/crates/ai-router/Cargo.toml
git commit -m "feat(ai-router): route each call by the kind of work it is"
```

---

### Task 8: `is_local` and `effective_redaction` tell the truth under a policy

**Files:**
- Modify: `core/crates/ai-router/src/router.rs`

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `router.rs`:

```rust
    #[test]
    fn a_policy_with_any_remote_route_is_not_local() {
        // `is_local` drives a claim the product treats as verifiable. A policy where anything may
        // leave the machine is not local, even if most calls stay.
        let router = Router::from_config(RouterConfig::ollama())
            .expect("ollama router")
            .with_route(
                RouteSpec {
                    name: "cloud".into(),
                    when: vec![Predicate::Task(vec![TaskKind::Summarize])],
                },
                Box::new(MockBackend::new()),
                BackendKind::Anthropic,
                RedactionPolicy::Secrets,
            );

        assert!(
            !router.is_local(),
            "a route to Anthropic means this router is not local"
        );
    }

    #[test]
    fn redaction_is_the_strictest_across_every_route() {
        let router = Router::from_config(RouterConfig::anthropic("k"))
            .expect("anthropic router")
            .with_redaction(RedactionPolicy::Secrets)
            .with_route(
                RouteSpec {
                    name: "strict".into(),
                    when: vec![],
                },
                Box::new(MockBackend::new()),
                BackendKind::Anthropic,
                RedactionPolicy::SecretsAndContacts,
            );

        assert_eq!(
            router.effective_redaction(),
            RedactionPolicy::SecretsAndContacts,
            "asked without a call context, it must not under-report masking"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p notewise-ai-router router::a_policy_with_any_remote router::redaction_is_the_strictest`
Expected: FAIL — `is_local` returns true and `effective_redaction` returns `Secrets`, because neither consults the routes yet.

- [ ] **Step 3: Write the minimal implementation**

Add a strictness ordering to `core/crates/ai-router/src/redact.rs`, inside `impl RedactionPolicy`:

```rust
    /// How much this masks, for comparing two policies. Higher masks more.
    fn strictness(self) -> u8 {
        match self {
            RedactionPolicy::Off => 0,
            RedactionPolicy::Secrets => 1,
            RedactionPolicy::SecretsAndContacts => 2,
        }
    }

    /// The policy that masks more of the two.
    pub fn stricter(self, other: Self) -> Self {
        if other.strictness() > self.strictness() {
            other
        } else {
            self
        }
    }
```

In `router.rs`, replace the bodies of `is_local` (the one on `impl Router` at line ~346) and `effective_redaction`:

```rust
    /// Whether **everything** this router might do stays on the machine.
    ///
    /// False if any route is remote. This drives a claim the UI presents as verifiable, so a
    /// policy that might send a summary to Anthropic is not local even if every other call stays.
    pub fn is_local(&self) -> bool {
        self.kind.is_local() && self.routes.iter().all(|r| r.kind.is_local())
    }

    /// The policy actually in force, taking the strictest across the default and every route.
    ///
    /// A local backend is always `Off`: nothing leaves the machine, so masking would only degrade
    /// the input. With routes, the question is asked without knowing which one a future call will
    /// take, so the answer has to be the one that under-reports nothing.
    pub fn effective_redaction(&self) -> RedactionPolicy {
        let effective = |kind: BackendKind, policy: RedactionPolicy| {
            if kind.is_local() {
                RedactionPolicy::Off
            } else {
                policy
            }
        };

        self.routes.iter().fold(
            effective(self.kind, self.redaction),
            |acc, r| acc.stricter(effective(r.kind, r.redaction)),
        )
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p notewise-ai-router`
Expected: PASS. The pre-existing `redaction_defaults_to_masking_secrets`, `redaction_can_be_disabled_explicitly`, and `privacy_is_answerable_without_constructing_a_backend` tests must still pass — with no routes, `fold` returns the unchanged starting value.

- [ ] **Step 5: Commit**

```bash
git add core/crates/ai-router/src/router.rs core/crates/ai-router/src/redact.rs
git commit -m "fix(ai-router): a router with a remote route is not local"
```

---

### Task 9: A retryable failure on a route falls back to the default, once

**Files:**
- Modify: `core/crates/ai-router/src/router.rs`

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `router.rs`:

```rust
    #[tokio::test]
    async fn a_retryable_route_failure_retries_on_the_default() {
        let router = Router::with_backend(named("default")).with_route(
            RouteSpec {
                name: "flaky".into(),
                when: vec![],
            },
            Box::new(MockBackend::new().failing_with(|| AiError::Transport {
                backend: "mock",
                source: transport_error(),
            })),
            BackendKind::Mock,
            RedactionPolicy::Off,
        );

        let summary = router
            .summarize(&TranscriptInput::new("t", "x"))
            .await
            .expect("should fall back to the default");
        assert_eq!(summary.model, "default");
    }

    #[tokio::test]
    async fn a_non_retryable_route_failure_does_not_fall_back() {
        // A refusal is the same on any backend. Retrying elsewhere just spends another call.
        let router = Router::with_backend(named("default")).with_route(
            RouteSpec {
                name: "refuses".into(),
                when: vec![],
            },
            Box::new(MockBackend::new().failing_with(|| AiError::Refused {
                backend: "mock",
                category: None,
            })),
            BackendKind::Mock,
            RedactionPolicy::Off,
        );

        let err = router
            .summarize(&TranscriptInput::new("t", "x"))
            .await
            .expect_err("a refusal must surface");
        assert!(matches!(err, AiError::Refused { .. }), "{err:?}");
    }
```

If `MockBackend` has no `failing_with`, add it in `backends/mock.rs` mirroring `MockConnector`'s failure hook in `notewise-connectors`:

```rust
    /// Make every call fail with this error, for exercising a caller's failure path.
    pub fn failing_with(
        mut self,
        failure: impl Fn() -> AiError + Send + Sync + 'static,
    ) -> Self {
        self.failure = Some(Box::new(failure));
        self
    }
```

with `failure: Option<Box<dyn Fn() -> AiError + Send + Sync>>` on the struct, initialised to `None`, and checked at the top of each trait method. Add a `transport_error()` test helper that produces a real `reqwest::Error` by awaiting a request to `http://127.0.0.1:1/`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p notewise-ai-router router::a_retryable_route router::a_non_retryable_route`
Expected: FAIL — the retryable case surfaces the transport error instead of falling back.

- [ ] **Step 3: Write the minimal implementation**

Add to `impl Router`:

```rust
    /// Run `call` on the selected backend, retrying on the default if a *route* failed
    /// retryably.
    ///
    /// One hop, not a cascade. A chain of failing backends turns one slow call into four, and a
    /// user is better served by an error than by a ninety-second wait. The default itself is
    /// never retried: it is already the fallback.
    async fn with_fallback<'a, T, F, Fut>(&'a self, facts: &RequestFacts, call: F) -> Result<T>
    where
        F: Fn(&'a dyn AiBackend) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let (backend, route) = self.route_for(facts);
        match call(backend).await {
            Ok(value) => Ok(value),
            Err(err) if route.is_some() && err.is_retryable() => {
                tracing::warn!(
                    route = route.unwrap_or_default(),
                    error = %err,
                    "route failed retryably; falling back to the default backend"
                );
                call(self.backend.as_ref()).await
            }
            Err(err) => Err(err),
        }
    }
```

Add `tracing = { workspace = true }` to `core/crates/ai-router/Cargo.toml` if absent, then route the four methods from Task 7 through it, e.g. for `summarize`:

```rust
        self.with_fallback(&facts, |b| b.summarize(input)).await
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p notewise-ai-router`
Expected: PASS, all tests.

- [ ] **Step 5: Commit**

```bash
git add core/crates/ai-router/src/router.rs core/crates/ai-router/src/backends/mock.rs core/crates/ai-router/Cargo.toml
git commit -m "feat(ai-router): fall back to the default once when a route fails retryably"
```

---

### Task 10: Gates, and the whole-crate check

**Files:** none

- [ ] **Step 1: Format**

Run: `cargo fmt --all`

- [ ] **Step 2: Lint at CI strictness**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings. `-D warnings` is what CI uses, so anything less is a red build later.

- [ ] **Step 3: Full workspace suite**

Run: `cargo test --workspace`
Expected: every crate passes. `ai-router` should now be 174 + roughly 20 new tests.

- [ ] **Step 4: Confirm no caller changed**

Run: `git diff --stat main -- core/crates/api-server apps/`
Expected: **empty**. The whole architecture rests on this — if a call site had to change, `Router` was the wrong place for the policy and R4 needs revisiting.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "chore(ai-router): fmt and clippy after the routing engine"
```

---

## What this plan does not do

Deliberately out of scope, for a second plan:

- **Loading rules from settings.** `SettingsRepository` persistence under one JSON key, per the spec's R8, plus route-scoped credentials in `CredentialStore`.
- **The explain endpoint.** `Router::explain` exists after Task 6; nothing exposes it over HTTP yet.
- **The shipped default policy.** Until rules load from settings, every router has an empty policy and behaves exactly as today. That is the safe intermediate state.
- **`LocalBackendHealthy`** — needs a cached probe result, which needs somewhere to cache it.
- **`TranscriptMinutesOver`** — `TranscriptInput` carries `title`, `text` and `context`, and no duration. Adding one is a change to a type every backend consumes.

## Self-review notes

Checked against the spec:

- R1 (task kind as primary signal) — Task 1, used in Task 7.
- R2 (local evaluation, no LLM) — Task 3; no predicate makes a call.
- R3 (routes carry their own redaction) — Task 6's `Route` holds `kind` and `redaction`; Task 8 uses them.
- R4 / R4a (routing inside `Router`, conservative whole-router answers) — Tasks 6 and 8. Task 10 Step 4 verifies the no-caller-change claim.
- R5 (`model_id` honesty) — **not implemented here.** `Router::model_id` still returns the default's id. `SummaryOutput.model` already carries the real per-call model, which is the value that gets stored, so nothing records a wrong attribution. Left for the second plan, where the settings UI needs a policy label.
- R6 (`is_local`) — Task 8.
- R7 (fallback once) — Task 9.
- R8 (rule set as JSON) — serialization in Task 5; persistence is the second plan.

Type consistency: `RouteSpec { name, when }` is used identically in Tasks 4-9. `select_index(&[RouteSpec], &RequestFacts) -> Option<usize>` is defined in Task 4 and consumed in Task 6. `RequestFacts` fields are fixed in Task 2 and unchanged after.
