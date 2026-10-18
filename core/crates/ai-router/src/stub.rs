//! A loopback HTTP server for tests that need to see what a backend actually sends.
//!
//! Hand-rolled on `std::net` rather than taken as a dependency: it answers one request per
//! connection with a canned reply, which is all a backend test needs, and a mock-server crate
//! would be the largest thing in this crate's dev tree for that.
//!
//! The point is the wire, not the reply. A backend that builds the right struct and then sends
//! the key in the URL, or retries a refusal, looks correct to every test that stops at the
//! struct — so these tests stop at the socket instead.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

/// One request as it arrived.
#[derive(Debug, Clone)]
pub(crate) struct Recorded {
    pub method: String,
    /// Path and query, exactly as sent.
    pub target: String,
    pub body: String,
}

impl Recorded {
    /// The path without its query string.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or(&self.target)
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

/// What to answer with.
#[derive(Debug, Clone)]
pub(crate) struct Reply {
    pub status: u16,
    pub body: String,
    pub headers: Vec<(String, String)>,
}

impl Reply {
    pub fn json(status: u16, body: serde_json::Value) -> Self {
        Self {
            status,
            body: body.to_string(),
            headers: Vec::new(),
        }
    }
}

/// A server on an ephemeral port, answering every request with `respond`.
///
/// The accept loop lives for the rest of the test process. Tests are short-lived and each binds
/// its own port, so there is nothing to clean up and nothing shared between them.
#[derive(Debug)]
pub(crate) struct Stub {
    pub url: String,
    seen: Arc<Mutex<Vec<Recorded>>>,
}

impl Stub {
    pub fn start(respond: impl Fn(&Recorded) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let url = format!("http://{}", listener.local_addr().expect("local address"));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                let reply = respond(&request);
                log.lock().expect("stub log").push(request);
                let _ = write_reply(&mut stream, &reply);
            }
        });

        Self { url, seen }
    }

    /// Every request received so far, in arrival order.
    pub fn requests(&self) -> Vec<Recorded> {
        self.seen.lock().expect("stub log").clone()
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Recorded> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);

    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();

    let mut length = 0usize;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            }
        }
    }

    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;

    Some(Recorded {
        method,
        target,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn write_reply(stream: &mut TcpStream, reply: &Reply) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} STUB\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");

    stream.write_all(head.as_bytes())?;
    stream.write_all(reply.body.as_bytes())?;
    stream.flush()
}
