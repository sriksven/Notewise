//! Masking secrets out of text before it leaves the machine.
//!
//! Notewise's promise is that the user chooses whether transcripts go to a cloud model. That
//! choice is meaningless if a transcript contains an API key someone read aloud, or a
//! screen-shared token that landed in a pasted note. Redaction is the safety net under the
//! choice, not a replacement for it.
//!
//! # What this does and does not claim
//!
//! This finds **secrets with recognisable shapes** — provider-prefixed API keys, AWS access
//! key ids, PEM private key blocks, JWTs, credentials embedded in URLs, and card numbers
//! that pass a Luhn check. It is deliberately not a general PII scrubber and **must not be
//! described to users as one**. A password spoken as ordinary words, an address, or an
//! account number with no distinguishing shape will pass straight through. Anything relying
//! on this for regulatory compliance is relying on the wrong thing.
//!
//! Contact details (emails, phone numbers) are handled separately under
//! [`RedactionPolicy::SecretsAndContacts`], because they are *not* incidental to a meeting
//! transcript: attendee emails are the input to drafting a follow-up. Redacting them by
//! default would break a feature the product is built around, so the caller opts in.

use std::borrow::Cow;

/// How aggressively to mask before sending text off the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionPolicy {
    /// Send text unchanged. Correct for a local backend, where nothing leaves the machine.
    Off,
    /// Mask credentials only. The default for anything non-local.
    #[default]
    Secrets,
    /// Also mask emails and phone numbers. Breaks email drafting, so it is opt-in.
    SecretsAndContacts,
}

impl RedactionPolicy {
    /// How much this masks. Higher masks more.
    fn strictness(self) -> u8 {
        match self {
            RedactionPolicy::Off => 0,
            RedactionPolicy::Secrets => 1,
            RedactionPolicy::SecretsAndContacts => 2,
        }
    }

    /// The policy of the two that masks more.
    ///
    /// Used to answer "what does this router mask" when several destinations are reachable and
    /// the question is asked without knowing which one a future call will take. Erring toward
    /// more masking is the only safe direction: under-reporting would tell a user their contacts
    /// are masked when a route exists that does not mask them.
    pub fn stricter(self, other: Self) -> Self {
        if other.strictness() > self.strictness() {
            other
        } else {
            self
        }
    }
}

/// What kind of thing was masked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    ApiKey,
    AwsAccessKeyId,
    PrivateKey,
    Jwt,
    UrlCredentials,
    CardNumber,
    Email,
    Phone,
}

impl Category {
    pub fn as_str(&self) -> &'static str {
        match self {
            Category::ApiKey => "api_key",
            Category::AwsAccessKeyId => "aws_access_key_id",
            Category::PrivateKey => "private_key",
            Category::Jwt => "jwt",
            Category::UrlCredentials => "url_credentials",
            Category::CardNumber => "card_number",
            Category::Email => "email",
            Category::Phone => "phone",
        }
    }

    fn is_secret(&self) -> bool {
        !matches!(self, Category::Email | Category::Phone)
    }

    fn placeholder(&self) -> &'static str {
        match self {
            Category::ApiKey => "[redacted:api_key]",
            Category::AwsAccessKeyId => "[redacted:aws_access_key_id]",
            Category::PrivateKey => "[redacted:private_key]",
            Category::Jwt => "[redacted:jwt]",
            Category::UrlCredentials => "[redacted:url_credentials]",
            Category::CardNumber => "[redacted:card_number]",
            Category::Email => "[redacted:email]",
            Category::Phone => "[redacted:phone]",
        }
    }
}

/// A count of what was masked. Deliberately carries no sample of the matched text — a
/// redaction report that quotes the secret defeats the purpose, including in logs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedactionReport {
    counts: Vec<(Category, usize)>,
}

impl RedactionReport {
    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }

    pub fn total(&self) -> usize {
        self.counts.iter().map(|(_, n)| n).sum()
    }

    pub fn counts(&self) -> &[(Category, usize)] {
        &self.counts
    }

    /// Fold another report into this one, for callers masking several fields in one request.
    pub(crate) fn merge(&mut self, other: &RedactionReport) {
        for (category, n) in &other.counts {
            match self.counts.iter_mut().find(|(c, _)| c == category) {
                Some((_, existing)) => *existing += n,
                None => self.counts.push((*category, *n)),
            }
        }
    }

    fn record(&mut self, category: Category) {
        match self.counts.iter_mut().find(|(c, _)| *c == category) {
            Some((_, n)) => *n += 1,
            None => self.counts.push((category, 1)),
        }
    }
}

