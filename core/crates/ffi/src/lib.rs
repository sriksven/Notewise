//! C ABI over the Notewise engine, for Swift and Kotlin.
//!
//! Exists so iOS and Android link the compiled core rather than reimplementing storage and
//! the graph per platform — three implementations of the same schema is three places for it
//! to drift.
//!
//! # Contract for callers
//!
//! 1. **Every returned string is owned by the caller** and must be released with
//!    [`nw_string_free`]. Freeing one any other way corrupts the allocator.
//! 2. **A null return means failure.** Call [`nw_last_error`] for the reason; it is
//!    thread-local, so a message never crosses threads.
//! 3. **An engine handle is not thread-safe.** It owns a SQLite connection. Use one handle
//!    per thread, or serialize access on the caller's side.
//! 4. **Every function is panic-safe.** A Rust panic unwinding across the FFI boundary is
//!    undefined behaviour, so each entry point catches panics and converts them to a null
//!    return plus an error message.
//!
//! ```c
//! NotewiseEngine *engine = nw_engine_open("/path/to/notewise.db");
//! if (!engine) { fprintf(stderr, "%s\n", nw_last_error()); return 1; }
//!
//! char *json = nw_meetings_json(engine, 20);
//! if (json) { puts(json); nw_string_free(json); }
//!
//! nw_engine_free(engine);
//! ```

#![warn(missing_debug_implementations)]

use std::cell::RefCell;
use std::ffi::{c_char, c_int, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};

use notewise_graph::{Graph, NodeKind, NodeRef};
use notewise_storage::{Database, Id, MeetingRepository, NoteRepository, SearchRepository};

thread_local! {
    /// Last error on this thread. Thread-local so a message from one thread cannot be read
    /// by another and misattributed.
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_error(message: impl Into<String>) {
    let message = message.into();
    // Interior nulls cannot go into a C string; replace rather than drop the message.
    let sanitized = message.replace('\0', "?");
    LAST_ERROR.with(|slot| {
        *slot.borrow_mut() = CString::new(sanitized).ok();
    });
}

fn clear_error() {
    LAST_ERROR.with(|slot| *slot.borrow_mut() = None);
}

/// An open engine. Opaque to C.
#[derive(Debug)]
pub struct NotewiseEngine {
    db: Database,
}

/// Run a body, converting any error or panic into a null return.
///
/// A panic unwinding across an FFI boundary is undefined behaviour; this is what makes every
/// entry point safe to call from Swift or Kotlin.
fn guard<T>(body: impl FnOnce() -> Result<*mut T, String>) -> *mut T {
    clear_error();
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(ptr)) => ptr,
        Ok(Err(message)) => {
            set_error(message);
            std::ptr::null_mut()
        }
        Err(_) => {
            set_error("internal error: a panic was caught at the FFI boundary");
            std::ptr::null_mut()
        }
    }
}

/// Convert a Rust string into a caller-owned C string.
fn to_c_string(value: String) -> Result<*mut c_char, String> {
    CString::new(value)
        .map(CString::into_raw)
        .map_err(|_| "result contained an interior null byte".to_string())
}

/// Read a C string argument.
///
/// # Safety
/// `ptr` must be null or a valid null-terminated C string.
unsafe fn read_str<'a>(ptr: *const c_char, name: &str) -> Result<&'a str, String> {
    if ptr.is_null() {
        return Err(format!("{name} must not be null"));
    }
    CStr::from_ptr(ptr)
        .to_str()
        .map_err(|_| format!("{name} must be valid UTF-8"))
}

// ---------------------------------------------------------------- lifecycle

/// Open (or create) a database and migrate it.
///
/// Returns null on failure; call [`nw_last_error`] for the reason. Release with
/// [`nw_engine_free`].
///
/// # Safety
/// `path` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn nw_engine_open(path: *const c_char) -> *mut NotewiseEngine {
    guard(|| {
        let path = read_str(path, "path")?;
        let db = Database::open(path).map_err(|e| e.to_string())?;
        Ok(Box::into_raw(Box::new(NotewiseEngine { db })))
    })
}

/// Open an in-memory database. Nothing is persisted; useful for tests and previews.
#[no_mangle]
pub extern "C" fn nw_engine_open_in_memory() -> *mut NotewiseEngine {
    guard(|| {
        let db = Database::open_in_memory().map_err(|e| e.to_string())?;
        Ok(Box::into_raw(Box::new(NotewiseEngine { db })))
    })
}

