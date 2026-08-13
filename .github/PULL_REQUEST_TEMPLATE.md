## What this changes

<!-- One or two sentences. What is different after this merges? -->

## Why

<!-- What problem does it solve? Link an issue if there is one. -->

## Checks

- [ ] `cargo fmt --all`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`

## Architectural rules

These are load-bearing. Tick each, or explain below why the change needs to break one —
sometimes a rule is wrong, but it should be a decision rather than a drift.

- [ ] No model provider is called outside `core/crates/ai-router`
- [ ] Dependencies still point downward (`graph` → `storage`, never the reverse; surfaces
      never depend on each other)
- [ ] SQL stays inside `core/crates/storage`
- [ ] Ownership is foreign keys, association is graph edges
- [ ] No `unwrap()` in library code
- [ ] Any test that cannot run in CI is `#[ignore]`d **with a reason string**

## Licensing

- [ ] No code moved between `core/`/`apps/` (MIT) and `cloud/` (BSL 1.1)

<!--
If this touches audio capture, say which path: microphone, system audio, or both. They are
entirely separate code per platform, and "audio capture" alone is not enough to review.
-->
