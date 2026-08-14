//! Speaker identity reported by a local producer.
//!
//! # What posts here
//!
//! A browser extension running in the user's own meeting tab. Meet, Zoom, and Teams all display
//! who is speaking, because they route the audio and therefore know; a content script reads that
//! and posts it. The desktop app already has the audio, so this carries **only identity** — a few
//! hundred bytes a minute, not a media stream.
//!
//! That split is what makes named speakers affordable. The alternative reading of "capture
//! everyone's audio" — a bot that joins the call to obtain a stream we already have — pays for the
//! expensive half to get the cheap half.
//!
//! # Two rules this endpoint enforces
//!
//! **Timestamps are relative to recording start.** The producer is a browser tab and the consumer
//! is an audio pipeline; their clocks are unrelated and the offset is unrecoverable afterwards. A
//! negative or inverted turn is rejected rather than stored, because it would silently poison every
//! overlap calculation downstream.
//!
//! **Accumulate now, apply at the end.** Posting does not relabel anything. The timeline is applied
//! once the meeting ends, for the same reason the rest of the pipeline works that way: speaker
//! evidence keeps arriving, and a name written from partial evidence cannot be corrected later —
//! refinement only replaces a channel label, and a segment already named no longer carries one.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use notewise_diarization::{Participant, ParticipantId, SpeakerTimeline, TimelineDiarizer};
use notewise_recorder::{refine_channel_speakers, Channel};
use notewise_storage::{Database, Id, MeetingRepository};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

type Shared = std::sync::Arc<AppState>;

/// One participant, as posted.
#[derive(Debug, Deserialize)]
pub struct PostedParticipant {
    pub id: String,
    pub display_name: String,
}

/// One stretch of speech, as posted.
#[derive(Debug, Deserialize)]
pub struct PostedTurn {
    pub participant: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// A batch of speaker events.
///
/// Batched for the same reason transcript segments are: a producer watching an active-speaker
/// indicator emits events several times a second, and one request each would swamp the loopback
/// API during a recording.
#[derive(Debug, Deserialize)]
pub struct SpeakerEvents {
    /// The roster. Sent with every batch so a late joiner or a rename is picked up without the
    /// producer tracking what it has already told us.
    #[serde(default)]
    pub participants: Vec<PostedParticipant>,

    #[serde(default)]
    pub turns: Vec<PostedTurn>,

    /// Which participant is the user at this machine, when the producer can tell.
    ///
    /// Their turns are dropped before the remote channel is refined — the system audio tap records
    /// what the machine plays, which is everyone *except* them. See
    /// [`SpeakerTimeline::excluding`].
    #[serde(default)]
    pub local_participant_id: Option<String>,
}

/// What the server did with a batch.
#[derive(Debug, Serialize)]
pub struct AcceptedEvents {
    pub participants_known: usize,
    pub turns_known: usize,
}

/// Accept speaker events for a meeting.
///
/// Rejects a turn naming a participant absent from the roster. The producer sends the roster in the
/// same request, so a mismatch is a producer bug, and storing the turn would leave a hole that a
/// later pass might fill from a neighbour — attributing words to whoever happened to be adjacent.
pub async fn post_speaker_events(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<SpeakerEvents>,
) -> ApiResult<Json<AcceptedEvents>> {
    let meeting_id: Id = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("'{id}' is not a valid id")))?;

    if body.participants.is_empty() && body.turns.is_empty() {
        return Err(ApiError::BadRequest("no speaker events supplied".into()));
    }

    // Confirm the meeting exists before accumulating anything against it.
    {
        let db = state.db().await;
        MeetingRepository::new(&db).get(meeting_id)?;
    }

    let mut timelines = state.speaker_timelines().lock().await;
    let entry = timelines.entry(meeting_id).or_default();

    for participant in body.participants {
        entry
            .timeline
            .upsert_participant(Participant::new(participant.id, participant.display_name));
    }

    if let Some(local) = body.local_participant_id {
        entry.local = Some(ParticipantId::new(local));
    }

    for turn in body.turns {
        entry
            .timeline
            .add_turn(
                ParticipantId::new(turn.participant),
                turn.start_ms,
                turn.end_ms,
            )
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    }

    Ok(Json(AcceptedEvents {
        participants_known: entry.timeline.participants().len(),
        turns_known: entry.timeline.turns().len(),
    }))
}

/// A meeting's accumulated speaker evidence.
#[derive(Debug, Default)]
pub struct PendingTimeline {
    pub timeline: SpeakerTimeline,
    /// The local user, whose turns do not belong to the system channel.
    pub local: Option<ParticipantId>,
}

/// Timelines awaiting the end of their meeting.
///
/// Keyed by meeting and drained by [`apply_pending_timeline`], so an entry's lifetime is its
/// recording's. Nothing else clears this map — a meeting that never ends keeps its timeline, which
/// is the intended behaviour and also the thing to watch if it ever grows.
pub type PendingTimelines = tokio::sync::Mutex<HashMap<Id, PendingTimeline>>;

/// Remove a meeting's accumulated events, if any were posted.
///
/// Separate from [`apply_timeline`] so the caller can await this **before** taking the database
/// lock. `Database` is `Send` but not `Sync`, so a guard held across any `.await` makes the whole
/// handler's future non-`Send` and axum rejects it. Draining first keeps the async and the
/// database-holding halves apart.
pub async fn take_pending(pending: &PendingTimelines, meeting_id: Id) -> Option<PendingTimeline> {
    pending.lock().await.remove(&meeting_id)
}

/// Name the speakers on a meeting's remote channel from posted events.
///
/// Returns how many segments gained a name. Synchronous, so it can run while the database guard is
/// held — see [`take_pending`].
///
/// # Only the remote channel
///
/// [`Channel::Microphone`] segments are already attributed exactly: they are the person at this
/// machine, established by which cable the audio arrived on rather than inferred. Platform events
/// are lower-quality evidence than that, so they are not allowed to overwrite it.
pub fn apply_timeline(db: &Database, meeting_id: Id, entry: PendingTimeline) -> Option<usize> {
    let timeline = match &entry.local {
        Some(local) => entry.timeline.excluding(local),
        None => entry.timeline,
    };

    if timeline.is_empty() {
        return None;
    }

    let diarizer = TimelineDiarizer::new(timeline);

    match refine_channel_speakers(db, meeting_id, Channel::System, &diarizer) {
        Ok(refined) => {
            tracing::info!(%meeting_id, refined, "named speakers from platform events");
            Some(refined)
        }
        // A failed naming pass must not fail the recording that produced the transcript. The
        // words are stored and correct; only the labels are missing.
        Err(e) => {
            tracing::warn!(%meeting_id, error = %e, "could not apply speaker timeline");
            None
        }
    }
}
