//! What Notewise says about itself, for questions about the app rather than what is in it.
//!
//! Ask answers from material, and the only material used to be what the user recorded. "How do I
//! change models?" and "how many recordings do I have?" were both refused, correctly, because no
//! transcript says either. The answers are in the app: on its Settings pages, in Help and About,
//! and in the counts behind Library, My Tasks and Trash. This module makes them material.
//!
//! # Where it lives
//!
//! Markdown under `guide/`, compiled into the engine, so the text ships with the build it
//! describes and every surface that calls `/v1/ask` gets it. Each `## ` heading is one section:
//!
//! ```markdown
//! ## Settings: Models
//! page: settings/models
//! keywords: model, llm, provider, ollama, api key
//! live: models
//!
//! Which AI model writes summaries and answers questions, and where it runs.
//! ```
//!
//! `page` is the desktop route the section describes, and what its citation opens. `keywords` are
//! the words a user would use that the body might not. `live` names values computed when the
//! question is asked ([`crate::facts`]), so a section can say what a setting is set to now and
//! not only what it does.
//!
//! # Why it is not in the index
//!
//! Nothing here is stored, indexed, embedded, exported or synced. In the index it would become the
//! user's material: it would turn up in search, in the agent's reading, and on a second machine
//! through sync, all of which are about what the user recorded.
//!
//! # Why word matching
//!
//! The guide is small and hand-written. A question that misses the section it should find is
//! fixed by adding a keyword, which is deterministic and testable; an embedding that misses is
//! not. It also costs no model call and works with Ollama stopped.

use std::sync::{Arc, OnceLock};

use crate::retrieval::{self, Passage, Target};
use crate::state::AppState;

/// The guide, compiled in. Earlier files win a tie, so the live workspace numbers come first.
const FILES: &[(&str, &str)] = &[
    ("workspace.md", include_str!("../guide/workspace.md")),
    ("settings.md", include_str!("../guide/settings.md")),
    ("pages.md", include_str!("../guide/pages.md")),
    ("help.md", include_str!("../guide/help.md")),
    ("about.md", include_str!("../guide/about.md")),
];

/// How many sections may join one question's material.
///
/// Three, because a question about the app is usually about one place in it, and every section
/// taken here is a passage of the user's own material that cannot be.
pub(crate) const MAX_SECTIONS: usize = 3;

/// Longest a section may be once its live values are in.
pub(crate) const MAX_SECTION_CHARS: usize = 1_500;

/// Words too ordinary to say what a question is about.
///
/// Two kinds. Words that ask about the app without naming a part of it: "change" is in the
/// keywords of half the settings, so without this "what did we decide to change about pricing?"
/// found the Models page. And the names of what people record: "what did Dana say in the meeting
/// about pricing?" is about pricing, not about the Library.
///
/// They still add to a section's score, and when a question is made of nothing else ("where are
/// my notes?", "what is Notewise?") they count in full: the question is then about where those
/// things live.
const GENERIC: &[&str] = &[
    "app",
    "button",
    "call",
    "change",
    "find",
    "list",
    "make",
    "meeting",
    "note",
    "notewise",
    "open",
    "option",
    "page",
    "recording",
    "screen",
    "see",
    "set",
    "setting",
    "show",
    "summarise",
    "summarize",
    "summary",
    "switch",
    "task",
    "transcript",
    "turn",
    "use",
    "using",
];

/// A value the engine fills in when a section is used. See [`crate::facts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Live {
    Models,
    Appearance,
    Transcription,
    Recording,
    Retention,
    Detection,
    Search,
    Speakers,
    About,
    Numbers,
    Recent,
    Tasks,
    Trash,
}

