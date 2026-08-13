# Applications

Thin shells around the engine in [`core/`](../core/). None of them reimplement storage,
transcription, or the graph — that is the entire point of the `ffi` crate.

| App | State | Phase |
|---|---|---|
| [`cli/`](cli/) | **Implemented** | 0 |
| [`desktop/`](desktop/) | Scaffold | 0–1 |
| [`mobile/`](mobile/) | Scaffold | 2 |
| [`wearable/`](wearable/) | Scaffold | 3 |
| [`browser-extension/`](browser-extension/) | Scaffold | 3 |
| [`web-dashboard/`](web-dashboard/) | Scaffold | 3 |

MIT licensed, same as `core/`.
