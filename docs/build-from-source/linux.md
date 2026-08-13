# Building on Linux

## Requirements

- Rust 1.82+
- A C compiler (`build-essential` or equivalent) — for bundled SQLite

```sh
cargo build --workspace
cargo test --workspace
```

## Audio capture

PipeWire is the target. On a PulseAudio-only system, the PipeWire compatibility layer
(`pipewire-pulse`) is required.

```sh
# Debian/Ubuntu
sudo apt install build-essential libpipewire-0.3-dev
```

Loopback capture depends on the session manager's policy, which varies by distribution — this
is the least uniform of the three platforms.

## Whisper

```sh
sudo apt install cmake
./scripts/download-models.sh base.en
cargo build --workspace --features whisper
```