impl std::fmt::Display for RedactionReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.counts.is_empty() {
            return f.write_str("nothing redacted");
        }
        let parts: Vec<String> = self
            .counts
            .iter()
            .map(|(c, n)| format!("{} x{n}", c.as_str()))
            .collect();
        f.write_str(&parts.join(", "))
    }
}

/// Mask secrets in `text` according to `policy`.
///
/// Returns the text borrowed and untouched when nothing matched, so the overwhelmingly
/// common case costs no allocation.
pub fn redact(text: &str, policy: RedactionPolicy) -> (Cow<'_, str>, RedactionReport) {
    let mut report = RedactionReport::default();
    if policy == RedactionPolicy::Off || text.is_empty() {
        return (Cow::Borrowed(text), report);
    }

    let spans = find_spans(text, policy, &mut report);
    if spans.is_empty() {
        return (Cow::Borrowed(text), report);
    }

    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for (start, end, category) in spans {
        out.push_str(&text[cursor..start]);
        out.push_str(category.placeholder());
        cursor = end;
    }
    out.push_str(&text[cursor..]);

    (Cow::Owned(out), report)
}

/// Byte spans to replace, in order and non-overlapping.
fn find_spans(
    text: &str,
    policy: RedactionPolicy,
    report: &mut RedactionReport,
) -> Vec<(usize, usize, Category)> {
    let mut spans: Vec<(usize, usize, Category)> = Vec::new();

    // PEM blocks first. They span lines and contain characters the token scanner would
    // otherwise chop up, so finding them whole avoids leaving half a key behind.
    let mut search_from = 0usize;
    while let Some(found) = text[search_from..].find("-----BEGIN") {
        let start = search_from + found;
        let Some(header_end) = text[start..].find("-----\n").map(|i| start + i + 6) else {
            break;
        };
        let end = match text[header_end..].find("-----END") {
            Some(i) => text[header_end + i..]
                .find('\n')
                .map(|j| header_end + i + j)
                .unwrap_or(text.len()),
            None => text.len(),
        };
        // Only key material, not every PEM object — a certificate is not a secret.
        if text[start..header_end].contains("PRIVATE KEY") {
            spans.push((start, end, Category::PrivateKey));
        }
        search_from = end;
    }

    // Numbers before tokens. A card or phone number in a transcript is written the way it
    // is spoken — "4111 1111 1111 1111" — so the whitespace-delimited token scanner would
    // see four harmless four-digit groups and let the whole thing through.
    for (start, end, run) in numeric_runs(text) {
        if spans.iter().any(|(s, e, _)| start < *e && end > *s) {
            continue;
        }
        if is_card_number(run) {
            spans.push((start, end, Category::CardNumber));
        } else if policy == RedactionPolicy::SecretsAndContacts && is_phone(run) {
            spans.push((start, end, Category::Phone));
        }
    }

    for (start, token) in tokens(text) {
        let end = start + token.len();
        if spans.iter().any(|(s, e, _)| start < *e && end > *s) {
            continue; // already inside a PEM block or a number
        }
        if let Some(category) = classify(token, policy) {
            spans.push((start, end, category));
        }
    }

    spans.sort_by_key(|(start, _, _)| *start);
    for (_, _, category) in &spans {
        report.record(*category);
    }
    spans
}

