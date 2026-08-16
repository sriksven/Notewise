//! Finding the parts of a workspace worth putting in front of a model.
//!
//! # Why retrieval at all
//!
//! Grounding a question on "the whole workspace" is not an option: a year of meetings is
//! millions of tokens, and even where a context window would hold it, the answer degrades —
//! the relevant three sentences are drowned by a hundred irrelevant pages. So the question is
//! used to select material first, and only the selection reaches the model.
//!
//! # Why the full-text index rather than embeddings
//!
//! The obvious design is a vector store: chunk everything, embed the chunks, embed the
//! question, take the nearest neighbours. It is also the design that stops working the moment
//! anything is unavailable. Embeddings need a model, and that model has to be the *same* model
//! that produced the stored vectors — swap it and every distance is quietly meaningless. Users
//! on a cloud backend would be shipping their whole workspace to a provider one chunk at a
//! time, which is the opposite of what this product promises. And it is a second index to
//! build, migrate and keep consistent.
//!
//! Meanwhile SQLite's FTS5 index is already here, already populated by triggers, already
//! consistent, and needs no model at all. Transcript segments are indexed individually, which
//! means the corpus arrives pre-chunked at exactly the granularity a speaker turn provides.
//! BM25 ranking floats documents matching more of the question's terms to the top.
//!
//! The honest trade-off: this is lexical, so it finds "pricing" and not "cost structure".
//! Synonyms are the thing it misses. That is a real limitation and it is stated in the UI
//! rather than hidden — but a retriever that always works beats a better one that needs a
//! download, a daemon, and a migration before it answers anything.
//!
//! When embeddings do arrive, they belong *beside* this rather than instead of it: hybrid
//! retrieval, with this as the floor.

use std::collections::BTreeMap;

use notewise_storage::{
    Database, Id, MeetingRepository, NoteRepository, SearchRepository, TicketRepository,
};

use crate::error::ApiResult;

/// How many index hits to consider before assembling passages.
///
/// Larger than the passage budget on purpose: several hits usually collapse into one passage
/// (many segments of one meeting), and hits pointing at trashed or deleted rows drop out.
const HIT_LIMIT: u32 = 60;

/// How many passages may reach the model.
const MAX_PASSAGES: usize = 8;

/// Roughly how much text may reach the model, in characters.
///
/// Characters rather than tokens because the token count depends on a tokenizer this crate
/// does not have and which differs per backend. ~12k characters is on the order of 3k tokens,
/// which every backend this app supports can hold alongside a conversation.
const MAX_CONTEXT_CHARS: usize = 12_000;

/// Longest single passage, so one enormous note cannot crowd out everything else.
const MAX_PASSAGE_CHARS: usize = 3_000;

/// Words carrying no retrieval signal.
///
/// Deliberately small and English-only. A long stopword list starts removing words that
/// matter ("no", "not", "cost"), and this list only has to stop a question's scaffolding from
/// matching every document in the workspace. Non-English input keeps all of its terms, which
/// is the safe failure: extra terms cost ranking quality, missing terms cost recall.
const STOPWORDS: &[&str] = &[
    "a", "about", "all", "am", "an", "and", "any", "are", "as", "at", "be", "been", "but", "by",
    "can", "did", "do", "does", "for", "from", "get", "had", "has", "have", "he", "her", "him",
    "his", "how", "i", "if", "in", "into", "is", "it", "its", "just", "me", "my", "of", "on", "or",
    "our", "out", "over", "she", "so", "than", "that", "the", "their", "them", "then", "there",
    "these", "they", "this", "to", "up", "us", "was", "we", "were", "what", "when", "where",
    "which", "who", "why", "will", "with", "would", "you", "your",
];

/// One piece of source material, with enough identity to cite it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Passage {
    /// `meeting`, `note`, or `ticket` — matching `NodeKind` naming.
    pub kind: &'static str,
    pub id: Id,
    pub title: String,
    pub text: String,
}

impl Passage {
    /// How this passage is labelled in the prompt and referred to in a citation.
    fn label(&self, index: usize) -> String {
        format!("[{}] {} — {}", index + 1, self.kind, self.title)
    }
}

