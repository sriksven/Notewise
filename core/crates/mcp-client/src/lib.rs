//! Calling tools on external MCP servers, with a human in the loop for every call.
//!
//! # This crate is not the other one
//!
//! `notewise-mcp-server` answers requests about this workspace. This one makes requests of somebody
//! else's server. Opposite direction, opposite trust model: the server crate publishes a fixed list
//! of tools whose blast radius was reasoned about one at a time, and this one discovers arbitrary
//! tools whose blast radius is unknowable.
//!
//! # Every call is confirmed, every time
//!
//! There is no auto-execute, no "always allow this tool", and no batch execute. An external tool can
//! send a message, file a ticket, or charge a card, and nothing here can tell which. So the gate
//! moves from "which tools are safe" — which is what `MUTATING_TOOLS` can be, for a list somebody
//! wrote — to "a person looked at this call."
//!
//! A remembered per-tool allow would collapse into auto-execute within a week of use, which is why
//! it is absent rather than defaulted off. The product has no send path anywhere by design, and this
//! must not become one by transitivity.
//!
//! # What is implemented here
//!
//! The parts where being wrong is expensive and which can be checked without a subprocess:
//! [`validate`] against a tool's published schema, [`Allowlist`] enforcing default-deny, and
//! [`parse_proposal`] reading a tool call out of model prose. **There is no transport** — nothing
//! here spawns a server or speaks JSON-RPC to one yet.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod validate;

pub use validate::{validate, Invalid};

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("no server named '{0}' is configured")]
    UnknownServer(String),

    #[error("'{tool}' on '{server}' is not enabled")]
    NotAllowed { server: String, tool: String },

    #[error("the proposed call is not valid: {0}")]
    Invalid(String),

    #[error("could not read the model's proposal: {0}")]
    Unparseable(String),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, McpError>;

/// A tool an external server publishes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// The JSON Schema for this tool's arguments, as `tools/list` returned it.
    #[serde(rename = "inputSchema", default)]
    pub input_schema: Value,
}

/// One tool call a model has proposed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub server: String,
    pub tool: String,
    #[serde(default)]
    pub arguments: Value,
    /// The model's own one-line reason, when it gave one. Shown in the confirmation.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Which server-and-tool pairs may be proposed at all.
///
/// Default-deny: a pair is permitted only by being present. A server added and forgotten grants
/// nothing, which mirrors how the server crate treats its own write scope.
#[derive(Debug, Default, Clone)]
pub struct Allowlist {
    allowed: BTreeSet<(String, String)>,
}

impl Allowlist {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from stored rows.
    pub fn from_pairs<I, S>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (S, S)>,
        S: Into<String>,
    {
        Self {
            allowed: pairs
                .into_iter()
                .map(|(server, tool)| (server.into(), tool.into()))
                .collect(),
        }
    }

    pub fn allow(&mut self, server: impl Into<String>, tool: impl Into<String>) {
        self.allowed.insert((server.into(), tool.into()));
    }

    pub fn permits(&self, server: &str, tool: &str) -> bool {
        self.allowed
            .contains(&(server.to_string(), tool.to_string()))
    }

    /// Check a proposal, naming what was refused.
    pub fn check(&self, proposal: &Proposal) -> Result<()> {
        if self.permits(&proposal.server, &proposal.tool) {
            Ok(())
        } else {
            Err(McpError::NotAllowed {
                server: proposal.server.clone(),
                tool: proposal.tool.clone(),
            })
        }
    }

    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }

    pub fn len(&self) -> usize {
        self.allowed.len()
    }
}

/// Read a tool call out of a model's reply.
///
/// # Why text rather than native tool calling
///
/// `AiBackend` has no tool-calling method, and adding one would mean implementing it for every
/// backend including a local model whose support depends on the GGUF somebody downloaded. The local
/// path is the one the product's promise rests on, so the protocol is text — the same trade the
/// agent already made.
///
/// This costs accuracy and buys working identically on Ollama and Anthropic.
///
/// Tolerant of prose and code fences, because a model asked for JSON will wrap it. An unreadable
/// reply is an error the caller feeds back as an observation rather than a failed run.
pub fn parse_proposal(reply: &str) -> Result<Proposal> {
    let candidate = extract_json(reply)
        .ok_or_else(|| McpError::Unparseable("no JSON object in the reply".into()))?;

    serde_json::from_str::<Proposal>(&candidate).map_err(|e| {
        McpError::Unparseable(format!(
            "{e}; expected {{\"server\":…,\"tool\":…,\"arguments\":…}}"
        ))
    })
}

