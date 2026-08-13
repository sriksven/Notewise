# Building on macOS

## Requirements

- Rust 1.82+
- Xcode Command Line Tools (`xcode-select --install`) — needed for `cc`

```sh
cargo build --workspace
cargo test --workspace
```

That is all the engine needs. SQLite is bundled.

## The audio-capture constraint

Microphone capture needs a `NSMicrophoneUsageDescription` entitlement.

**System-audio capture needs more.** It uses ScreenCaptureKit, which requires a
screen-recording permission grant against a **signed** application bundle. Two consequences:

- It cannot be developed against an unsigned debug build.
- No build or CI process can grant it — the TCC prompt needs a human.

Get signing working before starting on system capture, not after. Tests covering that path
are `#[ignore]`d for this reason.

## Whisper

```sh
brew install cmake
./scripts/download-models.sh base.en
cargo build --workspace --features whisper
```
