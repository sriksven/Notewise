# Building on Windows

## Requirements

- Rust 1.82+ (MSVC toolchain)
- Visual Studio Build Tools with the C++ workload — needed for bundled SQLite

```powershell
cargo build --workspace
cargo test --workspace
```

## Audio capture

System audio uses WASAPI loopback, which needs no special permission — simpler than macOS.
Microphone capture is subject to the Windows privacy setting for microphone access.

## Whisper

Install CMake and add it to `PATH`, then:

```powershell
.\scripts\download-models.sh base.en   # or download manually
cargo build --workspace --features whisper
```