impl Live {
    pub const ALL: [Live; 13] = [
        Live::Models,
        Live::Appearance,
        Live::Transcription,
        Live::Recording,
        Live::Retention,
        Live::Detection,
        Live::Search,
        Live::Speakers,
        Live::About,
        Live::Numbers,
        Live::Recent,
        Live::Tasks,
        Live::Trash,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Live::Models => "models",
            Live::Appearance => "appearance",
            Live::Transcription => "transcription",
            Live::Recording => "recording",
            Live::Retention => "retention",
            Live::Detection => "detection",
            Live::Search => "search",
            Live::Speakers => "speakers",
            Live::About => "about",
            Live::Numbers => "numbers",
            Live::Recent => "recent",
            Live::Tasks => "tasks",
            Live::Trash => "trash",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|live| live.as_str() == raw)
    }
}

/// One `## ` section of the guide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub title: String,
    pub page: String,
    pub live: Vec<Live>,
    pub body: String,
    /// What the scorer compares against, lowercased and split once rather than per question.
    title_words: Vec<String>,
    keyword_words: Vec<String>,
    body_words: Vec<String>,
}

/// Every section of the guide, parsed once.
///
/// A file that does not parse is left out rather than taking the engine down with it; the tests
/// parse every file, so that only happens to a build nobody tested.
pub fn sections() -> &'static [Section] {
    static SECTIONS: OnceLock<Vec<Section>> = OnceLock::new();
    SECTIONS.get_or_init(|| {
        FILES
            .iter()
            .flat_map(|(name, text)| {
                parse(text).unwrap_or_else(|error| {
                    tracing::warn!(file = name, %error, "guide file does not parse; left out");
                    Vec::new()
                })
            })
            .collect()
    })
}

/// Read one guide file.
pub fn parse(text: &str) -> Result<Vec<Section>, String> {
    let mut sections = Vec::new();
    // A Windows checkout turns these files into CRLF, and the blank line that ends a section's
    // metadata is then "\r\n\r\n". The leading newline lets a heading on the first line split
    // like any other; anything before the first heading is a note for whoever edits the file.
    let text = format!("\n{}", text.replace("\r\n", "\n"));

    for chunk in text.split("\n## ").skip(1) {
        let (title, rest) = chunk.split_once('\n').unwrap_or((chunk, ""));
        let title = title.trim().to_string();
        let (meta, body) = rest.split_once("\n\n").unwrap_or((rest, ""));

        let (mut page, mut keywords, mut live) = (None, None, Vec::new());
        for line in meta.lines().filter(|line| !line.trim().is_empty()) {
            let Some((key, value)) = line.split_once(':') else {
                return Err(format!(
                    "\"{title}\": \"{line}\" is not a `key: value` line"
                ));
            };
            let value = value.trim();
            match key.trim() {
                "page" => page = Some(value.to_string()),
                "keywords" => keywords = Some(value.to_string()),
                "live" => {
                    for name in value.split(',').map(str::trim).filter(|n| !n.is_empty()) {
                        live.push(
                            Live::parse(name)
                                .ok_or_else(|| format!("\"{title}\": no live value \"{name}\""))?,
                        );
                    }
                }
                other => return Err(format!("\"{title}\": unknown key \"{other}\"")),
            }
        }

        let page = page.ok_or_else(|| format!("\"{title}\" has no page"))?;
        let keywords = keywords.ok_or_else(|| format!("\"{title}\" has no keywords"))?;
        let body = body.trim().to_string();
        if body.lines().any(|line| line.starts_with('#')) {
            return Err(format!("\"{title}\" has a heading inside its body"));
        }
        if body.is_empty() && live.is_empty() {
            return Err(format!("\"{title}\" says nothing"));
        }

        // "Settings: Models" is about models. The part before the colon names a whole area, and
        // scoring it would make every settings section a title match for "settings".
        let topic = title
            .rsplit_once(": ")
            .map_or(title.as_str(), |(_, topic)| topic);
        sections.push(Section {
            title_words: words(topic),
            keyword_words: words(&keywords),
            body_words: words(&body),
            title,
            page,
            live,
            body,
        });
    }

    Ok(sections)
}

