//! Finding the parts of a workspace worth putting in front of a model.
//!
//! # Why retrieval at all
//!
//! Grounding a question on "the whole workspace" is not an option: a year of meetings is
//! millions of tokens, and even where a context window would hold it, the answer degrades —
//! the relevant three sentences are drowned by a hundred irrelevant pages. So the question is
//! used to select material first, and only the selection reaches the model.
//!
//! # Two retrievers, not one
//!
//! **Lexical**, over SQLite's FTS5 index. Already here, already populated by triggers, needs
//! no model, and never unavailable. BM25 floats documents matching more of the question's
//! terms to the top. What it cannot do is synonyms: ask about "pricing" and it will not find a
//! meeting that only ever said "cost structure".
//!
//! **Semantic**, over locally-computed embeddings ([`crate::indexing`]). This is what closes
//! that gap. It needs Ollama and an indexing pass, so it is not always available — and when it
//! is not, everything still works, less well.
//!
//! Neither subsumes the other, which is why both run. Lexical wins on names, identifiers,
//! quoted phrases and rare words — the things an embedding smooths away. Semantic wins on
//! paraphrase. A question usually contains both.
//!
//! # How the two are combined
//!
//! Reciprocal Rank Fusion: each retriever produces a ranking, and an entity's score is the sum
//! of `1 / (k + rank)` over the rankings it appears in.
//!
//! The alternative — normalising BM25 scores and cosine similarities onto a common scale and
//! adding them — requires the two to be comparable, and they are not. BM25 is unbounded and
//! corpus-dependent; cosine is bounded and says nothing about how good the *best* match is.
//! Tuning a weight between them means tuning it per workspace. RRF discards the magnitudes and
//! keeps only the order, which is the part both retrievers agree means something, and it needs
//! no tuning: an entity both retrievers rank highly beats one that either ranks alone.

use notewise_ai_router::cosine;
use notewise_storage::EmbeddingRepository;

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

/// The constant in Reciprocal Rank Fusion's `1 / (k + rank)`.
///
/// 60 is the value from the original paper and the de-facto default. Its effect is to flatten
/// the difference between the top few ranks, so a result that both retrievers place in their
/// top handful outranks one that a single retriever placed first — which is the behaviour the
/// fusion exists to produce.
const RRF_K: f32 = 60.0;

/// How many semantic neighbours to consider.
const VECTOR_LIMIT: usize = 40;

/// How close to the best match a neighbour must score to be kept.
///
/// **This is relative on purpose, and the absolute threshold it replaced was a bug.**
///
/// Measured against a real workspace with `nomic-embed-text`: the question "what did we decide
/// about pricing tiers?" scored 0.607 against the meeting that answered it, 0.435 against an
/// unrelated meeting about a broken coffee machine, and 0.292 against a note containing the
/// words "typing here". A fixed floor low enough to admit the first admits all three.
///
/// The baseline differs per model — some compress everything into a narrow high band, others
/// spread out — so any absolute number is calibrated for one embedder and wrong for the next.
/// A ratio to the best match is not: 0.607 × 0.85 keeps only the meeting that answered it.
const RELATIVE_KEEP: f32 = 0.85;

/// How far the best match must stand above a typical one for anything to count as a match.
///
/// The relative rule alone cannot tell "one good match and some noise" from "nothing here is
/// relevant" — in both cases something scores highest. What separates them is whether the best
/// score *stands out*. When every chunk scores about the same, the top one is not an answer,
/// it is an accident of ordering, and the refusal path should run.
///
/// Also a ratio rather than an absolute gap, for the same reason as above.
const MIN_SEPARATION: f32 = 1.15;

/// Below this many chunks there is no distribution to compare against, so the separation test
/// is skipped and the relative rule stands alone. A workspace with two notes in it should
/// still be searchable.
const MIN_CHUNKS_FOR_SEPARATION: usize = 4;

/// A sanity bound. Cosine similarity below this is not a weak match, it is an unrelated
/// document — no embedding model puts genuinely related text here.
const ABSOLUTE_FLOOR: f32 = 0.2;

