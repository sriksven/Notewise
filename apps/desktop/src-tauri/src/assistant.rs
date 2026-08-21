//! The dictation hotkey.
//!
//! # Why this is in the shell and not the engine
//!
//! A global hotkey needs two things the engine does not have: the main thread, and a run loop to
//! dispatch on. Carbon delivers a hot-key event to the application event target, which is serviced
//! by whatever is running the main thread — and in this app that is Tauri. An engine started on a
//! worker thread could register a hotkey successfully and never hear it pressed.
//!
//! So the shell owns the key and the engine owns the work, which is the same division as recording:
//! the window knows when the user asked, the engine knows how to do it.
//!
//! # Why the registration is leaked
//!
//! [`notewise_os_input::native::Registration`] is deliberately not `Send`, because unregistering has
//! to happen on the thread that registered. It lives for the process, so it is leaked on purpose
//! rather than moved somewhere it cannot go. The alternative — keeping it in a main-thread-only
//! store — would be ceremony around a value nothing will ever drop early.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use notewise_os_input::{native, Binding};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// Register the assistant's hotkeys and start listening for presses.
///
/// Must be called on the main thread, before the event loop starts. Failures are logged and not
/// fatal, per key: an app that will not open because another program holds one combination is worse
/// than one whose dictation shortcut does not work, and a user who lost the panel's key should still
/// have dictation.
pub fn install(app: &AppHandle, engine: SocketAddr, hotkeys: &[(String, String)]) {
    let Some(presses) = native::listen() else {
        tracing::warn!("something is already listening for hotkeys");
        return;
    };

    for (feature, hotkey) in hotkeys {
        let binding = match Binding::parse(hotkey) {
            Ok(binding) => binding,
            Err(e) => {
                tracing::warn!(%feature, hotkey, error = %e, "that hotkey is not usable");
                continue;
            }
        };

        match native::register(feature, &binding) {
            Ok(registration) => {
                tracing::info!(%feature, hotkey = %binding, "global hotkey ready");
                // Lives for the process; see the module docs.
                std::mem::forget(registration);
            }
            Err(e) => {
                tracing::warn!(%feature, hotkey = %binding, error = %e, "could not claim it")
            }
        }
    }

    // The receiver is `Send`, so the work happens off the main thread — a dictation that takes
    // several seconds to transcribe must not freeze the window. Showing the panel is the exception:
    // window work has to be back on the main thread, which `run_on_main_thread` does.
    let app = app.clone();
    std::thread::Builder::new()
        .name("notewise-assistant-hotkeys".into())
        .spawn(move || {
            let listening = Arc::new(AtomicBool::new(false));

            while let Ok(feature) = presses.recv() {
                match feature.as_str() {
                    "dictation" => toggle(engine, &listening),
                    "overlay" => {
                        let handle = app.clone();
                        let _ = app.run_on_main_thread(move || show_overlay(&handle, engine));
                    }
                    other => tracing::debug!(other, "a hotkey nothing is listening for"),
                }
            }
        })
        .map(|_| ())
        .unwrap_or_else(|e| tracing::warn!(error = %e, "could not start the hotkey thread"));
}

/// Show the assistant panel, building it the first time.
///
/// Built lazily rather than at launch: the window reads the focused application when it opens, and
/// one created at startup and hidden would have to be told to re-read. Kept afterwards rather than
/// destroyed, because rebuilding a webview on every press is a visible delay on the thing a user
/// presses most.
///
/// Must run on the main thread. Window creation on macOS is main-thread-only, and Tauri will panic
/// rather than misbehave — which is the right trade but not one to discover at runtime.
fn show_overlay(app: &AppHandle, engine: SocketAddr) {
    const LABEL: &str = "assistant";

    if let Some(window) = app.get_webview_window(LABEL) {
        // Already there: bring it forward and give it the keyboard. A panel that appears behind the
        // window the user was looking at is a panel they will think did not open.
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }

    let url = format!("http://{engine}/#/overlay");
    let parsed = match url.parse() {
        Ok(parsed) => parsed,
        Err(e) => {
            tracing::warn!(error = %e, "could not address the assistant panel");
            return;
        }
    };

    let built = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::External(parsed))
        .title("Notewise Assistant")
        .inner_size(520.0, 360.0)
        .min_inner_size(380.0, 220.0)
        // Above whatever the user is working in, because that is the point of it.
        .always_on_top(true)
        .resizable(true)
        // No traffic lights: this is a panel, not a document window, and Escape closes it.
        .decorations(false)
        .center()
        .focused(true)
        .build();

    match built {
        Ok(window) => {
            let _ = window.set_focus();
        }
        Err(e) => tracing::warn!(error = %e, "could not open the assistant panel"),
    }
}

/// One press: start listening, or stop and insert.
///
/// The flag is this thread's own idea of the state rather than a question asked of the engine. That
/// is a deliberate trade: a status round trip on every press would double the latency of the thing
/// a user notices most, and the two can only disagree if something else drove dictation — in which
/// case the recovery is one more press.
fn toggle(engine: SocketAddr, listening: &AtomicBool) {
    let was_listening = listening.load(Ordering::Relaxed);

    let (method, what) = if was_listening {
        ("DELETE", "stopped")
    } else {
        ("POST", "started")
    };

    match request(engine, method, "/v1/dictation") {
        Ok(status) if (200..300).contains(&status) => {
            listening.store(!was_listening, Ordering::Relaxed);
            tracing::info!("dictation {what}");
        }
        Ok(status) => {
            // A 409 on start means something else is already listening; on stop it means nothing
            // is. Either way this thread's flag was wrong, so it is corrected rather than retried.
            listening.store(was_listening, Ordering::Relaxed);
            if status == 409 {
                listening.store(!was_listening, Ordering::Relaxed);
            }
            tracing::warn!(status, "dictation {what} was refused");
        }
        Err(e) => tracing::warn!(error = %e, "could not reach the engine"),
    }
}

/// One HTTP request to the local engine.
///
/// Hand-rolled rather than pulling an HTTP client into the shell. It is one loopback request with no
/// body, no redirects, and no TLS, and only the status line is read — a client library here would be
/// a megabyte of dependency for a string.
fn request(engine: SocketAddr, method: &str, path: &str) -> std::io::Result<u16> {
    let mut stream = TcpStream::connect_timeout(&engine, Duration::from_secs(2))?;
    // Generous: a stop transcribes before it answers, and a long sentence on a slow machine takes
    // real time. Bounded so a wedged engine does not leave this thread waiting forever.
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {engine}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    stream.flush()?;

    // Only the status line is needed, so only enough to hold one is read.
    let mut head = [0u8; 64];
    let read = stream.read(&mut head)?;
    let line = String::from_utf8_lossy(&head[..read]);

    line.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("the engine answered with '{}'", line.trim()),
            )
        })
}