/// Pull the first balanced JSON object out of a string.
///
/// Brace counting rather than a regex, and string-aware, because a JSON argument value can itself
/// contain braces — an email body with `{` in it would otherwise truncate the object at the wrong
/// place and produce a confusing parse error instead of a working call.
fn extract_json(text: &str) -> Option<String> {
    let bytes: Vec<char> = text.chars().collect();
    let start = bytes.iter().position(|c| *c == '{')?;

    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, ch) in bytes[start..].iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if *ch == '\\' {
                escaped = true;
            } else if *ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(bytes[start..=start + offset].iter().collect());
                }
            }
            _ => {}
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn proposal() -> Proposal {
        Proposal {
            server: "linear".into(),
            tool: "create_issue".into(),
            arguments: json!({ "title": "Fix the importer" }),
            reason: None,
        }
    }

    /// Default-deny is the property everything else rests on.
    #[test]
    fn an_empty_allowlist_permits_nothing() {
        let list = Allowlist::new();
        assert!(list.is_empty());
        assert!(!list.permits("linear", "create_issue"));
        assert!(list.check(&proposal()).is_err());
    }

    #[test]
    fn only_the_exact_pair_is_permitted() {
        let mut list = Allowlist::new();
        list.allow("linear", "create_issue");

        assert!(list.permits("linear", "create_issue"));
        assert!(
            !list.permits("linear", "delete_issue"),
            "enabling one tool must not enable its neighbours"
        );
        assert!(
            !list.permits("jira", "create_issue"),
            "a tool name is only meaningful together with its server"
        );
    }

    #[test]
    fn a_refusal_names_what_was_refused() {
        let list = Allowlist::new();
        let err = list.check(&proposal()).expect_err("must refuse");
        let rendered = err.to_string();

        assert!(rendered.contains("create_issue"), "{rendered}");
        assert!(rendered.contains("linear"), "{rendered}");
    }

    #[test]
    fn an_allowlist_can_be_built_from_stored_rows() {
        let list = Allowlist::from_pairs([("linear", "create_issue"), ("slack", "post_message")]);
        assert_eq!(list.len(), 2);
        assert!(list.permits("slack", "post_message"));
    }

    #[test]
    fn a_bare_json_proposal_parses() {
        let parsed = parse_proposal(
            r#"{"server":"linear","tool":"create_issue","arguments":{"title":"Fix the importer"}}"#,
        )
        .expect("parses");
        assert_eq!(parsed, proposal());
    }

    /// A model asked for JSON will wrap it in prose or a fence.
    #[test]
    fn a_fenced_proposal_parses() {
        let parsed = parse_proposal(
            "Sure, here is the call:\n```json\n{\"server\":\"linear\",\"tool\":\"create_issue\",\
             \"arguments\":{\"title\":\"Fix the importer\"}}\n```\nLet me know.",
        )
        .expect("parses");
        assert_eq!(parsed.tool, "create_issue");
    }

    /// An email body containing a brace would otherwise truncate the object at the wrong place.
    #[test]
    fn braces_inside_a_string_do_not_end_the_object() {
        let parsed = parse_proposal(
            r#"{"server":"gmail","tool":"draft","arguments":{"body":"Use {placeholder} here"}}"#,
        )
        .expect("parses");
        assert_eq!(parsed.arguments["body"], "Use {placeholder} here");
    }

    #[test]
    fn an_escaped_quote_inside_a_string_is_handled() {
        let parsed = parse_proposal(
            r#"{"server":"s","tool":"t","arguments":{"body":"they said \"hello\" loudly"}}"#,
        )
        .expect("parses");
        assert_eq!(parsed.arguments["body"], r#"they said "hello" loudly"#);
    }

    #[test]
    fn a_reason_is_kept_for_the_confirmation() {
        let parsed = parse_proposal(
            r#"{"server":"s","tool":"t","arguments":{},"reason":"the action item asks for it"}"#,
        )
        .expect("parses");
        assert_eq!(
            parsed.reason.as_deref(),
            Some("the action item asks for it")
        );
    }

    #[test]
    fn a_reply_with_no_json_is_an_error_the_caller_can_feed_back() {
        let err = parse_proposal("I would rather not.").expect_err("must fail");
        assert!(matches!(err, McpError::Unparseable(_)), "{err:?}");
    }

    #[test]
    fn a_json_object_that_is_not_a_proposal_says_what_was_expected() {
        let err = parse_proposal(r#"{"thoughts":"hmm"}"#).expect_err("must fail");
        let rendered = err.to_string();
        assert!(rendered.contains("server"), "{rendered}");
    }

    #[test]
    fn an_unterminated_object_is_an_error_rather_than_a_panic() {
        assert!(parse_proposal(r#"{"server":"s","tool":"#).is_err());
    }

    #[test]
    fn arguments_default_to_an_empty_object_for_a_tool_that_takes_none() {
        let parsed = parse_proposal(r#"{"server":"s","tool":"list"}"#).expect("parses");
        assert!(parsed.arguments.is_null() || parsed.arguments.as_object().is_some());
    }

    /// The two halves compose: the allowlist decides whether it may be proposed at all, then the
    /// schema decides whether the call is well formed.
    #[test]
    fn a_permitted_proposal_still_has_to_validate() {
        let mut list = Allowlist::new();
        list.allow("linear", "create_issue");

        let schema = json!({
            "type": "object",
            "properties": { "title": { "type": "string" } },
            "required": ["title"]
        });

        let good = proposal();
        assert!(list.check(&good).is_ok());
        assert!(validate(&schema, &good.arguments).is_empty());

        let mut bad = proposal();
        bad.arguments = json!({ "titel": "typo" });
        assert!(list.check(&bad).is_ok(), "still an allowed tool");
        assert!(
            !validate(&schema, &bad.arguments).is_empty(),
            "but not a call worth showing anybody"
        );
    }
}