/// Release an engine. Safe to call with null.
///
/// # Safety
/// `engine` must be null, or a pointer from `nw_engine_open*` not yet freed.
#[no_mangle]
pub unsafe extern "C" fn nw_engine_free(engine: *mut NotewiseEngine) {
    if !engine.is_null() {
        drop(Box::from_raw(engine));
    }
}

/// The database's schema version, or `-1` on failure.
///
/// # Safety
/// `engine` must be a valid pointer from `nw_engine_open*`.
#[no_mangle]
pub unsafe extern "C" fn nw_schema_version(engine: *const NotewiseEngine) -> c_int {
    clear_error();
    let Some(engine) = engine.as_ref() else {
        set_error("engine must not be null");
        return -1;
    };

    match engine.db.schema_version() {
        Ok(version) => version as c_int,
        Err(e) => {
            set_error(e.to_string());
            -1
        }
    }
}

// ---------------------------------------------------------------- errors & strings

/// The last error on this thread, or null if the last call succeeded.
///
/// The returned pointer is owned by the library and valid until the next call on this
/// thread. Do **not** pass it to [`nw_string_free`].
#[no_mangle]
pub extern "C" fn nw_last_error() -> *const c_char {
    LAST_ERROR.with(|slot| match slot.borrow().as_ref() {
        Some(message) => message.as_ptr(),
        None => std::ptr::null(),
    })
}

/// Release a string returned by this library. Safe to call with null.
///
/// # Safety
/// `ptr` must be null, or a string returned by this library and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn nw_string_free(ptr: *mut c_char) {
    if !ptr.is_null() {
        drop(CString::from_raw(ptr));
    }
}

/// The library version. Owned by the library; do not free.
#[no_mangle]
pub extern "C" fn nw_version() -> *const c_char {
    // Null-terminated at compile time so no allocation or lifetime question arises.
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

// ---------------------------------------------------------------- queries

/// Recent meetings, as a JSON array. Release with [`nw_string_free`].
///
/// # Safety
/// `engine` must be a valid pointer from `nw_engine_open*`.
#[no_mangle]
pub unsafe extern "C" fn nw_meetings_json(
    engine: *const NotewiseEngine,
    limit: c_int,
) -> *mut c_char {
    guard(|| {
        let engine = engine.as_ref().ok_or("engine must not be null")?;
        let limit = limit.clamp(1, 1000) as u32;

        let meetings = MeetingRepository::new(&engine.db)
            .list_recent(limit)
            .map_err(|e| e.to_string())?;

        to_c_string(serde_json::to_string(&meetings).map_err(|e| e.to_string())?)
    })
}

/// One meeting's transcript as JSON. Release with [`nw_string_free`].
///
/// # Safety
/// `engine` must be valid; `meeting_id` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn nw_transcript_json(
    engine: *const NotewiseEngine,
    meeting_id: *const c_char,
) -> *mut c_char {
    guard(|| {
        let engine = engine.as_ref().ok_or("engine must not be null")?;
        let raw = read_str(meeting_id, "meeting_id")?;
        let id: Id = raw
            .parse()
            .map_err(|_| format!("'{raw}' is not a valid id"))?;

        let repo = MeetingRepository::new(&engine.db);
        repo.get(id).map_err(|e| e.to_string())?;
        let segments = repo.segments(id).map_err(|e| e.to_string())?;

        to_c_string(serde_json::to_string(&segments).map_err(|e| e.to_string())?)
    })
}

/// Full-text search, as a JSON array. Release with [`nw_string_free`].
///
/// # Safety
/// `engine` must be valid; `query` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn nw_search_json(
    engine: *const NotewiseEngine,
    query: *const c_char,
    limit: c_int,
) -> *mut c_char {
    guard(|| {
        let engine = engine.as_ref().ok_or("engine must not be null")?;
        let query = read_str(query, "query")?;
        let limit = limit.clamp(1, 200) as u32;

        let hits = SearchRepository::new(&engine.db)
            .search(query, limit)
            .map_err(|e| e.to_string())?;

        let json: Vec<_> = hits
            .into_iter()
            .map(|hit| {
                serde_json::json!({
                    "kind": hit.entity_kind,
                    "id": hit.entity_id.to_string(),
                    "title": hit.title,
                    "snippet": hit.snippet,
                })
            })
            .collect();

        to_c_string(serde_json::to_string(&json).map_err(|e| e.to_string())?)
    })
}