fn words(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        let word = word.to_lowercase();
        if word.chars().count() >= 2 && !out.contains(&word) {
            out.push(word);
        }
    }
    out
}

/// Whether a question's term names a word: the same once common endings are taken off.
///
/// "Lighter" finds "light", "recordings" finds "record", "deleting" finds "delete". Not plain
/// prefix matching, which had "mode" find "model" and sent every dark-mode question to the Models
/// page.
fn same_word(term: &str, word: &str) -> bool {
    stem(term) == stem(word)
}

/// A word without its ending, when what is left is still four letters or more.
///
/// The floor keeps short words whole: "notes" becomes "note" rather than "not", and "mode" stays
/// "mode" rather than meeting "model" halfway.
fn stem(word: &str) -> &str {
    for ending in ["ings", "ing", "ies", "ers", "es", "ed", "er", "s", "e", "y"] {
        if let Some(root) = word.strip_suffix(ending) {
            if root.chars().count() >= 4 {
                return root;
            }
        }
    }
    word
}

/// How well a section answers a question's terms, and whether it is a candidate at all.
///
/// A term is worth 3 in the title, 2 in the keywords and 1 in the body, at its best field. Only a
/// title or keyword match makes a section a candidate: a guide page that happens to use two of
/// the question's words in passing is not about the question.
fn score(section: &Section, terms: &[String]) -> (u32, bool) {
    let ordinary = |term: &str| GENERIC.iter().any(|word| same_word(term, word));
    let all_ordinary = terms.iter().all(|term| ordinary(term));

    let mut total = 0;
    let mut candidate = false;
    for term in terms {
        let hit = |words: &[String]| words.iter().any(|word| same_word(term, word));
        let points = if hit(&section.title_words) {
            3
        } else if hit(&section.keyword_words) {
            2
        } else if hit(&section.body_words) {
            1
        } else {
            0
        };
        if ordinary(term) && !all_ordinary {
            total += points.min(1);
        } else {
            total += points;
            candidate |= points >= 2;
        }
    }
    (total, candidate)
}

/// The sections a question is about, best first.
///
/// Sections well behind the best are left out: "how do I change the theme in settings?" is about
/// Appearance, and every other settings page that lists "settings" as a keyword is not.
pub fn find<'a>(all: &'a [Section], question: &str) -> Vec<&'a Section> {
    let terms = retrieval::terms(question);
    let mut scored: Vec<(u32, usize)> = all
        .iter()
        .enumerate()
        .filter_map(|(index, section)| {
            let (points, candidate) = score(section, &terms);
            candidate.then_some((points, index))
        })
        .collect();

    let best = scored.iter().map(|(points, _)| *points).max().unwrap_or(0);
    scored.retain(|(points, _)| points * 3 >= best * 2);
    // Stable, so a tie goes to the earlier section.
    scored.sort_by_key(|(points, _)| std::cmp::Reverse(*points));
    scored.truncate(MAX_SECTIONS);
    scored.into_iter().map(|(_, index)| &all[index]).collect()
}

