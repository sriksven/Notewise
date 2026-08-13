# Mobile apps

> **Status: scaffold.** No implementation yet — this directory exists so the architecture does not need reshaping when Phase 2 arrives. See [ROADMAP.md](../../ROADMAP.md).

Both platforms link the compiled engine through the [`ffi`](../../core/crates/ffi/) crate —
iOS via an XCFramework, Android via a `.aar` with a JNI bridge. Neither reimplements storage
or the graph; three implementations of one schema is three places for it to drift.

| Path | Toolchain | Links |
|---|---|---|
| `ios/` | SwiftUI | `libnotewise_ffi.a` in an XCFramework |
| `android/` | Kotlin | `libnotewise_ffi.so` via JNI |

## Before writing app code

Read the caller contract in the [`ffi` crate docs](../../core/crates/ffi/src/lib.rs). Three
rules matter most: every returned string must be released with `nw_string_free`, a null
return means failure and the reason is in `nw_last_error()`, and an engine handle is **not**
thread-safe — one per thread, or serialize access.

## Why this is not built yet

Building it needs Xcode with a signing identity and an Android SDK/NDK, and verifying it
needs simulators. None of that exists in a headless build environment, so shipping a
scaffold is honest and shipping untested app code would not be.
