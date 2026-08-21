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

/// Register the hotkey and start listening for presses.
///
/// Must be called on the main thread, before the event loop starts. Failures are logged and not
/// fatal: an app that will not open because another program holds a key combination is worse than
/// one whose dictation shortcut does not work.
pub fn install(engine: SocketAddr, hotkey: &str) {
    let binding = match Binding::parse(hotkey) {
        Ok(binding) => binding,
        Err(e) => {
            tracing::warn!(hotkey, error = %e, "the dictation hotkey is not usable");
            return;
        }
    };

    let Some(presses) = native::listen() else {
        tracing::warn!("something is already listening for hotkeys");
        return;
    };

    match native::register("dictation", &binding) {
        Ok(registration) => {
            tracing::info!(hotkey = %binding, "dictation hotkey ready");
            // Lives for the process; see the module docs.
            std::mem::forget(registration);
        }
        Err(e) => {
            tracing::warn!(hotkey = %binding, error = %e, "could not claim the dictation hotkey");
            return;
        }
    }

    // The receiver is `Send`, so the work happens off the main thread — a dictation that takes
    // several seconds to transcribe must not freeze the window.
    std::thread::Builder::new()
        .name("notewise-dictation-hotkey".into())
        .spawn(move || {
            let listening = Arc::new(AtomicBool::new(false));

            while let Ok(feature) = presses.recv() {
                if feature != "dictation" {
                    continue;
                }
                toggle(engine, &listening);
            }
        })
        .map(|_| ())
        .unwrap_or_else(|e| tracing::warn!(error = %e, "could not start the hotkey thread"));
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
