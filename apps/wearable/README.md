# Wearable companions

> **Status: scaffold.** No implementation yet — this directory exists so the architecture does not need reshaping when Phase 3 arrives. See `docs/roadmap.md`.

**Wearables do not run the engine.** They are a remote control and a glanceable display for
the paired phone, which itself talks to the desktop engine or the cloud.

This is worth being explicit about because the natural reading of "wearable support" is
"port the engine to watchOS", and that is not the plan — it is a much smaller companion
surface. Scope it as: start/stop recording, show the current meeting, surface an action item.

| Path | Platform |
|---|---|
| `watchos/` | watchOS, paired with the iOS app |
| `wearos/` | Wear OS, paired with the Android app |
