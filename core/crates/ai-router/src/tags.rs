//! Matching a model name against what a daemon actually holds.
//!
//! # The bug this exists to prevent
//!
//! Ollama expands an untagged name to `:latest`. `llama3.1` is therefore not "any llama3.1" —
//! it means exactly `llama3.1:latest`, and on a machine holding only `llama3.1:8b` it is a
//! 404. Notewise shipped `llama3.1` and `nomic-embed-text` as its defaults, so the first
//! summary on a fresh install failed for anyone whose `ollama pull` had named a size, which is
//! what the documentation tells people to do.
//!
//! The daemon can be asked. It was already being asked — to populate the model picker, and to
//! name the installed models in the error message — just never at the moment the default was
//! chosen. So the default asserted something the code had the means to check.
//!
//! # Why this is pure
//!
//! Same reason `is_model_missing` is: this is the part that can be wrong. Deciding which tag
//! to send is a rule about strings, and testing it should not require a daemon holding a
//! specific set of models.
//!
//! # What it deliberately will not do
//!
//! Substitute a *different family* for a name the user chose. Asking for `mistral:7b` and
//! quietly getting llama is worse than a clear 404: the output is attributed to the model that
//! produced it, and the user's choice would be silently overridden. Family substitution is
//! offered separately, through [`first_acceptable`], for the one case where nobody chose
//! anything — our own default.

/// Whether `tag` names `family`, with or without a version suffix.
///
/// `llama3.1` names `llama3.1:8b`; it does not name `llama3.1-instruct`, which is a different
/// model rather than a tag of this one. The separator is what makes that distinction, so it is
/// matched explicitly rather than with a bare `starts_with`.
pub(crate) fn names_family(family: &str, tag: &str) -> bool {
    tag == family
        || tag
            .strip_prefix(family)
            .is_some_and(|rest| rest.starts_with(':'))
}

/// The tag to actually send for `preferred`, given what is installed.
///
/// `None` means the daemon holds nothing from that family — the caller decides whether that is
/// an error or an invitation to fall back, because the answer differs depending on whether a
/// human chose the name.
pub(crate) fn resolve_tag(preferred: &str, installed: &[String]) -> Option<String> {
    // An exact hit needs no interpretation, and covers every fully-tagged name.
    if installed.iter().any(|tag| tag == preferred) {
        return Some(preferred.to_string());
    }

    // What the daemon would have resolved an untagged name to anyway. Preferred over any other
    // tag in the family so that behaviour on a machine that *does* have `:latest` is unchanged
    // by this function existing.
    let latest = format!("{preferred}:latest");
    if installed.contains(&latest) {
        return Some(latest);
    }

    // Any other tag of the same family. Sorted, because which model answers must not depend on
    // the order the daemon happened to list them in — Ollama returns them by modification
    // time, which changes every time a model is used.
    let mut family: Vec<&str> = installed
        .iter()
        .map(String::as_str)
        .filter(|tag| names_family(preferred, tag))
        .collect();
    family.sort_unstable();
    family.first().map(|tag| (*tag).to_string())
}

/// Any installed model that passes `acceptable`, deterministically.
///
/// The last resort for a default nobody chose: a machine with `mistral` and no llama should
/// summarize a meeting rather than report that our preference is missing.
pub(crate) fn first_acceptable(
    installed: &[String],
    acceptable: impl Fn(&str) -> bool,
) -> Option<String> {
    let mut usable: Vec<&str> = installed
        .iter()
        .map(String::as_str)
        .filter(|tag| acceptable(tag))
        .collect();
    usable.sort_unstable();
    usable.first().map(|tag| (*tag).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(tags: &[&str]) -> Vec<String> {
        tags.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn an_untagged_default_finds_the_tag_that_is_actually_installed() {
        // The shipped default, on a machine that pulled a size. Reproduced against a real
        // daemon: `llama3.1` returns 404 `model 'llama3.1' not found` while `llama3.1:8b`
        // sits right there.
        let have = installed(&["llama3.1:8b", "nomic-embed-text:latest"]);
        assert_eq!(
            resolve_tag("llama3.1", &have).as_deref(),
            Some("llama3.1:8b")
        );
    }

    #[test]
    fn latest_is_preferred_over_other_tags_in_the_family() {
        // Machines that have it must behave exactly as they did before this function existed.
        let have = installed(&["llama3.1:70b", "llama3.1:latest", "llama3.1:8b"]);
        assert_eq!(
            resolve_tag("llama3.1", &have).as_deref(),
            Some("llama3.1:latest")
        );
    }

    #[test]
    fn an_exact_tag_is_returned_untouched() {
        let have = installed(&["llama3.1:8b", "llama3.1:latest"]);
        assert_eq!(
            resolve_tag("llama3.1:8b", &have).as_deref(),
            Some("llama3.1:8b")
        );
    }

    #[test]
    fn a_chosen_tag_that_is_missing_does_not_become_a_different_size() {
        // `llama3.1:70b` and `llama3.1:8b` are not interchangeable. Someone who named a size
        // gets told it is missing, not handed a smaller model that answers differently.
        let have = installed(&["llama3.1:8b"]);
        assert_eq!(resolve_tag("llama3.1:70b", &have), None);
    }

    #[test]
    fn a_family_prefix_is_not_a_substring_match() {
        // `llama3` must not match `llama3.1:8b`: the tag separator is what distinguishes a
        // version of this model from a different model whose name starts the same way.
        let have = installed(&["llama3.1:8b", "llama3-instruct:latest"]);
        assert_eq!(resolve_tag("llama3", &have), None);
        assert!(names_family("llama3.1", "llama3.1:8b"));
        assert!(!names_family("llama3", "llama3.1:8b"));
        assert!(!names_family("llama3", "llama3-instruct:latest"));
    }

    #[test]
    fn nothing_installed_resolves_to_nothing() {
        assert_eq!(resolve_tag("llama3.1", &[]), None);
    }

    #[test]
    fn resolution_does_not_depend_on_the_order_the_daemon_listed_models() {
        // Ollama orders by modification time, so the same machine returns a different order
        // after every use. Which model answers must not follow that.
        let one = installed(&["llama3.1:70b", "llama3.1:8b"]);
        let two = installed(&["llama3.1:8b", "llama3.1:70b"]);
        assert_eq!(resolve_tag("llama3.1", &one), resolve_tag("llama3.1", &two));
    }

    #[test]
    fn a_fallback_skips_models_that_cannot_do_the_job() {
        // Picking an embedder to hold a conversation would turn a missing-model error into a
        // baffling one about output shape.
        let have = installed(&["bge-m3:latest", "mistral:7b", "nomic-embed-text:latest"]);
        let chosen = first_acceptable(&have, |m| !crate::embed::is_embedding_model(m));
        assert_eq!(chosen.as_deref(), Some("mistral:7b"));
    }

    #[test]
    fn a_fallback_with_nothing_usable_gives_up_rather_than_guessing() {
        let have = installed(&["bge-m3:latest", "nomic-embed-text:latest"]);
        assert_eq!(
            first_acceptable(&have, |m| !crate::embed::is_embedding_model(m)),
            None
        );
    }
}