/// The guide's answer to a question, as passages that cite the page they describe.
///
/// Live values are computed only for the sections a question found. A value nobody asked about is
/// never worked out, so an Ollama that is slow to answer costs nothing to a question about the
/// trash.
pub async fn passages(state: &Arc<AppState>, question: &str) -> Vec<Passage> {
    let mut found = Vec::new();
    for section in find(sections(), question) {
        let mut text = section.body.clone();
        for live in &section.live {
            if let Some(value) = crate::facts::render(state, *live).await {
                text.push_str("\n\n");
                text.push_str(&value);
            }
        }
        found.push(Passage {
            kind: "app",
            target: Target::Page(section.page.as_str()),
            title: section.title.clone(),
            text: retrieval::truncate(&text, MAX_SECTION_CHARS),
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
A note for whoever edits this file.

## Settings: Appearance
page: settings/appearance
keywords: theme, dark mode, light mode, colour, color, accent
live: appearance

Dark, Light or System, and an accent colour.

## Settings: Models
page: settings/models
keywords: model, llm, provider, ollama, change, settings

Which AI model writes summaries and answers questions.

## Trash
page: trash
keywords: deleted, bin, restore, settings

Deleted meetings and notes wait here until you empty it.
";

    fn sample() -> Vec<Section> {
        parse(SAMPLE).expect("sample parses")
    }

    fn titles(found: Vec<&Section>) -> Vec<&str> {
        found.into_iter().map(|s| s.title.as_str()).collect()
    }

    #[test]
    fn a_file_reads_as_its_sections() {
        let sections = sample();
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].title, "Settings: Appearance");
        assert_eq!(sections[0].page, "settings/appearance");
        assert_eq!(sections[0].live, vec![Live::Appearance]);
        assert_eq!(
            sections[0].body,
            "Dark, Light or System, and an accent colour."
        );
        assert!(sections[1].live.is_empty());
    }

    #[test]
    fn a_crlf_checkout_reads_the_same() {
        assert_eq!(parse(&SAMPLE.replace('\n', "\r\n")).unwrap(), sample());
    }

    #[test]
    fn a_section_without_a_page_or_keywords_or_with_an_unknown_live_value_is_refused() {
        assert!(parse("## A\nkeywords: a\n\nBody.").is_err());
        assert!(parse("## A\npage: trash\n\nBody.").is_err());
        assert!(parse("## A\npage: trash\nkeywords: a\nlive: weather\n\nBody.").is_err());
        assert!(parse("## A\npage: trash\nkeywords: a\n\n### Inner\nBody.").is_err());
    }

    #[test]
    fn a_question_finds_the_section_it_names() {
        let sections = sample();
        assert_eq!(
            titles(find(&sections, "how to change models?")),
            vec!["Settings: Models"]
        );
        assert_eq!(
            titles(find(&sections, "how do I get lighter colours")),
            vec!["Settings: Appearance"],
            "light finds lighter, colour finds colours"
        );
        assert_eq!(
            titles(find(&sections, "where do deleted things go?")),
            vec!["Trash"]
        );
    }

    /// "Change" is a keyword of the Models section, and is in plenty of questions about meetings.
    #[test]
    fn a_question_about_meetings_finds_nothing() {
        let sections = sample();
        assert!(find(&sections, "what did we decide about pricing?").is_empty());
        assert!(find(
            &sections,
            "what did we decide to change in the launch plan?"
        )
        .is_empty());
    }

    #[test]
    fn sections_well_behind_the_best_are_left_out() {
        let sections = sample();
        assert_eq!(
            titles(find(&sections, "how do I change the theme in settings?")),
            vec!["Settings: Appearance"],
            "Models and Trash only share the word settings"
        );
    }

    #[test]
    fn words_match_without_their_endings() {
        assert!(same_word("light", "lighter"));
        assert!(same_word("recordings", "recording"));
        assert!(same_word("deleting", "deleted"));
        assert!(same_word("task", "tasks"));
        assert!(same_word("notes", "note"));
        assert!(same_word("summaries", "summary"));
        assert!(!same_word("mode", "model"), "dark mode is not about models");
        assert!(!same_word("on", "only"));
        assert!(!same_word("app", "approve"));
    }

    // ------------------------------------------------------------ the guide as shipped

    #[test]
    fn every_guide_file_parses() {
        for (name, text) in FILES {
            let sections = parse(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!sections.is_empty(), "{name} has no sections");
        }
        assert_eq!(
            sections().len(),
            FILES
                .iter()
                .map(|(_, t)| parse(t).unwrap().len())
                .sum::<usize>()
        );
    }

    #[test]
    fn every_section_points_at_a_page_the_desktop_has() {
        const SETTINGS: &[&str] = &[
            "appearance",
            "recording",
            "transcription",
            "models",
            "summaries",
            "assistant",
            "tools",
            "search",
            "speakers",
        ];
        const PAGES: &[&str] = &[
            "home",
            "record",
            "library",
            "notes",
            "tasks",
            "ask",
            "agent",
            "connectors",
            "trash",
            "help/docs",
            "help/shortcuts",
            "help/whats-new",
            "help/support",
            "about",
        ];
        for section in sections() {
            let page = section.page.as_str();
            let known = PAGES.contains(&page)
                || page
                    .strip_prefix("settings/")
                    .is_some_and(|id| SETTINGS.contains(&id));
            assert!(known, "\"{}\" points at {page}", section.title);
        }
        for id in SETTINGS {
            assert!(
                sections()
                    .iter()
                    .any(|s| s.page == format!("settings/{id}")),
                "no section describes settings/{id}"
            );
        }
    }

    /// Every value the engine can fill in is used somewhere, or it is dead code that looks alive.
    #[test]
    fn every_live_value_has_a_section() {
        for live in Live::ALL {
            assert!(
                sections().iter().any(|s| s.live.contains(&live)),
                "no section shows {}",
                live.as_str()
            );
        }
    }

    #[test]
    fn the_guide_finds_the_questions_that_started_this() {
        let found = |q: &str| titles(find(sections(), q));
        assert!(
            found("how to change models?")
                .first()
                .is_some_and(|t| t.contains("Models")),
            "{:?}",
            found("how to change models?")
        );
        assert_eq!(
            found("how many recordings do I have?").first().copied(),
            Some("Your workspace in numbers")
        );
        assert_eq!(
            found("what's in the trash?").first().copied(),
            Some("In the trash")
        );
        assert_eq!(
            found("what are my open tasks?").first().copied(),
            Some("Open tasks")
        );
        assert!(found("what did we decide about pricing?").is_empty());
    }

    #[test]
    fn the_guide_finds_the_page_a_question_is_about() {
        let first = |q: &str| find(sections(), q).first().map(|s| s.title.clone());
        let cases = [
            ("how do I make it light mode?", "Settings: Appearance"),
            (
                "is my microphone allowed?",
                "Settings: Recording permissions",
            ),
            ("what version is this?", "About Notewise"),
            ("what is Notewise?", "About Notewise"),
            ("where are my notes?", "Notes"),
            ("how do I delete a meeting for good?", "Trash"),
            ("how do I summarise a meeting?", "Meeting pages"),
            (
                "what are the keyboard shortcuts?",
                "Help: Keyboard shortcuts",
            ),
            ("how do I turn on search by meaning?", "Settings: Search"),
            ("where do I put my api key?", "Settings: API keys"),
        ];
        for (question, expected) in cases {
            assert_eq!(first(question).as_deref(), Some(expected), "{question}");
        }
        assert!(
            find(sections(), "how do I switch to dark mode?")
                .iter()
                .all(|s| !s.title.contains("Model")),
            "mode is not model"
        );
    }

    /// Questions about what was said name meetings, notes and tasks all the time. Those words alone
    /// must not pull in the app.
    #[test]
    fn questions_about_what_was_said_find_nothing_in_the_guide() {
        for question in [
            "what did Dana say in the meeting about pricing?",
            "what tasks came out of the planning meeting?",
            "summarise the notes from the launch review",
            "who has raised concerns about the timeline?",
        ] {
            let found = titles(find(sections(), question));
            assert!(found.is_empty(), "{question}: {found:?}");
        }
    }

    #[test]
    fn no_section_is_longer_than_its_budget_before_live_values() {
        for section in sections() {
            assert!(
                section.body.len() <= MAX_SECTION_CHARS,
                "\"{}\" is {} characters",
                section.title,
                section.body.len()
            );
        }
    }
}
