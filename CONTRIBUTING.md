# Contributing

## Before you start

Check which license covers the directory you are touching: `core/` and `apps/` are MIT,
`cloud/` is BSL 1.1. Contributions are accepted under the license of the directory they
land in.

Many directories are scaffolds awaiting their phase. Each scaffold's `README.md` states its
status at the top — read it before assuming something is implemented, and check
[ROADMAP.md](ROADMAP.md) before building ahead of the current phase.

## Setup

```sh
# Rust 1.82+
cargo build --workspace
cargo test --workspace
```

## Before opening a PR

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs all three. Failing any of them blocks the merge.

## Architectural rules

These are enforced in review because they are load-bearing:

1. **Nothing calls a model provider directly.** All model access goes through the
   `ai-router` trait. This is what makes local-or-cloud real.
2. **Dependencies point downward.** `graph` → `storage`, never the reverse. Surfaces never
   depend on each other.
3. **SQL lives only in `storage`.** Add a repository method instead of opening a connection.
4. **Ownership is foreign keys; association is graph edges.**
5. **No `unwrap()` in library code.** Use the crate's `thiserror` enum.

If a change requires breaking one of these, say so explicitly in the PR description and
explain why — sometimes the rule is wrong, but it should be a decision rather than a drift.

## Tests

New behavior needs a test. `storage` and `graph` use in-memory SQLite, so tests are fast and
need no fixtures. `ai-router` tests use `MockBackend`.

If something genuinely cannot be tested in CI — it needs a GPU, a signed bundle, a TCC
permission grant, or a paid API key — mark it `#[ignore]` **with a reason string**. A green
CI run must never imply that unverified things work.

## Commits

Conventional commits: `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`. This drives
changelog generation.

Trunk-based development — short-lived branches off `main`, merged by PR. Keep PRs focused;
a reviewer should be able to hold the whole change in their head.

## Reporting bugs

Include your OS and version, the Notewise version, and what you expected versus what
happened. For audio capture issues, note whether you are capturing mic, system audio, or
both — they use entirely different code paths per platform.

**Never paste real meeting content into an issue.** Redact it or synthesize a minimal
reproduction.