/// Split into candidate tokens with their byte offsets.
///
/// Splitting on whitespace alone would keep trailing punctuation ("sk-abc123." ), and
/// splitting on all punctuation would destroy the tokens we care about — JWTs contain dots,
/// URLs contain colons and slashes. So the delimiter set is whitespace plus the punctuation
/// that cannot appear inside any secret shape we recognise.
fn tokens(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;

    for (i, ch) in text.char_indices() {
        let is_delimiter = ch.is_whitespace()
            || matches!(
                ch,
                ',' | ';' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '`'
            );
        if is_delimiter {
            if let Some(s) = start.take() {
                out.push((s, &text[s..i]));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push((s, &text[s..]));
    }

    // Trim trailing sentence punctuation, which is not part of any secret.
    out.into_iter()
        .map(|(s, t)| {
            let trimmed = t.trim_end_matches(['.', '!', '?', ':']);
            (s, if trimmed.is_empty() { t } else { trimmed })
        })
        .collect()
}

fn classify(token: &str, policy: RedactionPolicy) -> Option<Category> {
    let category = classify_secret(token).or_else(|| {
        if policy == RedactionPolicy::SecretsAndContacts {
            classify_contact(token)
        } else {
            None
        }
    })?;

    if !category.is_secret() && policy != RedactionPolicy::SecretsAndContacts {
        return None;
    }
    Some(category)
}

/// Maximal runs of digits and the separators people write inside numbers, trimmed back to
/// digits at both ends.
///
/// Bounded to five groups so that a spoken list — "1 2 3 4 5 6 7 8 9 10 11" — is not
/// mistaken for one long phone number. Real card and phone formats use at most four or five.
fn numeric_runs(text: &str) -> Vec<(usize, usize, &str)> {
    fn is_number_char(c: char) -> bool {
        c.is_ascii_digit() || matches!(c, '+' | '-' | '(' | ')' | ' ' | '.')
    }

    /// Trim a raw run back to digits at both ends and keep it only if it looks like one
    /// number rather than a spoken list.
    fn keep<'t>(text: &'t str, start: usize, end: usize, runs: &mut Vec<(usize, usize, &'t str)>) {
        let slice = &text[start..end];
        let lead = slice.len()
            - slice
                .trim_start_matches(|c: char| !c.is_ascii_digit())
                .len();
        let trail = slice.len() - slice.trim_end_matches(|c: char| !c.is_ascii_digit()).len();
        if lead + trail >= slice.len() {
            return; // no digits at all
        }

        let (start, end) = (start + lead, end - trail);
        let trimmed = &text[start..end];
        let groups = trimmed
            .split(|c: char| !c.is_ascii_digit())
            .filter(|g| !g.is_empty())
            .count();
        if groups <= 5 {
            runs.push((start, end, trimmed));
        }
    }

    let mut runs = Vec::new();
    let mut start: Option<usize> = None;

    for (i, ch) in text.char_indices() {
        if is_number_char(ch) {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            keep(text, s, i, &mut runs);
        }
    }
    if let Some(s) = start {
        keep(text, s, text.len(), &mut runs);
    }

    runs
}

fn classify_secret(token: &str) -> Option<Category> {
    if let Some(rest) = token.strip_prefix("AKIA") {
        // AWS access key ids are AKIA + 16 uppercase alphanumerics.
        if rest.len() == 16
            && rest
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        {
            return Some(Category::AwsAccessKeyId);
        }
    }

    // Provider-prefixed keys. Length guards keep "sk-" in prose from matching.
    const KEY_PREFIXES: &[(&str, usize)] = &[
        ("sk-ant-", 20),
        ("sk-", 20),
        ("ghp_", 20),
        ("gho_", 20),
        ("ghs_", 20),
        ("ghu_", 20),
        ("github_pat_", 20),
        ("xoxb-", 20),
        ("xoxp-", 20),
        ("xapp-", 20),
        ("glpat-", 15),
        ("AIza", 30),
        ("SG.", 30),
    ];
    for (prefix, min_len) in KEY_PREFIXES {
        if token.len() >= *min_len
            && token.starts_with(prefix)
            && token[prefix.len()..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Some(Category::ApiKey);
        }
    }

    if is_jwt(token) {
        return Some(Category::Jwt);
    }
    if has_url_credentials(token) {
        return Some(Category::UrlCredentials);
    }
    if is_card_number(token) {
        return Some(Category::CardNumber);
    }
    None
}

fn classify_contact(token: &str) -> Option<Category> {
    if is_email(token) {
        return Some(Category::Email);
    }
    if is_phone(token) {
        return Some(Category::Phone);
    }
    None
}

/// Three base64url segments, the first decoding to something that starts a JSON object.
///
/// `eyJ` is base64 for `{"`, which is what every JWT header begins with. Checking that
/// rather than only the segment count avoids matching ordinary dotted identifiers.
fn is_jwt(token: &str) -> bool {
    let parts: Vec<&str> = token.split('.').collect();
    parts.len() == 3
        && parts[0].starts_with("eyJ")
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '='))
        })
}

