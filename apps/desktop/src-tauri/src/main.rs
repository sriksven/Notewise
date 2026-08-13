//! The Notewise desktop shell.
//!
//! Runs the engine **in-process** and opens a native window onto it. There is no background
//! daemon: a hidden process that records meetings is both a support burden and a trust
//! problem, and "quit the app and it stops listening" should be literally true.
//!
//! # Why the window points at `http://127.0.0.1:<port>`
//!
//! The obvious setup — Tauri serving the frontend from its own bundle and the frontend
//! calling the engine over HTTP — is cross-origin, and would require permissive CORS on an
//! unauthenticated loopback API. That would let any page the user visits read their meetings,
//! since same-origin policy is the only thing currently preventing it.
//!
//! Serving the UI from the engine keeps everything same-origin, so no CORS is needed at all.
//! Tauri's job is then precisely what it is actually needed for: a native window, an app
//! bundle, and the `Info.plist` entries that make the microphone and system-audio permission
//! grants possible. None of that requires IPC.

// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::PathBuf;

use notewise_ai_router::{BackendKind, Router as AiRouter, RouterConfig};
use notewise_api_server::{AppState, Server};
use notewise_storage::Database;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("NOTEWISE_LOG")
                .unwrap_or_else(|_| "notewise=info,notewise_shell=info".into()),
        )
        .init();

    tauri::Builder::default()
        .setup(|app| {
            // Bind before opening the window. If the window loaded first it would race the
            // server and show a connection error on a cold launch.
            let addr = start_engine(app.handle())?;
            let url = format!("http://{addr}");
            tracing::info!(%url, "engine ready");

            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url.parse()?))
                .title("Notewise")
                .inner_size(1040.0, 700.0)
                .min_inner_size(720.0, 480.0)
                .resizable(true)
                // Native traffic lights and title bar: this is a document-shaped app, and a
                // custom chrome would only reimplement what the OS already does correctly.
                .decorations(true)
                .build()?;

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to start the Notewise shell");
}

/// Start the engine on loopback and return the address it actually bound.
fn start_engine(app: &tauri::AppHandle) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    let data_dir = data_dir(app)?;
    std::fs::create_dir_all(&data_dir)?;

    let db = Database::open(data_dir.join("notewise.db"))?;
    let ai = AiRouter::from_config(backend_config())?;

    tracing::info!(
        db = %data_dir.join("notewise.db").display(),
        backend = ai.model_id(),
        local = ai.is_local(),
        "engine configured"
    );

    let state = AppState::new(db, ai).with_model_dir(model_dir(&data_dir));
    let frontend = frontend_dir(app)?;

    // Port 0 asks the OS for a free port. A fixed port would collide with a `notewise serve`
    // already running, and failing to launch because a CLI is open would be a poor trade.
    let server = Server::bind("127.0.0.1:0")?;

    // The engine runs on its own runtime thread. Tauri owns the main thread for the event
    // loop, and blocking it would freeze the window.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("notewise-engine".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Runtime::new() {
                Ok(runtime) => runtime,
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                    return;
                }
            };

            runtime.block_on(async move {
                match server.bind_with_frontend(state, frontend).await {
                    Ok((addr, serving)) => {
                        if tx.send(Ok(addr)).is_err() {
                            return; // shell gave up
                        }
                        if let Err(e) = serving.await {
                            tracing::error!(error = %e, "engine stopped");
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                    }
                }
            });
        })?;

    rx.recv_timeout(std::time::Duration::from_secs(10))
        .map_err(|_| "the engine did not start in time")?
        .map_err(|e| e.into())
}

/// Where the database lives.
///
/// Uses Tauri's resolved app-data directory, so it follows each platform's convention and
/// stays inside the sandbox container if the app is ever sandboxed.
fn data_dir(app: &tauri::AppHandle) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(dir) = std::env::var("NOTEWISE_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(app.path().app_data_dir()?)
}

/// Where transcription models live.
///
/// Beside the database inside the app-data container by default, so they survive an app update
/// and are removed with the app's data rather than left as orphaned gigabytes elsewhere.
/// `NOTEWISE_MODEL_DIR` overrides it, which is what lets the app share a model the CLI already
/// downloaded — a 148 MB file is not worth fetching twice.
fn model_dir(data_dir: &std::path::Path) -> PathBuf {
    std::env::var("NOTEWISE_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| data_dir.join("models"))
}

/// Where the built frontend lives.
///
/// In a bundle it is a packaged resource. In development it is `apps/desktop/dist`, so
/// `npm run build && cargo run` works without packaging anything.
fn frontend_dir(app: &tauri::AppHandle) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(dir) = std::env::var("NOTEWISE_FRONTEND_DIR") {
        return Ok(PathBuf::from(dir));
    }

    if let Ok(resource) = app.path().resolve("dist", tauri::path::BaseDirectory::Resource) {
        if resource.join("index.html").exists() {
            return Ok(resource);
        }
    }

    // Development fallback: CARGO_MANIFEST_DIR is apps/desktop/src-tauri.
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("could not resolve the desktop app directory")?
        .join("dist");

    if !dev.join("index.html").exists() {
        return Err(format!(
            "no frontend found at {} — run `npm run build` in apps/desktop first",
            dev.display()
        )
        .into());
    }

    Ok(dev)
}

/// Choose an AI backend from the environment.
///
/// Mirrors the CLI's resolution so the desktop app and `notewise status` never disagree about
/// which backend is active. Defaults to local: a user who has configured nothing must not have
/// their meetings uploaded anywhere.
fn backend_config() -> RouterConfig {
    let apply = |mut config: RouterConfig| {
        if let Ok(model) = std::env::var("NOTEWISE_MODEL") {
            config = config.with_model(model);
        }
        if let Ok(endpoint) = std::env::var("NOTEWISE_ENDPOINT") {
            config = config.with_endpoint(endpoint);
        }
        config
    };

    if let Ok(name) = std::env::var("NOTEWISE_BACKEND") {
        if let Some(kind) = BackendKind::parse(name.trim()) {
            let mut config = RouterConfig::new(kind);
            if let Some(key) = key_for(kind) {
                config = config.with_api_key(key);
            }
            return apply(config);
        }
    }

    for kind in [
        BackendKind::Anthropic,
        BackendKind::Gemini,
        BackendKind::Groq,
        BackendKind::OpenRouter,
    ] {
        if let Some(key) = key_for(kind) {
            return apply(RouterConfig::new(kind).with_api_key(key));
        }
    }

    apply(RouterConfig::ollama())
}

fn key_for(kind: BackendKind) -> Option<String> {
    let name = match kind {
        BackendKind::Anthropic => "ANTHROPIC_API_KEY",
        BackendKind::Gemini => "GEMINI_API_KEY",
        BackendKind::Groq => "GROQ_API_KEY",
        BackendKind::OpenRouter => "OPENROUTER_API_KEY",
        _ => return None,
    };
    std::env::var(name).ok().filter(|k| !k.trim().is_empty())
}