/// Split a question into the terms worth searching for.
///
/// Public because the same tokenization decides whether a question is searchable at all: a
/// caller that gets an empty list back knows retrieval will find nothing and can say so
/// rather than silently answering from no material.
pub fn terms(query: &str) -> Vec<String> {
    let mut seen = Vec::new();

    for raw in query.split(|c: char| !c.is_alphanumeric()) {
        let term = raw.to_lowercase();

        // Single characters match far too much, and digits alone ("2024") are usually a date
        // fragment that pulls in every meeting held that year.
        if term.chars().count() < 2 || term.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if STOPWORDS.contains(&term.as_str()) {
            continue;
        }
        if seen.contains(&term) {
            continue;
        }

        seen.push(term);

        // A cap, because FTS5 query cost grows with the number of OR clauses and a pasted
        // paragraph would otherwise become a 200-clause query that matches everything.
        if seen.len() == 16 {
            break;
        }
    }

    seen
}

/// Material from the workspace relevant to `query`.
///
/// Returns an empty list when nothing matches, which callers must handle explicitly — a model
/// given no material and asked a question will answer from its own weights, and that answer
/// will look exactly like a grounded one.
pub fn gather(db: &Database, query: &str) -> ApiResult<Vec<Passage>> {
    let terms = terms(query);
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    let hits = SearchRepository::new(db).search_any(&terms, HIT_LIMIT)?;

    // Segments are grouped back into their meeting before anything else happens. Eight
    // separate hits from one standup are one passage, not eight, and de-duplicating after
    // truncation would have already thrown away the material that made the meeting rank.
    let mut segment_ids = Vec::new();
    let mut ordered: Vec<(&'static str, Id)> = Vec::new();

    for hit in &hits {
        match hit.entity_kind.as_str() {
            "transcript_segment" => segment_ids.push(hit.entity_id),
            "note" => push_unique(&mut ordered, ("note", hit.entity_id)),
            "ticket" => push_unique(&mut ordered, ("ticket", hit.entity_id)),
            // An index row for a kind this build does not know how to render. Skipping is
            // right: a future entity kind should not break search for everything else.
            _ => {}
        }
    }

    if !segment_ids.is_empty() {
        // Rank order is preserved: `segment_meetings` returns pairs, and the first time a
        // meeting appears is at its best-ranked segment.
        let owners = MeetingRepository::new(db).segment_meetings(&segment_ids)?;
        let by_segment: BTreeMap<Id, Id> = owners.into_iter().collect();
        for segment in &segment_ids {
            if let Some(meeting) = by_segment.get(segment) {
                push_unique(&mut ordered, ("meeting", *meeting));
            }
        }
    }

    let mut passages = Vec::new();
    let mut budget = MAX_CONTEXT_CHARS;

    for (kind, id) in ordered {
        if passages.len() == MAX_PASSAGES || budget == 0 {
            break;
        }

        let Some(passage) = load(db, kind, id) else {
            continue;
        };

        let allowance = budget.min(MAX_PASSAGE_CHARS);
        let text = truncate(&passage.text, allowance);
        if text.trim().is_empty() {
            continue;
        }

        budget = budget.saturating_sub(text.len());
        passages.push(Passage { text, ..passage });
    }

    Ok(passages)
}

fn push_unique(ordered: &mut Vec<(&'static str, Id)>, entry: (&'static str, Id)) {
    if !ordered.contains(&entry) {
        ordered.push(entry);
    }
}

/// Read one entity, or `None` if it is gone or trashed.
///
/// Errors are swallowed rather than propagated: the index can outlive a row, and one dangling
/// entry should not turn a question into a 500.
fn load(db: &Database, kind: &'static str, id: Id) -> Option<Passage> {
    match kind {
        "note" => {
            let note = NoteRepository::new(db).get(id).ok()?;
            // Trashed notes are excluded. A user who deleted a note does not expect it to
            // keep informing answers, and the trash is not a hidden knowledge base.
            if note.deleted_at.is_some() {
                return None;
            }
            Some(Passage {
                kind,
                id,
                title: note.title,
                text: note.body,
            })
        }
        "ticket" => {
            let ticket = TicketRepository::new(db).get(id).ok()?;
            Some(Passage {
                kind,
                id,
                title: ticket.title,
                text: ticket.description.unwrap_or_default(),
            })
        }
        "meeting" => {
            let repo = MeetingRepository::new(db);
            let meeting = repo.get(id).ok()?;
            Some(Passage {
                kind,
                id,
                title: meeting.title,
                text: repo.transcript_text(id).ok()?,
            })
        }
        _ => None,
    }
}

/// Cut to a character budget on a UTF-8 boundary, preferring a sentence or line break.
fn truncate(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.len() <= max {
        return text.to_string();
    }

    // Walk back to a boundary so the cut never lands mid-codepoint.
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }

    let head = &text[..end];
    // Prefer to end on a sentence, but only if that does not throw away most of the budget.
    let cut = head
        .rfind(['.', '\n'])
        .filter(|stop| *stop > end * 3 / 4)
        .map(|stop| stop + 1)
        .unwrap_or(end);

    format!("{}…", head[..cut].trim_end())
}

