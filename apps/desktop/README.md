# Desktop app

> **Status: scaffold.** The Tauri shell is not built yet. The engine it will wrap is
> complete and can be driven today through [`apps/cli`](../cli/) or the local REST API.

Tauri shell around the engine, running it **in-process** rather than shipping a separate
daemon — a background process the user cannot see is a support burden and a trust problem for
a product that records meetings.

## Layout

| Path | Purpose |
|---|---|
| `src-tauri/` | Rust glue; Tauri commands calling into `core/` |
| `src/` | Frontend |
| `platform/macos/` | Entitlements for audio capture, menu-bar integration |
| `platform/windows/` | Installer config, WASAPI permission flow |
| `platform/linux/` | AppImage/deb packaging, PipeWire permission flow |

`src-tauri` is deliberately **excluded from the Cargo workspace** (see the root
`Cargo.toml`): Tauri pulls a webview toolchain, and engine CI should not depend on GUI build
tooling to run.

## The constraint to solve first

macOS system-audio capture needs a ScreenCaptureKit permission grant against a **signed**
bundle. That means the permission flow cannot be developed against an unsigned debug build —
signing has to work before capture can be tested at all. Plan for it early rather than
discovering it at the end.