/// `scheme://user:password@host`. The password is the secret; the host is not.
fn has_url_credentials(token: &str) -> bool {
    let Some(after_scheme) = token.split_once("://").map(|(_, rest)| rest) else {
        return false;
    };
    let Some((userinfo, host)) = after_scheme.split_once('@') else {
        return false;
    };
    // A colon in the userinfo means a password is present. `user@host` alone is not a secret.
    !host.is_empty() && userinfo.contains(':') && !userinfo.starts_with(':')
}

fn is_card_number(token: &str) -> bool {
    let digits: Vec<u32> = token
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .map(|c| c.to_digit(10).unwrap_or(u32::MAX))
        .collect();

    if digits.contains(&u32::MAX) || !(13..=19).contains(&digits.len()) {
        return false;
    }
    luhn(&digits)
}

/// The check digit algorithm every card number satisfies. Without it, any long number —
/// a meeting id, a phone number, a row count — would be masked as a card.
fn luhn(digits: &[u32]) -> bool {
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| {
            if i % 2 == 1 {
                let doubled = d * 2;
                if doubled > 9 {
                    doubled - 9
                } else {
                    doubled
                }
            } else {
                *d
            }
        })
        .sum();
    sum % 10 == 0
}

fn is_email(token: &str) -> bool {
    let Some((local, domain)) = token.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
}

