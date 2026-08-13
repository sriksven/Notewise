# The AI router

The seam the entire "local or cloud, your choice" claim rests on.

## What it is

One trait — `summarize`, `extract_decisions`, `extract_action_items`, `chat` — with three
interchangeable backends. Every feature calls the trait. Nothing above this crate imports a
provider SDK or names a provider type.

That makes local-or-cloud a property the compiler checks, not a promise in a README.

| Backend | Runs | Needs |
|---|---|---|
| `MockBackend` | In-process | Nothing |
| `OllamaBackend` | The user's machine | A running Ollama daemon |
| `AnthropicBackend` | Anthropic API | The user's own key |

## Why `MockBackend` is not just a test fixture

A boundary is only protected if it is testable. Without a mock, every test touching
summarization needs a GPU or a paid API key — so those tests get skipped, and the seam
quietly erodes until someone "temporarily" calls a provider directly.

It ships in the public API for that reason, and it is genuinely useful for UI development and
demos (`NOTEWISE_BACKEND=mock`).

## The default is local

`RouterConfig::default()` selects Ollama. A user who configures nothing gets local inference.
Sending meeting content to a third party requires an explicit opt-in — in practice, setting
an API key.

`BackendKind::is_local()` answers the privacy question **without constructing a backend**, so
settings UI can show the implication of each option before the user picks one.

## Three API details the Anthropic backend gets right

Each of these would break a naive implementation, and each is covered by a test:

1. **No sampling parameters.** `temperature`, `top_p`, and `top_k` are rejected with a 400 on
   current models. The request body omits them; behaviour is steered through the prompt.
2. **A refusal is an HTTP 200.** Safety classifiers can decline and return
   `stop_reason: "refusal"` with an **empty** `content` array. Reading `content[0]` without
   checking `stop_reason` first panics on exactly the responses you least want to crash on.
3. **`content` is a heterogeneous block list.** Thinking blocks can precede text blocks, so
   text is located by `type`, never by position.

Structured extraction uses schema-constrained output rather than prefilling an assistant
turn — prefill now returns a 400.

## Errors are provider-neutral

A caller handling `RateLimited` should not need to know which provider produced it.
`AiError::is_retryable()` encodes the retry policy in one place — and a refusal is
deliberately **not** retryable, since the same input produces the same refusal and retrying
only burns quota.

## Adding a backend

Implement `AiBackend`, add a `BackendKind` variant, extend `Router::from_config`. Nothing
outside `ai-router` changes — that is the property the crate exists to provide.