/// A match this strong needs no corroboration from the rest of the distribution.
///
/// The counterpart to [`ABSOLUTE_FLOOR`], and safe for the opposite reason: no embedding model
/// scores unrelated text this high, so admitting on it alone risks little. It exists because a
/// relative test cannot handle a corpus with no spread — if every chunk scores identically,
/// nothing "stands out" and every relative rule rejects everything, including a perfect match.
const STRONG_MATCH: f32 = 0.75;

/// Material matching `query` by word alone.
///
/// What [`gather_hybrid`] falls back to whenever the semantic half cannot contribute. Returns
/// an empty list when nothing matches, which callers must handle explicitly — a model given no
/// material and asked a question will answer from its own weights, and that answer looks
/// exactly like a grounded one.
fn lexical_only(db: &Database, query: &str) -> ApiResult<Vec<Passage>> {
    let ranked = lexical_ranking(db, query)?;
    Ok(assemble(db, ranked, &BTreeMap::new()))
}

/// Lexical and semantic together, fused by rank.
///
/// Falls back to lexical-only whenever the semantic half cannot contribute — no embedder, no
/// vectors for this model, a stopped daemon. That is not an error path; it is the ordinary
/// state of a workspace that has never been indexed, and it must return results rather than
/// complain.
///
/// Takes the state rather than a `&Database` on purpose. Embedding the question is an
/// `.await`, and `Database` is `Send` but not `Sync` — a lock guard held across that await
/// makes the whole future non-`Send`, which axum rejects at the route. So the lock is taken
/// three times, briefly, and never spans the model call.
pub async fn gather_hybrid(
    state: &std::sync::Arc<crate::state::AppState>,
    query: &str,
) -> ApiResult<Vec<Passage>> {
    let embedder = state.embedder();

    // Is there anything to fuse? A count is cheap, and asking first avoids spending a model
    // call embedding the question for a workspace that has never been indexed.
    let (lexical, vector_count) = {
        let db = state.db().await;
        let count = EmbeddingRepository::new(&db)
            .count(embedder.model())
            .unwrap_or(0);
        (lexical_ranking(&db, query)?, count)
    };

    if vector_count == 0 {
        let db = state.db().await;
        return lexical_only(&db, query);
    }

    // No lock held here. `embed_query` rather than `embed_documents`: the two use different
    // task prefixes, and using the document form for a question is the standard way to get
    // quietly worse retrieval.
    let embedded = match embedder.embed_query(query).await {
        Ok(vector) => Some(vector),
        // A daemon that stopped between indexing and asking. Lexical still works.
        Err(error) => {
            tracing::debug!(%error, "semantic retrieval unavailable; falling back to lexical");
            None
        }
    };

    let db = state.db().await;
    let Some(question) = embedded else {
        return lexical_only(&db, query);
    };

    let stored = EmbeddingRepository::new(&db)
        .all_for_model(embedder.model())
        .unwrap_or_default();

    let (semantic, best_chunks) = rank_by_similarity(&question, &stored);
    Ok(assemble(&db, fuse(&[lexical, semantic]), &best_chunks))
}