/// Recent notes, as a JSON array. Release with [`nw_string_free`].
///
/// # Safety
/// `engine` must be a valid pointer from `nw_engine_open*`.
#[no_mangle]
pub unsafe extern "C" fn nw_notes_json(engine: *const NotewiseEngine, limit: c_int) -> *mut c_char {
    guard(|| {
        let engine = engine.as_ref().ok_or("engine must not be null")?;
        let limit = limit.clamp(1, 1000) as u32;

        let notes = NoteRepository::new(&engine.db)
            .list_recent(limit)
            .map_err(|e| e.to_string())?;

        to_c_string(serde_json::to_string(&notes).map_err(|e| e.to_string())?)
    })
}

/// Everything connected to a meeting, as a JSON array. Release with [`nw_string_free`].
///
/// # Safety
/// `engine` must be valid; `meeting_id` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn nw_related_json(
    engine: *const NotewiseEngine,
    meeting_id: *const c_char,
    depth: c_int,
) -> *mut c_char {
    guard(|| {
        let engine = engine.as_ref().ok_or("engine must not be null")?;
        let raw = read_str(meeting_id, "meeting_id")?;
        let id: Id = raw
            .parse()
            .map_err(|_| format!("'{raw}' is not a valid id"))?;
        let depth = depth.clamp(1, Graph::MAX_DEPTH as c_int) as u32;

        let related = Graph::new(&engine.db)
            .related(NodeRef::new(NodeKind::Meeting, id), depth)
            .map_err(|e| e.to_string())?;

        let json: Vec<_> = related
            .into_iter()
            .map(|node| {
                serde_json::json!({
                    "kind": node.node.kind.as_str(),
                    "id": node.node.id.to_string(),
                    "distance": node.distance,
                    "via": node.via.as_str(),
                })
            })
            .collect();

        to_c_string(serde_json::to_string(&json).map_err(|e| e.to_string())?)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notewise_storage::{MeetingSource, NewMeeting, NewNote};

    /// Drive the FFI the way a C caller would, then release the string.
    unsafe fn take_string(ptr: *mut c_char) -> Option<String> {
        if ptr.is_null() {
            return None;
        }
        let value = CStr::from_ptr(ptr).to_string_lossy().into_owned();
        nw_string_free(ptr);
        Some(value)
    }

    unsafe fn last_error() -> Option<String> {
        let ptr = nw_last_error();
        if ptr.is_null() {
            None
        } else {
            Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
        }
    }

    #[test]
    fn version_is_a_valid_c_string() {
        let version = unsafe { CStr::from_ptr(nw_version()) }
            .to_str()
            .expect("valid UTF-8");
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn an_in_memory_engine_opens_and_frees() {
        unsafe {
            let engine = nw_engine_open_in_memory();
            assert!(!engine.is_null());
            assert!(last_error().is_none());
            assert_eq!(
                nw_schema_version(engine),
                notewise_storage::SUPPORTED_VERSION as c_int
            );
            nw_engine_free(engine);
        }
    }

    #[test]
    fn freeing_null_is_safe() {
        unsafe {
            nw_engine_free(std::ptr::null_mut());
            nw_string_free(std::ptr::null_mut());
        }
    }

    #[test]
    fn a_null_path_is_reported_not_dereferenced() {
        unsafe {
            let engine = nw_engine_open(std::ptr::null());
            assert!(engine.is_null());
            assert!(last_error().unwrap().contains("path"));
        }
    }

    #[test]
    fn a_null_engine_is_reported_not_dereferenced() {
        unsafe {
            assert!(nw_meetings_json(std::ptr::null(), 10).is_null());
            assert!(last_error().unwrap().contains("engine"));
            assert_eq!(nw_schema_version(std::ptr::null()), -1);
        }
    }

    #[test]
    fn an_unopenable_path_returns_null_with_a_message() {
        unsafe {
            let path = CString::new("/definitely/not/a/directory/x.db").unwrap();
            let engine = nw_engine_open(path.as_ptr());

            assert!(engine.is_null());
            assert!(last_error().is_some());
        }
    }

    #[test]
    fn the_error_slot_clears_on_success() {
        unsafe {
            // Provoke an error.
            assert!(nw_meetings_json(std::ptr::null(), 10).is_null());
            assert!(last_error().is_some());

            // A subsequent success must not leave the stale message readable.
            let engine = nw_engine_open_in_memory();
            assert!(last_error().is_none(), "stale error survived a success");
            nw_engine_free(engine);
        }
    }

    #[test]
    fn meetings_are_returned_as_json() {
        unsafe {
            let engine = nw_engine_open_in_memory();
            let json = take_string(nw_meetings_json(engine, 10)).expect("json");

            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert!(parsed.as_array().unwrap().is_empty());

            nw_engine_free(engine);
        }
    }

    #[test]
    fn limits_are_clamped_rather_than_overflowing() {
        unsafe {
            let engine = nw_engine_open_in_memory();

            // A negative limit from a caller must not become a huge unsigned value.
            assert!(!nw_meetings_json(engine, -5).is_null());
            assert!(!nw_meetings_json(engine, c_int::MAX).is_null());
            take_string(nw_meetings_json(engine, -5));
            take_string(nw_meetings_json(engine, c_int::MAX));

            nw_engine_free(engine);
        }
    }

    #[test]
    fn a_malformed_id_is_reported() {
        unsafe {
            let engine = nw_engine_open_in_memory();
            let bad = CString::new("not-a-uuid").unwrap();

            assert!(nw_transcript_json(engine, bad.as_ptr()).is_null());
            assert!(last_error().unwrap().contains("not-a-uuid"));

            nw_engine_free(engine);
        }
    }

    #[test]
    fn an_unknown_meeting_is_reported_not_returned_empty() {
        unsafe {
            let engine = nw_engine_open_in_memory();
            let id = CString::new(Id::new().to_string()).unwrap();

            assert!(
                nw_transcript_json(engine, id.as_ptr()).is_null(),
                "an unknown id must not look like an empty transcript"
            );
            assert!(last_error().is_some());

            nw_engine_free(engine);
        }
    }

    #[test]
    fn search_returns_results_from_seeded_data() {
        unsafe {
            let engine = nw_engine_open_in_memory();
            NoteRepository::new(&(*engine).db)
                .create(NewNote {
                    project_id: None,
                    title: "Migration plan".into(),
                    body: "Move to Postgres.".into(),
                })
                .unwrap();

            let query = CString::new("Postgres").unwrap();
            let json = take_string(nw_search_json(engine, query.as_ptr(), 10)).expect("json");

            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed.as_array().unwrap().len(), 1);
            assert_eq!(parsed[0]["kind"], "note");

            nw_engine_free(engine);
        }
    }

    #[test]
    fn notes_are_returned_as_json() {
        unsafe {
            let engine = nw_engine_open_in_memory();
            NoteRepository::new(&(*engine).db)
                .create(NewNote {
                    project_id: None,
                    title: "A note".into(),
                    body: String::new(),
                })
                .unwrap();

            let json = take_string(nw_notes_json(engine, 10)).expect("json");
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed[0]["title"], "A note");

            nw_engine_free(engine);
        }
    }

    #[test]
    fn a_null_query_is_reported() {
        unsafe {
            let engine = nw_engine_open_in_memory();
            assert!(nw_search_json(engine, std::ptr::null(), 10).is_null());
            assert!(last_error().unwrap().contains("query"));
            nw_engine_free(engine);
        }
    }

    #[test]
    fn errors_containing_a_null_byte_do_not_lose_the_message() {
        set_error("bad\0input");
        let message = unsafe { last_error() }.expect("message");
        assert!(message.contains("bad"), "{message}");
    }

    #[test]
    fn traversal_depth_is_clamped_to_the_graph_maximum() {
        unsafe {
            let engine = nw_engine_open_in_memory();
            let repo = MeetingRepository::new(&(*engine).db);
            let meeting = repo
                .create(NewMeeting {
                    project_id: None,
                    title: "Sync".into(),
                    source: MeetingSource::Microphone,
                    started_at: chrono::Utc::now(),
                })
                .unwrap();

            let id = CString::new(meeting.id.to_string()).unwrap();
            // Would exceed Graph::MAX_DEPTH and error if not clamped.
            let json = take_string(nw_related_json(engine, id.as_ptr(), 9999));
            assert!(json.is_some(), "{:?}", last_error());

            nw_engine_free(engine);
        }
    }

    #[test]
    fn multiple_engines_coexist() {
        unsafe {
            let a = nw_engine_open_in_memory();
            let b = nw_engine_open_in_memory();

            assert!(!a.is_null() && !b.is_null());
            assert_ne!(a, b);

            nw_engine_free(a);
            nw_engine_free(b);
        }
    }
}