/// Lay passages out as the context block a model is asked to answer from.
///
/// Numbered, because the instruction to cite is worthless if there is nothing to cite *by*.
pub fn as_context(passages: &[Passage]) -> String {
    passages
        .iter()
        .enumerate()
        .map(|(index, passage)| format!("{}\n{}", passage.label(index), passage.text))
        .collect::<Vec<_>>()
        .join("\n\n---\n\n")
}

/// The standing instruction attached to every grounded answer.
///
/// The three rules that matter, in the order they get broken: answer only from the material,
/// cite which passage, and say so when the material does not contain the answer. The last is
/// the one worth the words — a model that invents a plausible decision nobody made is worse
/// than useless here, because the whole point is a record of what was actually said.
pub const GROUNDING_RULES: &str = "\
Answer only from the material above. Cite the passages you used by their number, like [2].
If the material does not contain the answer, say so plainly and stop — do not fill the gap \
from general knowledge, and do not guess. Quote exact wording when the question is about what \
someone actually said or decided.";

/// What the passages are, for a client that wants to show its working.
pub fn citations(passages: &[Passage]) -> Vec<serde_json::Value> {
    passages
        .iter()
        .enumerate()
        .map(|(index, passage)| {
            serde_json::json!({
                "n": index + 1,
                "kind": passage.kind,
                "id": passage.id,
                "title": passage.title,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use notewise_storage::{MeetingSource, NewMeeting, NewNote, NewTicket, NewTranscriptSegment};

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn meeting_with(db: &Database, title: &str, lines: &[&str]) -> Id {
        let repo = MeetingRepository::new(db);
        let meeting = repo
            .create(NewMeeting {
                project_id: None,
                title: title.into(),
                source: MeetingSource::Import,
                started_at: Utc.timestamp_opt(1_700_000_000, 0).single().unwrap(),
            })
            .expect("meeting");

        for (index, line) in lines.iter().enumerate() {
            repo.add_segment(NewTranscriptSegment {
                meeting_id: meeting.id,
                speaker: Some("Alex".into()),
                text: (*line).into(),
                start_ms: index as i64 * 1000,
                end_ms: index as i64 * 1000 + 900,
                confidence: None,
            })
            .expect("segment");
        }

        meeting.id
    }

    #[test]
    fn tokenizing_drops_scaffolding_and_keeps_content() {
        assert_eq!(
            terms("What did we decide about the pricing tiers?"),
            vec!["decide", "pricing", "tiers"]
        );
    }

    #[test]
    fn tokenizing_deduplicates_and_folds_case() {
        assert_eq!(terms("Pricing pricing PRICING"), vec!["pricing"]);
    }

    #[test]
    fn a_question_made_only_of_stopwords_yields_nothing() {
        assert!(terms("what is it about?").is_empty());
        assert!(terms("").is_empty());
        assert!(terms("??? !!!").is_empty());
    }

    /// A bare year matched every meeting held that year, which is the opposite of selective.
    #[test]
    fn bare_numbers_are_not_search_terms() {
        assert_eq!(terms("the 2024 pricing review"), vec!["pricing", "review"]);
    }

    #[test]
    fn a_question_with_no_searchable_terms_retrieves_nothing() {
        let db = db();
        meeting_with(&db, "Pricing", &["We agreed to raise prices."]);
        assert!(gather(&db, "what is it?").expect("gather").is_empty());
    }

    #[test]
    fn finds_the_meeting_whose_transcript_answers_the_question() {
        let db = db();
        let pricing = meeting_with(
            &db,
            "Pricing review",
            &[
                "We settled on three pricing tiers.",
                "Enterprise stays on a custom quote.",
            ],
        );
        meeting_with(&db, "Unrelated", &["The office coffee machine is broken."]);

        let passages = gather(&db, "what did we decide about pricing tiers?").expect("gather");

        assert_eq!(passages.len(), 1, "got {passages:?}");
        assert_eq!(passages[0].kind, "meeting");
        assert_eq!(passages[0].id, pricing);
        assert!(passages[0].text.contains("three pricing tiers"));
    }

    /// Several matching segments of one meeting are one passage. Before grouping, a long
    /// meeting could occupy every slot and hide every other source.
    #[test]
    fn many_matching_segments_collapse_into_one_meeting_passage() {
        let db = db();
        meeting_with(
            &db,
            "Standup",
            &[
                "pricing came up again",
                "pricing is still unresolved",
                "pricing owner is Dana",
                "pricing deadline is Friday",
            ],
        );

        let passages = gather(&db, "pricing").expect("gather");
        assert_eq!(passages.len(), 1);
        // All four lines survive into the single passage.
        assert!(passages[0].text.contains("Dana"), "{}", passages[0].text);
        assert!(passages[0].text.contains("Friday"), "{}", passages[0].text);
    }

    #[test]
    fn notes_and_tickets_are_retrievable_too() {
        let db = db();
        NoteRepository::new(&db)
            .create(NewNote {
                project_id: None,
                title: "Latency budget".into(),
                body: "Keep p99 latency under 200ms.".into(),
            })
            .expect("note");
        TicketRepository::new(&db)
            .create(NewTicket {
                project_id: None,
                title: "Reduce latency".into(),
                description: Some("Profile the slow query path.".into()),
                owner: None,
                due_at: None,
            })
            .expect("ticket");

        let kinds: Vec<_> = gather(&db, "what is our latency budget?")
            .expect("gather")
            .into_iter()
            .map(|p| p.kind)
            .collect();

        assert!(kinds.contains(&"note"), "got {kinds:?}");
        assert!(kinds.contains(&"ticket"), "got {kinds:?}");
    }

    /// A deleted note must stop informing answers. The trash is not a hidden knowledge base.
    #[test]
    fn trashed_notes_are_not_retrieved() {
        let db = db();
        let notes = NoteRepository::new(&db);
        let note = notes
            .create(NewNote {
                project_id: None,
                title: "Secret".into(),
                body: "The launch date is March.".into(),
            })
            .expect("note");

        assert_eq!(gather(&db, "when is the launch date?").unwrap().len(), 1);

        notes.trash(note.id).expect("trash");
        assert!(gather(&db, "when is the launch date?").unwrap().is_empty());
    }

    #[test]
    fn one_enormous_document_cannot_use_the_whole_budget() {
        let db = db();
        let filler = "pricing ".repeat(4_000);
        NoteRepository::new(&db)
            .create(NewNote {
                project_id: None,
                title: "Huge".into(),
                body: filler,
            })
            .expect("note");
        meeting_with(&db, "Short", &["pricing was discussed briefly"]);

        let passages = gather(&db, "pricing").expect("gather");
        assert_eq!(passages.len(), 2, "the short source must still fit");
        // The cap, plus the ellipsis truncation appends.
        let ceiling = MAX_PASSAGE_CHARS + '…'.len_utf8();
        assert!(
            passages.iter().all(|p| p.text.len() <= ceiling),
            "lengths: {:?}",
            passages.iter().map(|p| p.text.len()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn truncation_lands_on_a_character_boundary() {
        // Multi-byte throughout, so a naive byte slice would panic.
        let text = "é".repeat(500);
        let cut = truncate(&text, 101);
        assert!(cut.chars().count() <= 102);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn context_is_numbered_so_citations_can_refer_to_it() {
        let db = db();
        meeting_with(&db, "Pricing review", &["Three tiers."]);
        let passages = gather(&db, "pricing tiers").expect("gather");

        let context = as_context(&passages);
        assert!(
            context.starts_with("[1] meeting — Pricing review"),
            "{context}"
        );

        let citations = citations(&passages);
        assert_eq!(citations[0]["n"], 1);
        assert_eq!(citations[0]["kind"], "meeting");
    }

    #[test]
    fn a_hostile_question_does_not_break_the_query() {
        let db = db();
        meeting_with(&db, "Sync", &["pricing was discussed"]);

        // Quotes and FTS5 operators arriving as user input.
        let passages = gather(&db, "\"pricing\" OR NEAR(x) AND * pricing").expect("gather");
        assert_eq!(passages.len(), 1);
    }
}