/// Score every chunk against the question and reduce to a ranking of entities.
///
/// Also returns the chunks that matched, per entity: that text is better grounding than the
/// whole entity, and becomes the passage.
#[allow(clippy::type_complexity)]
fn rank_by_similarity(
    question: &[f32],
    stored: &[notewise_storage::Embedding],
) -> (
    Vec<(&'static str, Id)>,
    BTreeMap<(&'static str, Id), Vec<String>>,
) {
    let mut scored: Vec<(f32, &notewise_storage::Embedding)> = stored
        .iter()
        .map(|row| (cosine(question, &row.vector), row))
        .collect();
    // `total_cmp` rather than `partial_cmp().unwrap()`: a NaN would panic the second and sort
    // arbitrarily under the first, and `cosine` is written to never produce one anyway.
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));

    let Some(cutoff) = cutoff(&scored) else {
        return (Vec::new(), BTreeMap::new());
    };

    let mut ranking: Vec<(&'static str, Id)> = Vec::new();
    let mut best_chunks: BTreeMap<(&'static str, Id), Vec<String>> = BTreeMap::new();

    for (score, row) in scored.into_iter().take(VECTOR_LIMIT) {
        if score < cutoff {
            break;
        }
        let Some(kind) = static_kind(&row.entity_kind) else {
            continue;
        };

        let key = (kind, row.entity_id);
        push_unique(&mut ranking, key);
        let chunks = best_chunks.entry(key).or_default();
        // At most two chunks per entity: enough for a decision and its context, not so many
        // that one long meeting fills the budget.
        if chunks.len() < 2 {
            chunks.push(row.text.clone());
        }
    }

    (ranking, best_chunks)
}

/// The score a chunk must reach to count as a match, or `None` if nothing does.
///
/// Takes the scores already sorted descending. See [`RELATIVE_KEEP`] and [`MIN_SEPARATION`]
/// for why both rules exist and why neither is an absolute similarity threshold.
fn cutoff<T>(scored: &[(f32, T)]) -> Option<f32> {
    let best = scored.first()?.0;
    if best < ABSOLUTE_FLOOR {
        return None;
    }

    // A very strong match needs no corroboration. This is the one absolute number here, and
    // it is a ceiling rather than a floor — no embedding model scores unrelated text this
    // high, so being wrong about it costs far less than the floor it replaces. It also covers
    // the degenerate case the separation test cannot: a corpus where everything matches
    // perfectly has no spread to measure, and rejecting all of it would be absurd.
    if best >= STRONG_MATCH {
        return Some(best * RELATIVE_KEEP);
    }

    // Does the best score stand out from the background? With too few chunks there is no
    // background to compare against, and the question is unanswerable rather than answered no.
    if scored.len() >= MIN_CHUNKS_FOR_SEPARATION {
        // The background is the median of the *lower half*, not of everything. A plain median
        // lands inside the relevant cluster whenever several documents genuinely match, and
        // then rejects the very case retrieval exists for — measured: scores of
        // [0.61, 0.59, 0.55, 0.30, 0.29] have a median of 0.55, which suppresses all three
        // real matches. The lower half estimates what an *irrelevant* document scores.
        let tail = &scored[scored.len() / 2..];
        let background = tail[tail.len() / 2].0;

        // A background at or below zero means most of the corpus is orthogonal to the
        // question — the easy case, where anything positive stands out. Multiplying by it
        // would also invert the comparison.
        if background > 0.0 && best < background * MIN_SEPARATION {
            return None;
        }
    }

    Some(best * RELATIVE_KEEP)
}

/// Entities matching `query` by word, best first.
fn lexical_ranking(db: &Database, query: &str) -> ApiResult<Vec<(&'static str, Id)>> {
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

    Ok(ordered)
}

/// Reciprocal Rank Fusion over any number of rankings.
///
/// Ties are broken by the earliest ranking an entity appeared in, so the result is
/// deterministic — an unstable order here would make the same question return different
/// citations on consecutive asks.
pub fn fuse(rankings: &[Vec<(&'static str, Id)>]) -> Vec<(&'static str, Id)> {
    let mut scores: BTreeMap<(&'static str, Id), (f32, usize)> = BTreeMap::new();

    for ranking in rankings {
        for (rank, entity) in ranking.iter().enumerate() {
            let entry = scores.entry(*entity).or_insert((0.0, usize::MAX));
            entry.0 += 1.0 / (RRF_K + rank as f32);
            entry.1 = entry.1.min(rank);
        }
    }

    let mut fused: Vec<_> = scores.into_iter().collect();
    fused.sort_by(|a, b| {
        b.1 .0
            .total_cmp(&a.1 .0)
            .then_with(|| a.1 .1.cmp(&b.1 .1))
            .then_with(|| a.0.cmp(&b.0))
    });

    fused.into_iter().map(|(entity, _)| entity).collect()
}

/// Turn a ranked list of entities into passages inside the context budget.
///
/// `preferred` holds the specific chunks semantic search matched. Where they exist they are
/// used instead of the whole entity: two paragraphs that actually answer the question ground
/// an answer better than the first three thousand characters of an hour-long transcript.
fn assemble(
    db: &Database,
    ranked: Vec<(&'static str, Id)>,
    preferred: &BTreeMap<(&'static str, Id), Vec<String>>,
) -> Vec<Passage> {
    let mut passages = Vec::new();
    let mut budget = MAX_CONTEXT_CHARS;

    for (kind, id) in ranked {
        if passages.len() == MAX_PASSAGES || budget == 0 {
            break;
        }

        let Some(passage) = load(db, kind, id) else {
            continue;
        };

        let source = match preferred.get(&(kind, id)) {
            Some(chunks) if !chunks.is_empty() => chunks.join("\n\n…\n\n"),
            _ => passage.text,
        };

        let allowance = budget.min(MAX_PASSAGE_CHARS);
        let text = truncate(&source, allowance);
        if text.trim().is_empty() {
            continue;
        }

        budget = budget.saturating_sub(text.len());
        passages.push(Passage { text, ..passage });
    }

    passages
}

/// The `'static` name for a stored kind, or `None` for one this build does not render.
fn static_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "meeting" => Some("meeting"),
        "note" => Some("note"),
        "ticket" => Some("ticket"),
        _ => None,
    }
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
    use notewise_storage::Embedding;
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
        assert!(lexical_only(&db, "what is it?")
            .expect("retrieve")
            .is_empty());
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

        let passages =
            lexical_only(&db, "what did we decide about pricing tiers?").expect("retrieve");

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

        let passages = lexical_only(&db, "pricing").expect("retrieve");
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

        let kinds: Vec<_> = lexical_only(&db, "what is our latency budget?")
            .expect("retrieve")
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

        assert_eq!(
            lexical_only(&db, "when is the launch date?").unwrap().len(),
            1
        );

        notes.trash(note.id).expect("trash");
        assert!(lexical_only(&db, "when is the launch date?")
            .unwrap()
            .is_empty());
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

        let passages = lexical_only(&db, "pricing").expect("retrieve");
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
        let passages = lexical_only(&db, "pricing tiers").expect("retrieve");

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
        let passages = lexical_only(&db, "\"pricing\" OR NEAR(x) AND * pricing").expect("retrieve");
        assert_eq!(passages.len(), 1);
    }

    // ------------------------------------------------------------ fusion

    /// Distinct ids, generated once per test. `Id::new` is random, which is fine here — the
    /// assertions compare against the values captured, never against a literal.
    fn entity(kind: &'static str) -> (&'static str, Id) {
        (kind, Id::new())
    }

    #[test]
    fn fusing_one_ranking_preserves_its_order() {
        let a = entity("note");
        let b = entity("note");
        let c = entity("note");

        assert_eq!(fuse(&[vec![a, b, c]]), vec![a, b, c]);
    }

    /// The behaviour the whole design rests on: something both retrievers like beats something
    /// only one of them put first.
    #[test]
    fn agreement_between_retrievers_outranks_a_single_first_place() {
        let agreed = entity("meeting");
        let lexical_favourite = entity("meeting");
        let semantic_favourite = entity("meeting");

        // Each retriever ranks its own favourite first and the agreed one second.
        let lexical = vec![lexical_favourite, agreed];
        let semantic = vec![semantic_favourite, agreed];

        let fused = fuse(&[lexical, semantic]);
        assert_eq!(
            fused[0], agreed,
            "the entity both retrievers ranked highly should win: {fused:?}"
        );
    }

    #[test]
    fn an_entity_only_one_retriever_found_still_appears() {
        let shared = entity("note");
        let lexical_only_hit = entity("note");
        let semantic_only_hit = entity("meeting");

        let fused = fuse(&[
            vec![shared, lexical_only_hit],
            vec![shared, semantic_only_hit],
        ]);

        assert!(fused.contains(&lexical_only_hit), "{fused:?}");
        assert!(fused.contains(&semantic_only_hit), "{fused:?}");
    }

    #[test]
    fn fusing_nothing_yields_nothing() {
        assert!(fuse(&[]).is_empty());
        assert!(fuse(&[Vec::new(), Vec::new()]).is_empty());
    }

    /// An unstable order would make the same question return different citations on
    /// consecutive asks, which is indistinguishable from the model being inconsistent.
    #[test]
    fn fusion_is_deterministic_for_tied_scores() {
        let a = entity("note");
        let b = entity("note");

        // Both appear once, at the same rank, in different rankings — a perfect tie.
        let first = fuse(&[vec![a], vec![b]]);
        for _ in 0..20 {
            assert_eq!(fuse(&[vec![a], vec![b]]), first);
        }
    }

    #[test]
    fn a_duplicate_within_one_ranking_does_not_double_count() {
        let a = entity("note");
        let b = entity("note");

        // `push_unique` prevents this upstream; the fusion should not depend on that.
        let fused = fuse(&[vec![a, b]]);
        assert_eq!(fused.len(), 2);
    }

    // ------------------------------------------------------------ similarity ranking

    fn embedding(kind: &str, id: Id, index: i64, text: &str, vector: Vec<f32>) -> Embedding {
        Embedding {
            id: Id::new(),
            entity_kind: kind.to_string(),
            entity_id: id,
            chunk_index: index,
            text: text.to_string(),
            vector,
            model: "test".into(),
            source_updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn similarity_ranks_the_nearest_chunk_first() {
        let near = Id::new();
        let far = Id::new();
        let question = vec![1.0, 0.0];

        let stored = vec![
            embedding("note", far, 0, "unrelated", vec![0.0, 1.0]),
            embedding("note", near, 0, "on point", vec![0.99, 0.14]),
        ];

        let (ranking, chunks) = rank_by_similarity(&question, &stored);
        assert_eq!(ranking.first(), Some(&("note", near)), "{ranking:?}");
        assert_eq!(chunks[&("note", near)], vec!["on point".to_string()]);
    }

    /// Without a floor, every question retrieves *something* — and the refusal path that
    /// stops a confident answer with no basis never runs.
    #[test]
    fn distant_neighbours_are_not_treated_as_matches() {
        let id = Id::new();
        // Orthogonal: cosine 0, well under the floor.
        let stored = vec![embedding(
            "note",
            id,
            0,
            "nothing to do with it",
            vec![0.0, 1.0],
        )];

        let (ranking, _) = rank_by_similarity(&[1.0, 0.0], &stored);
        assert!(ranking.is_empty(), "{ranking:?}");
    }

    #[test]
    fn at_most_two_chunks_are_kept_per_entity() {
        let id = Id::new();
        let stored: Vec<_> = (0..5)
            .map(|n| embedding("meeting", id, n, &format!("chunk {n}"), vec![1.0, 0.0]))
            .collect();

        let (ranking, chunks) = rank_by_similarity(&[1.0, 0.0], &stored);
        assert_eq!(ranking, vec![("meeting", id)], "one entity, once");
        assert_eq!(
            chunks[&("meeting", id)].len(),
            2,
            "one long meeting must not fill the budget"
        );
    }

    #[test]
    fn an_unknown_stored_kind_is_skipped_rather_than_breaking_the_ranking() {
        let known = Id::new();
        let stored = vec![
            embedding("something_new", Id::new(), 0, "future kind", vec![1.0, 0.0]),
            embedding("note", known, 0, "known kind", vec![1.0, 0.0]),
        ];

        let (ranking, _) = rank_by_similarity(&[1.0, 0.0], &stored);
        assert_eq!(ranking, vec![("note", known)]);
    }

    #[test]
    fn similarity_over_nothing_ranks_nothing() {
        let (ranking, chunks) = rank_by_similarity(&[1.0, 0.0], &[]);
        assert!(ranking.is_empty());
        assert!(chunks.is_empty());
    }

    // ------------------------------------------------------------ the cutoff

    /// Scores as they came out of a real workspace, embedded with `nomic-embed-text`, for the
    /// question "what did we decide about pricing tiers?".
    ///
    /// These are the numbers that showed the original absolute threshold was wrong: it was set
    /// at 0.25, and the *irrelevant* entries here score 0.292 and 0.435.
    const MEASURED: &[f32] = &[
        0.607, // the meeting that answered it — and never used the word "pricing"
        0.435, // an unrelated meeting about a broken coffee machine
        0.292, // a note whose entire content is "typing here"
        0.292, // ditto
    ];

    fn scores(values: &[f32]) -> Vec<(f32, ())> {
        values.iter().map(|v| (*v, ())).collect()
    }

    #[test]
    fn the_cutoff_admits_only_the_relevant_match_on_measured_scores() {
        let cut = cutoff(&scores(MEASURED)).expect("something matched");

        assert!(cut > 0.435, "the coffee-machine meeting must not qualify");
        assert!(cut <= 0.607, "the meeting that answered it must qualify");

        let kept = MEASURED.iter().filter(|s| **s >= cut).count();
        assert_eq!(kept, 1, "cutoff {cut} kept {kept} of {MEASURED:?}");
    }

    /// Two genuinely relevant documents must both survive — the rule trims noise, not results.
    #[test]
    fn several_close_matches_are_all_kept() {
        let close = scores(&[0.61, 0.59, 0.55, 0.30, 0.29]);
        let cut = cutoff(&close).expect("something matched");

        let kept = close.iter().filter(|(s, _)| *s >= cut).count();
        assert_eq!(kept, 3, "cutoff {cut}");
    }

    /// When nothing stands out, nothing is a match — this is what makes the refusal path run
    /// rather than dressing up the least-distant document as an answer.
    #[test]
    fn a_flat_distribution_matches_nothing() {
        assert!(cutoff(&scores(&[0.31, 0.30, 0.30, 0.29, 0.29])).is_none());
    }

    #[test]
    fn a_uniformly_low_distribution_matches_nothing() {
        assert!(cutoff(&scores(&[0.11, 0.10, 0.09, 0.08])).is_none());
    }

    /// A small workspace has no distribution to compare against. It must still be searchable.
    #[test]
    fn a_tiny_corpus_skips_the_separation_test() {
        // Two chunks, close together: with the separation rule applied this would match
        // nothing, leaving a two-note workspace unsearchable.
        let cut = cutoff(&scores(&[0.52, 0.50])).expect("a small workspace is still searchable");
        assert!(cut <= 0.52);
    }

    #[test]
    fn an_empty_corpus_has_no_cutoff() {
        assert!(cutoff::<()>(&[]).is_none());
    }

    /// A corpus with no spread has nothing to measure standing-out against. Every relative
    /// rule rejects all of it, including a perfect match, so a strong score is exempt.
    #[test]
    fn a_uniformly_perfect_corpus_still_matches() {
        let cut = cutoff(&scores(&[1.0, 1.0, 1.0, 1.0, 1.0])).expect("a perfect match matches");
        assert!(cut <= 1.0);
    }

    /// The exemption is a ceiling, not a licence: a merely-decent score still has to stand out.
    #[test]
    fn a_middling_score_is_not_exempt_from_the_separation_test() {
        assert!(cutoff(&scores(&[0.60, 0.59, 0.58, 0.57, 0.56])).is_none());
    }

    #[test]
    fn a_negative_or_zero_median_does_not_break_the_separation_test() {
        // Most of the corpus orthogonal or opposed to the question: the easy case, and a
        // multiplication against a non-positive median would invert the comparison.
        let cut = cutoff(&scores(&[0.62, 0.10, -0.05, -0.20, -0.30])).expect("should match");
        assert!(cut > 0.10);
    }
}