fn is_phone(token: &str) -> bool {
    let digits = token.chars().filter(char::is_ascii_digit).count();
    let allowed = token
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '(' | ')' | ' ' | '.'));
    allowed && (10..=15).contains(&digits) && !is_card_number(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secrets(text: &str) -> (String, RedactionReport) {
        let (out, report) = redact(text, RedactionPolicy::Secrets);
        (out.into_owned(), report)
    }

    #[test]
    fn ordinary_transcript_text_is_untouched_and_not_reallocated() {
        let text = "Priya said we should ship on Friday. Sam disagreed about the timeline.";
        let (out, report) = redact(text, RedactionPolicy::Secrets);

        assert!(matches!(out, Cow::Borrowed(_)), "should not allocate");
        assert!(report.is_empty());
    }

    #[test]
    fn an_anthropic_key_read_aloud_is_masked() {
        let (out, report) =
            secrets("The key is sk-ant-api03-AAAABBBBCCCCDDDDEEEEFFFF and it works.");

        assert!(!out.contains("sk-ant-api03"), "{out}");
        assert!(out.contains("[redacted:api_key]"), "{out}");
        assert_eq!(report.total(), 1);
    }

    #[test]
    fn github_and_slack_tokens_are_masked() {
        for token in [
            "ghp_AAAABBBBCCCCDDDDEEEEFFFFGGGG",
            "xoxb-1234567890-ABCDEFGHIJKLMNOP",
        ] {
            let (out, _) = secrets(&format!("token: {token}"));
            assert!(!out.contains(token), "{token} survived as {out}");
        }
    }

    #[test]
    fn an_aws_access_key_id_is_masked_but_a_similar_word_is_not() {
        let (out, _) = secrets("AKIAIOSFODNN7EXAMPLE is the id");
        assert!(out.contains("[redacted:aws_access_key_id]"), "{out}");

        let (out, report) = secrets("AKIA is just four letters");
        assert!(out.contains("AKIA"), "{out}");
        assert!(report.is_empty());
    }

    #[test]
    fn a_private_key_block_is_masked_whole() {
        let text = "here it is:\n-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA\nabc123\n-----END RSA PRIVATE KEY-----\nthanks";
        let (out, report) = secrets(text);

        assert!(!out.contains("MIIEowIBAAKCAQEA"), "{out}");
        assert!(!out.contains("BEGIN RSA PRIVATE KEY"), "{out}");
        assert!(out.contains("here it is:"), "surrounding text must survive");
        assert!(out.contains("thanks"), "surrounding text must survive");
        assert_eq!(report.counts(), &[(Category::PrivateKey, 1)]);
    }

    /// A certificate is public by design. Masking it would be noise.
    #[test]
    fn a_certificate_block_is_left_alone() {
        let text = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
        let (out, report) = secrets(text);
        assert!(out.contains("CERTIFICATE"), "{out}");
        assert!(report.is_empty());
    }

    #[test]
    fn a_jwt_is_masked_but_a_dotted_identifier_is_not() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NSJ9.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1g";
        let (out, _) = secrets(&format!("bearer {jwt}"));
        assert!(out.contains("[redacted:jwt]"), "{out}");

        let (out, report) = secrets("see notewise.core.storage for details");
        assert!(out.contains("notewise.core.storage"), "{out}");
        assert!(report.is_empty());
    }

    #[test]
    fn credentials_in_a_url_are_masked_but_a_plain_url_is_not() {
        let (out, _) = secrets("connect to postgres://admin:hunter2@db.internal:5432/app");
        assert!(!out.contains("hunter2"), "{out}");

        let (out, report) = secrets("the docs are at https://notewise.dev/getting-started");
        assert!(
            out.contains("https://notewise.dev/getting-started"),
            "{out}"
        );
        assert!(report.is_empty());
    }

    /// Without a Luhn check every long number in a transcript would be masked as a card.
    #[test]
    fn a_card_number_is_masked_and_an_ordinary_long_number_is_not() {
        let (out, _) = secrets("card 4111 1111 1111 1111 on file");
        assert!(out.contains("[redacted:card_number]"), "{out}");

        let (out, report) = secrets("we processed 1234567890123456 events last quarter");
        assert!(out.contains("1234567890123456"), "{out}");
        assert!(report.is_empty(), "{report}");
    }

    /// Attendee emails are the input to drafting a follow-up. Masking them by default would
    /// break the feature the product is built around.
    #[test]
    fn emails_survive_the_default_policy_and_are_masked_only_on_request() {
        let text = "email priya@example.com about it";

        let (out, report) = secrets(text);
        assert!(out.contains("priya@example.com"), "{out}");
        assert!(report.is_empty());

        let (out, report) = redact(text, RedactionPolicy::SecretsAndContacts);
        assert!(!out.contains("priya@example.com"), "{out}");
        assert_eq!(report.counts(), &[(Category::Email, 1)]);
    }

    #[test]
    fn a_phone_number_is_masked_only_under_the_contacts_policy() {
        let text = "call +1 (555) 123-4567 tomorrow";

        assert!(secrets(text).1.is_empty());

        let (out, report) = redact(text, RedactionPolicy::SecretsAndContacts);
        assert!(out.contains("[redacted:phone]"), "{out}");
        assert_eq!(report.total(), 1);
    }

    #[test]
    fn the_off_policy_changes_nothing_even_with_a_key_present() {
        let text = "sk-ant-api03-AAAABBBBCCCCDDDDEEEEFFFF";
        let (out, report) = redact(text, RedactionPolicy::Off);

        assert_eq!(out, text);
        assert!(report.is_empty());
    }

    #[test]
    fn several_secrets_in_one_transcript_are_all_masked() {
        let text = "key sk-ant-api03-AAAABBBBCCCCDDDDEEEEFFFF and id AKIAIOSFODNN7EXAMPLE";
        let (out, report) = secrets(text);

        assert!(!out.contains("sk-ant"), "{out}");
        assert!(!out.contains("AKIAIOSF"), "{out}");
        assert_eq!(report.total(), 2);
    }

    /// A report that quotes what it found would leak the secret into logs and UI.
    #[test]
    fn the_report_never_contains_the_matched_text() {
        let (_, report) = secrets("key sk-ant-api03-AAAABBBBCCCCDDDDEEEEFFFF");
        let rendered = report.to_string();

        assert!(!rendered.contains("sk-ant"), "{rendered}");
        assert!(rendered.contains("api_key"), "{rendered}");
    }

    #[test]
    fn masking_preserves_the_surrounding_sentence() {
        let (out, _) = secrets("Sam pasted ghp_AAAABBBBCCCCDDDDEEEEFFFFGGGG into the channel.");
        assert!(out.starts_with("Sam pasted "), "{out}");
        assert!(out.ends_with(" into the channel."), "{out}");
    }

    #[test]
    fn multibyte_text_around_a_secret_is_not_corrupted() {
        let (out, _) = secrets("Café — clé sk-ant-api03-AAAABBBBCCCCDDDDEEEEFFFF — fin");
        assert!(out.contains("Café"), "{out}");
        assert!(out.contains("fin"), "{out}");
        assert!(out.contains("[redacted:api_key]"), "{out}");
    }

    #[test]
    fn empty_input_is_handled() {
        let (out, report) = secrets("");
        assert!(out.is_empty());
        assert!(report.is_empty());
    }
}
