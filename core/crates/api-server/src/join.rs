//! Noticing that a meeting has started, and offering to record it.
//!
//! # Why the decision lives in one pure object
//!
//! Signals arrive from different places at different times and each is individually unreliable. A
//! tab being open is not a meeting; a calendar entry is not attendance. Fusing them in one place
//! means the rules about *when to speak* are one testable object rather than scattered across the
//! things that produce signals.
//!
//! This is the shape `clarify.rs` already uses, for the reason it states: almost all the difficulty
//! is in *when* to speak rather than what to say.
//!
//! # Why detection never starts a recording
//!
//! It creates a notification and waits to be clicked. Recording on a guess would mean capturing
//! audio of other people because software inferred a meeting began — and a false positive there is a
//! recording nobody knew about, which is a different category of failure from a missed meeting. One
//! is lost value; the other is a breach of the thing the product sells. The asymmetry decides it.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use notewise_storage::Id;
use serde::{Deserialize, Serialize};

/// Where a signal came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalSource {
    /// The browser extension, watching a meeting page.
    Extension,
    /// A calendar event that is happening now.
    Calendar,
    /// A native audio-session check. Not implemented; the variant exists so fusion does not have to
    /// change when it is.
    Native,
}

/// Which meeting platform, when it is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Meet,
    Zoom,
    Teams,
    Unknown,
}

impl Platform {
    /// Recognise a platform from a meeting URL.
    /// A platform named outright by whoever knows.
    ///
    /// The extension knows which adapter matched the page, so it says so rather than sending a URL
    /// for the engine to re-derive it from — one fewer thing on the wire and one fewer regex to keep
    /// in step with a vendor's link format.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "meet" | "google-meet" | "google_meet" => Platform::Meet,
            "zoom" | "zoom-web" => Platform::Zoom,
            "teams" | "teams-web" | "microsoft-teams" => Platform::Teams,
            "unknown" => Platform::Unknown,
            _ => return None,
        })
    }

    pub fn from_url(url: &str) -> Self {
        let lower = url.to_ascii_lowercase();
        if lower.contains("meet.google.com") {
            Platform::Meet
        } else if lower.contains("zoom.us") {
            Platform::Zoom
        } else if lower.contains("teams.microsoft.com") || lower.contains("teams.live.com") {
            Platform::Teams
        } else {
            Platform::Unknown
        }
    }
}

/// How much a signal is worth on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Suggestive. A calendar full of meetings nobody attends is the reason this exists.
    Weak,
    /// Deliberate. Somebody has a meeting page open.
    Strong,
}

/// One observation that a meeting might be happening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinSignal {
    pub source: SignalSource,
    pub platform: Option<Platform>,
    pub confidence: Confidence,
    /// A calendar external id or a meeting URL, whichever the producer has.
    pub external_ref: Option<String>,
    /// Whether the user declined this. A declined event never produces a notification.
    pub declined: bool,
    /// A title to put in the prompt, so a recording starts labelled rather than as "Meeting".
    pub title: Option<String>,
    pub observed_at: DateTime<Utc>,
}

/// What the tracker decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Tell the user now.
    Notify {
        key: String,
        title: Option<String>,
        platform: Option<Platform>,
    },
    /// A weak signal on its own. Ask again after this.
    Waiting { until: DateTime<Utc> },
    /// Deliberately silent, and why.
    Silent { reason: &'static str },
}

impl Decision {
    pub fn notifies(&self) -> bool {
        matches!(self, Decision::Notify { .. })
    }
}

/// Tuning.
#[derive(Debug, Clone, Copy)]
pub struct TrackerConfig {
    /// How long a lone weak signal waits before it is worth mentioning.
    ///
    /// Calendars are full of meetings nobody attends. Waiting converts "your calendar says
    /// something" into "something appears to be happening".
    pub grace: Duration,
    /// How long one meeting stays deduplicated.
    ///
    /// Repeats are what kills this feature: three notifications for one standup and it gets turned
    /// off, permanently.
    pub dedup_window: Duration,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            grace: Duration::minutes(2),
            dedup_window: Duration::hours(2),
        }
    }
}

/// An offer to record something that appears to be happening.
///
/// Kept in memory rather than stored. The whole lifetime of one of these is the few minutes at the
/// start of a meeting: a row that outlived the app would be an offer to record a meeting that
/// finished yesterday, and a notification saying a meeting is starting delivered forty minutes late
/// is worse than none. The `notifications` row is the durable half, and it is durable because
/// delivering it is somebody else's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub id: Id,
    /// The notification queued for this, so the interface can mark it delivered when it shows the
    /// offer itself and the user does not get told twice.
    pub notification_id: Option<Id>,
    pub key: String,
    pub title: Option<String>,
    pub platform: Option<Platform>,
    /// A calendar event's external id, when the signal carried one. What makes the resulting
    /// recording linkable to the event directly rather than inferred from time overlap later.
    pub external_ref: Option<String>,
    pub created_at: DateTime<Utc>,
    pub dismissed: bool,
}

impl Offer {
    /// What to call the meeting if this is accepted.
    ///
    /// The calendar title when there is one, and a platform-shaped placeholder otherwise — "A Zoom
    /// call" beats "Meeting 2026-08-18", which is what pressing record by hand produces.
    pub fn suggested_title(&self, now: DateTime<Utc>) -> String {
        if let Some(title) = self.title.as_deref().filter(|t| !t.trim().is_empty()) {
            return title.to_string();
        }

        match self.platform {
            Some(Platform::Meet) => "Google Meet call".into(),
            Some(Platform::Zoom) => "Zoom call".into(),
            Some(Platform::Teams) => "Teams call".into(),
            _ => format!("Meeting {}", now.format("%Y-%m-%d %H:%M")),
        }
    }
}

/// How many offers to keep, and how long one is worth showing.
///
/// Bounded because this is a queue nothing drains on a quiet machine: a tab left open across a
/// weekend of standups would otherwise accumulate one per meeting forever.
pub const MAX_OFFERS: usize = 8;

/// After this, an offer is stale. An offer to record a meeting that started half an hour ago cannot
/// be kept — the recording would be missing the half hour that mattered.
pub const OFFER_TTL_MINUTES: i64 = 10;

/// Fuses signals and decides when to speak.
#[derive(Debug)]
pub struct JoinTracker {
    config: TrackerConfig,
    /// Weak signals seen but not yet acted on, by key.
    pending: HashMap<String, Pending>,
    /// Keys already notified, and when.
    notified: HashMap<String, DateTime<Utc>>,
    /// Offers currently worth showing, oldest first.
    offers: Vec<Offer>,
    /// When each source was last heard from.
    ///
    /// For the settings screen and nothing else. "The extension has not been heard from" is the
    /// answer to "why is this not working", and without it the only way to find out is to read a
    /// log — which means the answer is unavailable to the person who needs it.
    last_seen: HashMap<SignalSource, DateTime<Utc>>,
}

#[derive(Debug, Clone)]
struct Pending {
    first_seen: DateTime<Utc>,
    /// Distinct sources that have reported this key. Two weak sources agreeing is as good as one
    /// strong one — they are independent observations of the same thing.
    sources: Vec<SignalSource>,
    title: Option<String>,
    platform: Option<Platform>,
}

impl Default for JoinTracker {
    fn default() -> Self {
        Self::new(TrackerConfig::default())
    }
}

impl JoinTracker {
    pub fn new(config: TrackerConfig) -> Self {
        Self {
            config,
            pending: HashMap::new(),
            notified: HashMap::new(),
            offers: Vec::new(),
            last_seen: HashMap::new(),
        }
    }

    /// When each source last reported anything, whether or not it led to a notification.
    pub fn last_seen(&self) -> Vec<(SignalSource, DateTime<Utc>)> {
        let mut seen: Vec<(SignalSource, DateTime<Utc>)> =
            self.last_seen.iter().map(|(k, v)| (*k, *v)).collect();
        seen.sort_by_key(|(source, _)| *source);
        seen
    }

    /// Record an offer for a decision that notified.
    ///
    /// Separate from [`Self::observe`] so the decision stays a pure function of the signals: what to
    /// do about a decision is the caller's, and the caller is the one that knows whether queuing the
    /// notification actually worked.
    pub fn offer(
        &mut self,
        key: String,
        title: Option<String>,
        platform: Option<Platform>,
        external_ref: Option<String>,
        notification_id: Option<Id>,
        now: DateTime<Utc>,
    ) -> Offer {
        let offer = Offer {
            id: Id::new(),
            notification_id,
            key,
            title,
            platform,
            external_ref,
            created_at: now,
            dismissed: false,
        };

        self.offers.push(offer.clone());
        self.expire_offers(now);
        offer
    }

    /// Offers still worth showing: not dismissed, not stale.
    pub fn live_offers(&mut self, now: DateTime<Utc>) -> Vec<Offer> {
        self.expire_offers(now);
        self.offers
            .iter()
            .filter(|offer| !offer.dismissed)
            .cloned()
            .collect()
    }

    pub fn offer_by_id(&self, id: Id) -> Option<&Offer> {
        self.offers.iter().find(|offer| offer.id == id)
    }

    /// Put an offer away.
    ///
    /// Answers whether there is an offer with that id, so dismissing one twice succeeds twice — a
    /// double click must not read as a failure. `false` means it expired or never existed, which is
    /// the same thing from the interface's side: the card should disappear either way.
    pub fn dismiss(&mut self, id: Id) -> bool {
        match self.offers.iter_mut().find(|offer| offer.id == id) {
            Some(offer) => {
                offer.dismissed = true;
                true
            }
            None => false,
        }
    }

    /// Drop what is stale, and keep the list bounded.
    fn expire_offers(&mut self, now: DateTime<Utc>) {
        let cutoff = now - Duration::minutes(OFFER_TTL_MINUTES);
        self.offers.retain(|offer| offer.created_at > cutoff);

        if self.offers.len() > MAX_OFFERS {
            let excess = self.offers.len() - MAX_OFFERS;
            self.offers.drain(0..excess);
        }
    }

    /// Take a signal and decide.
    pub fn observe(&mut self, signal: &JoinSignal, now: DateTime<Utc>) -> Decision {
        // Recorded before any of the rules, including the declined one: the question this answers is
        // "is the extension talking to me at all", and a declined meeting still proves that it is.
        self.last_seen.insert(signal.source, now);

        // Checked first: notifying about a meeting somebody explicitly declined is the most
        // annoying false positive available.
        if signal.declined {
            return Decision::Silent {
                reason: "the user declined this meeting",
            };
        }

        let key = dedup_key(signal);

        if let Some(at) = self.notified.get(&key) {
            if now - *at < self.config.dedup_window {
                return Decision::Silent {
                    reason: "already mentioned this meeting",
                };
            }
        }

        // A meeting page open is deliberate — somebody navigated there. No waiting.
        if signal.confidence == Confidence::Strong {
            self.notified.insert(key.clone(), now);
            self.pending.remove(&key);
            return Decision::Notify {
                key,
                title: signal.title.clone(),
                platform: signal.platform,
            };
        }

        let entry = self.pending.entry(key.clone()).or_insert_with(|| Pending {
            first_seen: signal.observed_at,
            sources: Vec::new(),
            title: signal.title.clone(),
            platform: signal.platform,
        });
        if !entry.sources.contains(&signal.source) {
            entry.sources.push(signal.source);
        }
        // A later signal may know more than the first — a calendar poll that runs after the
        // extension has a title where the extension had none.
        if entry.title.is_none() {
            entry.title = signal.title.clone();
        }
        if entry.platform.is_none() {
            entry.platform = signal.platform;
        }

        let corroborated = entry.sources.len() >= 2;
        let waited_long_enough = now - entry.first_seen >= self.config.grace;

        if corroborated || waited_long_enough {
            let pending = self.pending.remove(&key).expect("just inserted");
            self.notified.insert(key.clone(), now);
            return Decision::Notify {
                key,
                title: pending.title,
                platform: pending.platform,
            };
        }

        Decision::Waiting {
            until: entry.first_seen + self.config.grace,
        }
    }

    /// Forget state older than the dedup window.
    ///
    /// Called periodically so a long-running process does not accumulate a key per meeting forever.
    pub fn prune(&mut self, now: DateTime<Utc>) {
        let window = self.config.dedup_window;
        self.notified.retain(|_, at| now - *at < window);
        self.pending.retain(|_, p| now - p.first_seen < window);
    }

    pub fn tracked(&self) -> usize {
        self.pending.len() + self.notified.len()
    }
}

/// The identity of a meeting occurrence, from the strongest thing available.
///
/// Calendar id first, then the normalised URL, then platform plus a coarse time bucket. The bucket
/// exists so two signals minutes apart about the same untitled Zoom call collapse — without it, a
/// meeting with no identifiable reference would notify on every poll.
pub fn dedup_key(signal: &JoinSignal) -> String {
    if let Some(reference) = signal.external_ref.as_deref() {
        let trimmed = reference.trim();
        if !trimmed.is_empty() {
            return if trimmed.starts_with("http") {
                format!("url:{}", normalize_url(trimmed))
            } else {
                format!("cal:{trimmed}")
            };
        }
    }

    // Fifteen-minute buckets: long enough that signals about one meeting collapse, short enough
    // that back-to-back calls do not.
    let bucket = signal.observed_at.timestamp() / (15 * 60);
    let platform = signal
        .platform
        .map(|p| format!("{p:?}"))
        .unwrap_or_else(|| "unknown".into());
    format!("time:{platform}:{bucket}")
}

/// Reduce a meeting URL to the part that identifies the meeting.
///
/// Query strings carry per-participant tokens, so two people's links to one call differ. Comparing
/// them raw would treat the same meeting as two.
pub fn normalize_url(url: &str) -> String {
    let lower = url.trim().to_ascii_lowercase();
    let without_scheme = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .unwrap_or(&lower);
    let without_query = without_scheme
        .split(['?', '#'])
        .next()
        .unwrap_or(without_scheme);
    without_query.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(mins: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + mins * 60, 0).expect("timestamp")
    }

    fn signal(source: SignalSource, confidence: Confidence, at: DateTime<Utc>) -> JoinSignal {
        JoinSignal {
            source,
            platform: Some(Platform::Meet),
            confidence,
            external_ref: Some("cal-evt-1".into()),
            declined: false,
            title: Some("Platform standup".into()),
            observed_at: at,
        }
    }

    /// Somebody navigated to a meeting page. That is deliberate and needs no corroboration.
    #[test]
    fn a_strong_signal_notifies_at_once() {
        let mut tracker = JoinTracker::default();
        let decision = tracker.observe(
            &signal(SignalSource::Extension, Confidence::Strong, at(0)),
            at(0),
        );

        assert!(decision.notifies(), "{decision:?}");
        match decision {
            Decision::Notify { title, .. } => {
                assert_eq!(title.as_deref(), Some("Platform standup"))
            }
            other => panic!("{other:?}"),
        }
    }

    /// Calendars are full of meetings nobody attends.
    #[test]
    fn a_lone_weak_signal_waits_then_speaks() {
        let mut tracker = JoinTracker::default();
        let first = tracker.observe(
            &signal(SignalSource::Calendar, Confidence::Weak, at(0)),
            at(0),
        );
        assert!(matches!(first, Decision::Waiting { .. }), "{first:?}");

        // Same signal seen again after the grace period.
        let later = tracker.observe(
            &signal(SignalSource::Calendar, Confidence::Weak, at(0)),
            at(3),
        );
        assert!(later.notifies(), "{later:?}");
    }

    /// Two independent observations of the same thing are as good as one deliberate one.
    #[test]
    fn two_weak_sources_corroborate_without_waiting() {
        let mut tracker = JoinTracker::default();
        tracker.observe(
            &signal(SignalSource::Calendar, Confidence::Weak, at(0)),
            at(0),
        );

        let second = tracker.observe(
            &signal(SignalSource::Native, Confidence::Weak, at(0)),
            at(0),
        );
        assert!(second.notifies(), "{second:?}");
    }

    /// The same source reporting twice is one observation, not two.
    #[test]
    fn the_same_weak_source_twice_is_not_corroboration() {
        let mut tracker = JoinTracker::default();
        tracker.observe(
            &signal(SignalSource::Calendar, Confidence::Weak, at(0)),
            at(0),
        );
        let again = tracker.observe(
            &signal(SignalSource::Calendar, Confidence::Weak, at(0)),
            at(1),
        );

        assert!(
            matches!(again, Decision::Waiting { .. }),
            "one source agreeing with itself proves nothing: {again:?}"
        );
    }

    /// Three notifications for one standup and the feature gets turned off, permanently.
    #[test]
    fn one_meeting_is_mentioned_once() {
        let mut tracker = JoinTracker::default();
        assert!(tracker
            .observe(
                &signal(SignalSource::Extension, Confidence::Strong, at(0)),
                at(0)
            )
            .notifies());

        for minute in [1, 5, 30, 100] {
            let again = tracker.observe(
                &signal(SignalSource::Extension, Confidence::Strong, at(minute)),
                at(minute),
            );
            assert!(!again.notifies(), "at {minute}m: {again:?}");
        }
    }

    #[test]
    fn a_meeting_can_be_mentioned_again_after_the_window() {
        let mut tracker = JoinTracker::default();
        tracker.observe(
            &signal(SignalSource::Extension, Confidence::Strong, at(0)),
            at(0),
        );

        // A weekly standup with the same calendar id, a week later.
        let next_week = tracker.observe(
            &signal(SignalSource::Extension, Confidence::Strong, at(60 * 24 * 7)),
            at(60 * 24 * 7),
        );
        assert!(next_week.notifies(), "{next_week:?}");
    }

    /// The most annoying false positive available.
    #[test]
    fn a_declined_meeting_is_never_mentioned() {
        let mut tracker = JoinTracker::default();
        let mut declined = signal(SignalSource::Extension, Confidence::Strong, at(0));
        declined.declined = true;

        let decision = tracker.observe(&declined, at(0));
        assert!(matches!(decision, Decision::Silent { .. }), "{decision:?}");
    }

    /// Two people's links to one call differ by a per-participant token.
    #[test]
    fn urls_are_normalised_before_comparing() {
        assert_eq!(
            normalize_url("https://meet.google.com/abc-defg-hij?authuser=1"),
            normalize_url("http://meet.google.com/abc-defg-hij/")
        );
        assert_eq!(
            normalize_url("https://Zoom.us/j/123#success"),
            "zoom.us/j/123"
        );
    }

    #[test]
    fn a_calendar_id_and_a_url_produce_different_keys() {
        let mut by_id = signal(SignalSource::Calendar, Confidence::Weak, at(0));
        by_id.external_ref = Some("evt-1".into());

        let mut by_url = signal(SignalSource::Extension, Confidence::Strong, at(0));
        by_url.external_ref = Some("https://meet.google.com/abc".into());

        assert_ne!(dedup_key(&by_id), dedup_key(&by_url));
        assert!(dedup_key(&by_id).starts_with("cal:"));
        assert!(dedup_key(&by_url).starts_with("url:"));
    }

    /// Without a bucket, a meeting with no identifiable reference notifies on every poll.
    #[test]
    fn signals_with_no_reference_collapse_within_a_bucket() {
        let mut anonymous = signal(SignalSource::Extension, Confidence::Strong, at(0));
        anonymous.external_ref = None;

        let mut minutes_later = anonymous.clone();
        minutes_later.observed_at = at(3);

        assert_eq!(dedup_key(&anonymous), dedup_key(&minutes_later));

        let mut much_later = anonymous.clone();
        much_later.observed_at = at(40);
        assert_ne!(dedup_key(&anonymous), dedup_key(&much_later));
    }

    #[test]
    fn a_later_signal_can_supply_a_title_the_first_lacked() {
        let mut tracker = JoinTracker::default();
        let mut untitled = signal(SignalSource::Extension, Confidence::Weak, at(0));
        untitled.title = None;
        untitled.platform = None;
        tracker.observe(&untitled, at(0));

        let titled = signal(SignalSource::Calendar, Confidence::Weak, at(0));
        match tracker.observe(&titled, at(0)) {
            Decision::Notify {
                title, platform, ..
            } => {
                assert_eq!(title.as_deref(), Some("Platform standup"));
                assert_eq!(platform, Some(Platform::Meet));
            }
            other => panic!("expected corroboration, got {other:?}"),
        }
    }

    #[test]
    fn platforms_are_recognised_from_their_urls() {
        assert_eq!(
            Platform::from_url("https://meet.google.com/x"),
            Platform::Meet
        );
        assert_eq!(
            Platform::from_url("https://acme.zoom.us/j/1"),
            Platform::Zoom
        );
        assert_eq!(
            Platform::from_url("https://teams.microsoft.com/l/x"),
            Platform::Teams
        );
        assert_eq!(
            Platform::from_url("https://example.com/call"),
            Platform::Unknown
        );
    }

    /// A long-running process must not accumulate a key per meeting forever.
    #[test]
    fn state_is_pruned_after_the_window() {
        let mut tracker = JoinTracker::default();
        tracker.observe(
            &signal(SignalSource::Extension, Confidence::Strong, at(0)),
            at(0),
        );
        assert_eq!(tracker.tracked(), 1);

        tracker.prune(at(60 * 24));
        assert_eq!(tracker.tracked(), 0);
    }

    #[test]
    fn pruning_keeps_what_is_still_current() {
        let mut tracker = JoinTracker::default();
        tracker.observe(
            &signal(SignalSource::Extension, Confidence::Strong, at(0)),
            at(0),
        );

        tracker.prune(at(5));
        assert_eq!(tracker.tracked(), 1, "still inside the dedup window");
    }
    // ------------------------------------------------------------ offers

    /// An offer is what the interface shows and what a click turns into a labelled recording.
    #[test]
    fn an_offer_survives_until_it_is_answered() {
        let mut tracker = JoinTracker::default();
        let offer = tracker.offer(
            "key".into(),
            Some("Platform standup".into()),
            Some(Platform::Meet),
            Some("cal:abc".into()),
            None,
            at(0),
        );

        assert_eq!(tracker.live_offers(at(1)).len(), 1);
        assert!(tracker.dismiss(offer.id));
        assert!(tracker.live_offers(at(1)).is_empty());
        assert!(
            tracker.dismiss(offer.id),
            "a double click must not read as a failure"
        );
        assert!(
            !tracker.dismiss(Id::new()),
            "an offer that never existed is not dismissed"
        );
    }

    /// A prompt to record a meeting that started half an hour ago cannot be kept: the recording
    /// would be missing the half hour that mattered.
    #[test]
    fn a_stale_offer_stops_being_offered() {
        let mut tracker = JoinTracker::default();
        tracker.offer("key".into(), None, Some(Platform::Zoom), None, None, at(0));

        assert_eq!(tracker.live_offers(at(OFFER_TTL_MINUTES - 1)).len(), 1);
        assert!(tracker.live_offers(at(OFFER_TTL_MINUTES + 1)).is_empty());
    }

    /// A tab left open across a weekend of standups must not accumulate one per meeting forever.
    #[test]
    fn offers_are_bounded() {
        let mut tracker = JoinTracker::default();
        for n in 0..MAX_OFFERS + 5 {
            tracker.offer(format!("key-{n}"), None, None, None, None, at(0));
        }
        assert_eq!(tracker.live_offers(at(0)).len(), MAX_OFFERS);
    }

    /// "A Zoom call" beats "Meeting 2026-08-18", which is what pressing record by hand produces.
    #[test]
    fn an_offer_without_a_title_is_still_named_by_its_platform() {
        let mut tracker = JoinTracker::default();

        let titled = tracker.offer(
            "a".into(),
            Some("Q3 planning".into()),
            Some(Platform::Teams),
            None,
            None,
            at(0),
        );
        assert_eq!(titled.suggested_title(at(0)), "Q3 planning");

        let untitled = tracker.offer("b".into(), None, Some(Platform::Zoom), None, None, at(0));
        assert_eq!(untitled.suggested_title(at(0)), "Zoom call");

        let anonymous = tracker.offer("c".into(), None, None, None, None, at(0));
        assert!(
            anonymous.suggested_title(at(0)).starts_with("Meeting "),
            "{}",
            anonymous.suggested_title(at(0))
        );
    }

    /// A blank title is not a title.
    #[test]
    fn a_whitespace_title_falls_back_to_the_platform() {
        let mut tracker = JoinTracker::default();
        let offer = tracker.offer(
            "a".into(),
            Some("   ".into()),
            Some(Platform::Meet),
            None,
            None,
            at(0),
        );
        assert_eq!(offer.suggested_title(at(0)), "Google Meet call");
    }

    // ------------------------------------------------------------ the platform, named outright

    /// The extension names the platform rather than sending a URL for the engine to re-derive it
    /// from, so the names it uses have to be the ones this understands.
    #[test]
    fn the_names_the_extension_sends_are_understood() {
        assert_eq!(Platform::parse("meet"), Some(Platform::Meet));
        assert_eq!(Platform::parse("zoom"), Some(Platform::Zoom));
        assert_eq!(Platform::parse("teams"), Some(Platform::Teams));

        // The adapter names, in case a producer sends those instead.
        assert_eq!(Platform::parse("google-meet"), Some(Platform::Meet));
        assert_eq!(Platform::parse("zoom-web"), Some(Platform::Zoom));
        assert_eq!(Platform::parse("teams-web"), Some(Platform::Teams));

        assert_eq!(Platform::parse("MEET"), Some(Platform::Meet));
        assert_eq!(Platform::parse("webex"), None);
    }

    // ------------------------------------------------------------ the calendar signal

    fn event(
        title: Option<&str>,
        join_url: Option<&str>,
        status: notewise_storage::EventStatus,
        starts: DateTime<Utc>,
        ends: DateTime<Utc>,
    ) -> notewise_storage::CalendarEvent {
        notewise_storage::CalendarEvent {
            id: notewise_storage::Id::new(),
            external_item_id: notewise_storage::Id::new(),
            calendar_id: "primary".into(),
            provider_source: "google".into(),
            title: title.map(str::to_string),
            starts_at: starts,
            ends_at: ends,
            is_all_day: false,
            location: None,
            join_url: join_url.map(str::to_string),
            organizer_email: Some("boss@example.com".into()),
            recurrence_key: None,
            status,
        }
    }

    fn attendee(email: &str, response: Option<&str>) -> notewise_storage::Attendee {
        notewise_storage::Attendee {
            id: notewise_storage::Id::new(),
            calendar_event_id: notewise_storage::Id::new(),
            email: email.into(),
            display_name: None,
            response_status: response.map(str::to_string),
            is_organizer: false,
            person_id: None,
        }
    }

    #[test]
    fn an_event_happening_now_with_a_link_is_a_signal() {
        let now = at(0);
        let meeting = event(
            Some("Platform standup"),
            Some("https://meet.google.com/abc-defg-hij"),
            notewise_storage::EventStatus::Confirmed,
            now,
            now + Duration::minutes(30),
        );

        let signal = calendar_signal(&meeting, &[], &[], now).expect("a signal");
        assert_eq!(signal.source, SignalSource::Calendar);
        assert_eq!(signal.platform, Some(Platform::Meet));
        assert_eq!(
            signal.confidence,
            Confidence::Weak,
            "a calendar entry is a plan, not attendance"
        );
        assert_eq!(signal.title.as_deref(), Some("Platform standup"));
        assert!(!signal.declined);
        assert!(
            signal
                .external_ref
                .as_deref()
                .is_some_and(|r| r.starts_with("cal:")),
            "{signal:?}"
        );
    }

    /// A birthday, a focus block, a holiday. None of them is a meeting anybody joins.
    #[test]
    fn an_event_with_no_link_is_not_a_meeting() {
        let now = at(0);
        let blocked = event(
            Some("Focus time"),
            None,
            notewise_storage::EventStatus::Confirmed,
            now,
            now + Duration::hours(2),
        );
        assert!(calendar_signal(&blocked, &[], &[], now).is_none());

        let blank = event(
            Some("Focus time"),
            Some("   "),
            notewise_storage::EventStatus::Confirmed,
            now,
            now + Duration::hours(2),
        );
        assert!(calendar_signal(&blank, &[], &[], now).is_none());
    }

    #[test]
    fn a_cancelled_event_is_never_a_signal() {
        let now = at(0);
        let cancelled = event(
            Some("Cancelled review"),
            Some("https://zoom.us/j/123456789"),
            notewise_storage::EventStatus::Cancelled,
            now,
            now + Duration::minutes(30),
        );
        assert!(calendar_signal(&cancelled, &[], &[], now).is_none());
    }

    /// People join a minute or two before the hour, and an offer that arrives at exactly the start
    /// time arrives after the greeting.
    #[test]
    fn an_event_about_to_start_already_counts_and_one_long_past_does_not() {
        let start = at(60);
        let meeting = event(
            Some("Standup"),
            Some("https://meet.google.com/abc-defg-hij"),
            notewise_storage::EventStatus::Confirmed,
            start,
            start + Duration::minutes(30),
        );

        // Just before.
        assert!(calendar_signal(&meeting, &[], &[], start - Duration::minutes(1)).is_some());
        // Too early.
        assert!(calendar_signal(&meeting, &[], &[], start - Duration::minutes(10)).is_none());
        // Still running.
        assert!(calendar_signal(&meeting, &[], &[], start + Duration::minutes(20)).is_some());
        // Over.
        assert!(calendar_signal(&meeting, &[], &[], start + Duration::hours(2)).is_none());
    }

    /// The most annoying possible false positive.
    #[test]
    fn an_event_the_user_declined_is_marked_declined_and_the_tracker_refuses_it() {
        let now = at(0);
        let meeting = event(
            Some("Optional sync"),
            Some("https://meet.google.com/abc-defg-hij"),
            notewise_storage::EventStatus::Confirmed,
            now,
            now + Duration::minutes(30),
        );

        let attendees = [
            attendee("me@example.com", Some("declined")),
            attendee("someone@example.com", Some("accepted")),
        ];
        let mine = ["me@example.com".to_string()];

        let signal = calendar_signal(&meeting, &attendees, &mine, now).expect("a signal");
        assert!(signal.declined);

        // And it never becomes a notification, which is where the rule actually bites.
        let decision = JoinTracker::default().observe(&signal, now);
        assert!(!decision.notifies(), "{decision:?}");
    }

    /// Somebody *else* declining is not the user declining. Refusing every event because one
    /// attendee said no would silence the feature for anybody with a large invite list.
    #[test]
    fn another_attendee_declining_does_not_silence_it() {
        let now = at(0);
        let meeting = event(
            Some("All hands"),
            Some("https://meet.google.com/abc-defg-hij"),
            notewise_storage::EventStatus::Confirmed,
            now,
            now + Duration::minutes(30),
        );

        let attendees = [attendee("someone@example.com", Some("declined"))];
        let mine = ["me@example.com".to_string()];

        let signal = calendar_signal(&meeting, &attendees, &mine, now).expect("a signal");
        assert!(!signal.declined);
    }

    /// With no known address the check cannot be made, and it is skipped rather than guessed.
    #[test]
    fn with_no_identity_the_declined_check_is_skipped_rather_than_guessed() {
        let now = at(0);
        let meeting = event(
            Some("Optional sync"),
            Some("https://meet.google.com/abc-defg-hij"),
            notewise_storage::EventStatus::Confirmed,
            now,
            now + Duration::minutes(30),
        );

        let attendees = [attendee("me@example.com", Some("declined"))];
        let signal = calendar_signal(&meeting, &attendees, &[], now).expect("a signal");
        assert!(
            !signal.declined,
            "an unknown identity must not turn every decline into a silence"
        );
    }

    #[test]
    fn an_address_matches_regardless_of_case() {
        let now = at(0);
        let meeting = event(
            Some("Sync"),
            Some("https://meet.google.com/abc-defg-hij"),
            notewise_storage::EventStatus::Confirmed,
            now,
            now + Duration::minutes(30),
        );

        let attendees = [attendee("Me@Example.COM", Some("declined"))];
        let mine = ["me@example.com".to_string()];
        assert!(
            calendar_signal(&meeting, &attendees, &mine, now)
                .expect("a signal")
                .declined
        );
    }
}

use std::sync::Arc;

use axum::extract::State;
use axum::{routing::post, Json, Router as AxumRouter};
use notewise_storage::{NewNotification, NotificationChannel, NotificationRepository};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

type Shared = Arc<AppState>;

pub fn routes() -> AxumRouter<Shared> {
    AxumRouter::new()
        .route("/v1/signals/join", post(report_signal))
        .route("/v1/signals/join", axum::routing::get(detection_status))
        .route("/v1/signals/join/offers", axum::routing::get(list_offers))
        .route("/v1/signals/join/offers/:id/accept", post(accept_offer))
        .route(
            "/v1/signals/join/offers/:id",
            axum::routing::delete(dismiss_offer),
        )
}

#[derive(Debug, Deserialize)]
struct SignalBody {
    /// `extension`, `calendar`, or `native`.
    source: String,
    /// A meeting URL. The platform and the dedup key are both derived from it.
    ///
    /// Optional, and the extension does not send one: it sends `platform` and an opaque
    /// `external_id` instead, because the engine has no use for the link and posting data nothing
    /// reads is worth avoiding. A calendar poll has a URL and sends it.
    #[serde(default)]
    url: Option<String>,
    /// `meet`, `zoom`, or `teams`, when the producer knows without a URL to derive it from.
    #[serde(default)]
    platform: Option<String>,
    /// A calendar event's external id, when the producer has one instead of a URL.
    #[serde(default)]
    external_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    declined: bool,
    /// Whether this is a deliberate observation. Defaults to weak, so a producer that forgets to
    /// say gets the cautious treatment rather than an immediate notification.
    #[serde(default)]
    strong: bool,
}

#[derive(Debug, Serialize)]
struct SignalResponse {
    /// `notified`, `waiting`, or `silent`.
    outcome: &'static str,
    /// Why, when it stayed quiet. The extension logs this, which is what makes the rules debuggable
    /// from outside.
    reason: Option<String>,
    notification_id: Option<String>,
    /// The offer this created, which is what the interface acts on.
    offer_id: Option<String>,
}

/// Report that a meeting may have started.
///
/// Never starts a recording. It queues a notification the shell delivers and the user clicks — see
/// the module docs for why a false positive here must not be a recording nobody knew about.
async fn report_signal(
    State(state): State<Shared>,
    Json(body): Json<SignalBody>,
) -> ApiResult<Json<SignalResponse>> {
    let source = match body.source.trim() {
        "extension" => SignalSource::Extension,
        "calendar" => SignalSource::Calendar,
        "native" => SignalSource::Native,
        other => {
            return Err(ApiError::BadRequest(format!(
                "'{other}' is not a signal source; expected extension, calendar, or native"
            )))
        }
    };

    // An explicit platform wins over one derived from a URL: the producer that named it knows, and
    // a producer that sends neither gets `Unknown`, which reads as "a meeting" in the prompt.
    let platform = body
        .platform
        .as_deref()
        .and_then(Platform::parse)
        .or_else(|| body.url.as_deref().map(Platform::from_url));
    let signal = JoinSignal {
        source,
        platform,
        confidence: if body.strong {
            Confidence::Strong
        } else {
            Confidence::Weak
        },
        // A URL identifies a meeting more precisely than a calendar id when both are present, but
        // the calendar id survives a link being regenerated — so it wins.
        external_ref: body.external_id.clone().or_else(|| body.url.clone()),
        declined: body.declined,
        title: body.title.clone(),
        observed_at: Utc::now(),
    };

    let decision = state.join_tracker().await.observe(&signal, Utc::now());

    match decision {
        Decision::Notify { .. } => {
            let (notification_id, offer) = queue_offer(&state, &signal, &decision).await?;

            Ok(Json(SignalResponse {
                outcome: "notified",
                reason: None,
                notification_id: Some(notification_id.to_string()),
                offer_id: Some(offer.id.to_string()),
            }))
        }
        Decision::Waiting { until } => Ok(Json(SignalResponse {
            outcome: "waiting",
            reason: Some(format!("a single weak signal; will decide after {until}")),
            notification_id: None,
            offer_id: None,
        })),
        Decision::Silent { reason } => Ok(Json(SignalResponse {
            outcome: "silent",
            reason: Some(reason.to_string()),
            notification_id: None,
            offer_id: None,
        })),
    }
}

/// Queue the notification and record the offer for a decision that notified.
///
/// Shared by the HTTP intake and the calendar poll, so the two cannot disagree about what a
/// notification says or about what an offer carries. Called only for [`Decision::Notify`]; anything
/// else is a caller bug rather than a runtime condition, and it returns a conflict rather than
/// silently doing nothing.
async fn queue_offer(
    state: &Shared,
    signal: &JoinSignal,
    decision: &Decision,
) -> ApiResult<(Id, Offer)> {
    let Decision::Notify {
        key,
        title,
        platform,
    } = decision
    else {
        return Err(ApiError::Conflict(
            "there is nothing to offer for a decision that stayed silent".into(),
        ));
    };

    let described = title.clone().unwrap_or_else(|| match platform {
        Some(Platform::Meet) => "A Google Meet call".into(),
        Some(Platform::Zoom) => "A Zoom call".into(),
        Some(Platform::Teams) => "A Teams call".into(),
        _ => "A meeting".into(),
    });

    let queued = {
        let db = state.db().await;
        NotificationRepository::new(&db).create(NewNotification {
            source_kind: "join_offer".into(),
            // No meeting exists yet — that is the point, the user is being asked whether to make
            // one. A fresh id keeps the row well-formed without implying a link.
            source_id: Id::new(),
            recipient: "me".into(),
            channel: NotificationChannel::Desktop,
            body: format!("{described} seems to have started. Record it?"),
        })?
    };

    // The offer is what the interface shows and what a click turns into a labelled recording.
    // Recorded after the notification, so a queue that failed does not leave an offer whose
    // notification never existed.
    let offer = state.join_tracker().await.offer(
        key.clone(),
        title.clone(),
        *platform,
        signal.external_ref.clone(),
        Some(queued.id),
        Utc::now(),
    );

    Ok((queued.id, offer))
}

// ---------------------------------------------------------------- the calendar signal

/// How often to look at the calendar.
///
/// A minute is enough. The grace period is measured in minutes and the offer is worth ten, so
/// checking more often would only spend wakeups to notice something a few seconds sooner.
pub const CALENDAR_TICK: std::time::Duration = std::time::Duration::from_secs(60);

/// How early an event counts as happening.
///
/// People join a minute or two before the hour, and an offer that arrives at exactly the start time
/// arrives after the greeting.
pub const CALENDAR_LEAD_MINUTES: i64 = 2;

/// Whether a calendar event is worth a signal, and what kind.
///
/// Pure, which is the point: everything that decides whether the user is interrupted is testable
/// without a Google account. `None` means the event is not a meeting happening now, which is the
/// answer for almost every row in a calendar.
///
/// A declined event produces a signal *marked* declined rather than no signal at all. The tracker
/// then refuses it and says why, which means "why was I not told about this" has an answer instead
/// of the event silently never having been considered.
pub fn calendar_signal(
    event: &notewise_storage::CalendarEvent,
    attendees: &[notewise_storage::Attendee],
    mine: &[String],
    now: DateTime<Utc>,
) -> Option<JoinSignal> {
    // An event with no link is not a meeting anybody joins — a birthday, a focus block, a holiday.
    let join_url = event
        .join_url
        .as_deref()
        .filter(|url| !url.trim().is_empty())?;

    if event.status == notewise_storage::EventStatus::Cancelled {
        return None;
    }

    let starts = event.starts_at - Duration::minutes(CALENDAR_LEAD_MINUTES);
    if now < starts || now > event.ends_at {
        return None;
    }

    // Declined by the user, as opposed to by somebody else. Comparing against the addresses the
    // connected accounts recorded is the only identity this engine has; without one the check
    // cannot be made, and it is skipped rather than guessed — refusing every event because one
    // attendee declined would silence the feature for anybody with a large invite list.
    let declined = attendees.iter().any(|attendee| {
        matches!(
            attendee.response_status.as_deref(),
            Some("declined") | Some("DECLINED")
        ) && mine
            .iter()
            .any(|address| address.eq_ignore_ascii_case(&attendee.email))
    });

    Some(JoinSignal {
        source: SignalSource::Calendar,
        platform: Some(Platform::from_url(join_url)),
        // A calendar entry is a plan, not attendance. On its own it waits out the grace period.
        confidence: Confidence::Weak,
        // The event's own id rather than its URL: a link can be regenerated, and this is what makes
        // the resulting recording linkable to the event directly instead of inferred from time
        // overlap by a later reconciliation pass. Prefixed so the shape says where it came from —
        // the extension's opaque key is deliberately not one of these.
        external_ref: Some(format!("cal:{}", event.id)),
        declined,
        title: event.title.clone(),
        observed_at: now,
    })
}

/// The addresses that are the user's, as far as this workspace knows.
///
/// The connected accounts' labels. Empty when nothing is connected, which turns the declined check
/// off rather than making it wrong.
fn my_addresses(db: &notewise_storage::Database) -> Vec<String> {
    notewise_storage::ConnectorAccountRepository::new(db)
        .list()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|account| account.account_label)
        .filter(|label| label.contains('@'))
        .collect()
}

/// One pass over the calendar. Separate from the loop so it is callable directly.
///
/// Returns how many offers it made, which is what makes it testable without a clock to wind.
pub async fn tick(state: &Shared) -> ApiResult<usize> {
    let now = Utc::now();

    let signals = {
        let db = state.db().await;
        let repo = notewise_storage::CalendarRepository::new(&db);
        let mine = my_addresses(&db);

        // A window wide enough to catch a long meeting that is already running, not just one
        // starting now: the app may have been opened half an hour into a call.
        let events = repo.between(now - Duration::hours(6), now + Duration::hours(1))?;

        let mut out = Vec::new();
        for event in events {
            let attendees = repo.attendees(event.id).unwrap_or_default();
            if let Some(signal) = calendar_signal(&event, &attendees, &mine, now) {
                out.push(signal);
            }
        }
        out
    };

    let mut offered = 0;
    for signal in signals {
        let decision = state.join_tracker().await.observe(&signal, now);
        if decision.notifies() {
            match queue_offer(state, &signal, &decision).await {
                Ok(_) => offered += 1,
                // A failed notification is logged and dropped. A missed prompt is recoverable by
                // pressing record; a retry loop is not recoverable by anything.
                Err(e) => tracing::warn!(error = %e, "could not offer a calendar meeting"),
            }
        }
    }

    Ok(offered)
}

/// Watch the calendar for meetings starting.
///
/// Started with the server rather than with the router, for the same reason the scheduler is: `app`
/// is also called by tests and by an embedder that only wants the route table, and neither wants a
/// background loop.
pub fn spawn(state: Shared) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(CALENDAR_TICK).await;
            if let Err(e) = tick(&state).await {
                // A watcher that dies on one bad tick stops noticing meetings, silently.
                tracing::warn!(error = %e, "a calendar detection tick failed; continuing");
            }
        }
    });
}

#[derive(Debug, Serialize)]
struct SourceStatus {
    source: &'static str,
    /// When this source last reported anything. `null` means it has never been heard from, which for
    /// the extension is the answer to "why is nothing happening".
    last_seen_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct DetectionStatus {
    sources: Vec<SourceStatus>,
    /// Offers waiting to be answered.
    offers: usize,
    /// Whether a calendar connector is connected, which is the other half of detection.
    calendar_connected: bool,
    /// How long a lone calendar signal waits before it is worth mentioning, in seconds.
    grace_secs: i64,
    /// What this cannot see, stated rather than left to be discovered.
    ///
    /// The design excludes native process and audio-session detection from the first cut, which
    /// means a user who only ever takes calls in the desktop Zoom or Teams client and keeps no
    /// calendar gets nothing. That is the most likely reason somebody finds this underwhelming, and
    /// it belongs on the screen rather than in a design document.
    blind_spot: &'static str,
}

/// What detection can currently see.
async fn detection_status(State(state): State<Shared>) -> ApiResult<Json<DetectionStatus>> {
    let now = Utc::now();

    let (seen, offers) = {
        let mut tracker = state.join_tracker().await;
        (tracker.last_seen(), tracker.live_offers(now).len())
    };

    let calendar_connected = {
        let db = state.db().await;
        notewise_storage::ConnectorAccountRepository::new(&db)
            .list()
            .map(|accounts| {
                accounts.iter().any(|account| {
                    account.status == notewise_storage::AccountStatus::Connected
                        && (account.connector_id.contains("google")
                            || account.connector_id.contains("microsoft"))
                })
            })
            .unwrap_or(false)
    };

    let described = |source: SignalSource| -> SourceStatus {
        SourceStatus {
            source: match source {
                SignalSource::Extension => "extension",
                SignalSource::Calendar => "calendar",
                SignalSource::Native => "native",
            },
            last_seen_at: seen
                .iter()
                .find(|(candidate, _)| *candidate == source)
                .map(|(_, at)| at.to_rfc3339()),
        }
    };

    Ok(Json(DetectionStatus {
        sources: vec![
            described(SignalSource::Extension),
            described(SignalSource::Calendar),
        ],
        offers,
        calendar_connected,
        grace_secs: TrackerConfig::default().grace.num_seconds(),
        blind_spot: "Meetings taken only in the desktop Zoom or Teams app, with nothing on your \
                     calendar, are not detected — that needs to watch what is running on your \
                     machine, which Notewise does not do.",
    }))
}

#[derive(Debug, Serialize)]
struct OfferBody {
    id: String,
    /// What the recording would be called.
    title: String,
    platform: &'static str,
    /// The notification queued for this, so an interface showing the offer itself can mark it
    /// delivered rather than letting the user be told the same thing twice.
    notification_id: Option<String>,
    created_at: String,
    /// Seconds until this stops being worth offering.
    expires_in_secs: i64,
}

fn describe_offer(offer: &Offer, now: DateTime<Utc>) -> OfferBody {
    OfferBody {
        id: offer.id.to_string(),
        title: offer.suggested_title(now),
        platform: match offer.platform {
            Some(Platform::Meet) => "meet",
            Some(Platform::Zoom) => "zoom",
            Some(Platform::Teams) => "teams",
            _ => "unknown",
        },
        notification_id: offer.notification_id.map(|id| id.to_string()),
        created_at: offer.created_at.to_rfc3339(),
        expires_in_secs: (offer.created_at + Duration::minutes(OFFER_TTL_MINUTES) - now)
            .num_seconds()
            .max(0),
    }
}

/// Meetings that appear to have started and have not been answered.
///
/// The interface polls this. An empty list is the normal state and not worth an error.
async fn list_offers(State(state): State<Shared>) -> Json<Vec<OfferBody>> {
    let now = Utc::now();
    let offers = state.join_tracker().await.live_offers(now);

    Json(offers.iter().map(|o| describe_offer(o, now)).collect())
}

#[derive(Debug, Serialize)]
struct AcceptedOffer {
    /// What to call the recording. The caller starts it — this endpoint does not.
    title: String,
    /// The calendar event this belongs to, when the signal came from one.
    ///
    /// A link that is *known* should never be guessed, and this is the one path where it is known —
    /// so it comes back here rather than being left for the reconciliation pass to infer from time
    /// overlap.
    calendar_event_id: Option<String>,
}

/// Take an offer up.
///
/// Deliberately does *not* start recording. Capture needs a device, a model, and a build that has
/// both, and all of that already lives behind `POST /v1/recording` with its own errors and its own
/// feature flags. Duplicating it here would mean two paths into the microphone and two places to get
/// the permission handling wrong. So this answers "what would you call it" and the caller starts the
/// recording it was already able to start.
///
/// The offer is dismissed either way: an offer taken up is answered.
async fn accept_offer(
    State(state): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<Json<AcceptedOffer>> {
    let id: Id = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("'{id}' is not an id")))?;

    let now = Utc::now();
    let mut tracker = state.join_tracker().await;

    let offer = tracker
        .offer_by_id(id)
        .cloned()
        .ok_or_else(|| ApiError::NotFound("that offer has expired".into()))?;

    tracker.dismiss(id);

    Ok(Json(AcceptedOffer {
        title: offer.suggested_title(now),
        // Only a calendar reference, never the extension's opaque key: the point of it is to link
        // the recording to a real event, and a digest of a meeting URL links to nothing.
        calendar_event_id: offer
            .external_ref
            .as_deref()
            .and_then(|reference| reference.strip_prefix("cal:"))
            .map(str::to_string),
    }))
}

/// Put an offer away without recording.
///
/// Answers `dismissed: false` for one that had already expired, which is not an error — the user
/// clicking a stale card should see it disappear either way.
async fn dismiss_offer(
    State(state): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let id: Id = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("'{id}' is not an id")))?;

    let dismissed = state.join_tracker().await.dismiss(id);
    Ok(Json(serde_json::json!({ "dismissed": dismissed })))
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use notewise_ai_router::{Router as AiRouter, RouterConfig};
    use notewise_storage::Database;
    use tower::ServiceExt;

    fn app() -> AxumRouter<()> {
        let state = AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        );
        routes().with_state(Arc::new(state))
    }

    /// One request, with the status and whatever body came back.
    async fn call(app: &AxumRouter<()>, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = app.clone().oneshot(request).await.expect("call");
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        (status, json)
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("builds")
    }

    fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("builds")
    }

    async fn post_signal(
        app: &AxumRouter<()>,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/signals/join")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request");

        let response = app.clone().oneshot(request).await.expect("call");
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn a_strong_extension_signal_queues_a_notification() {
        let (status, body) = post_signal(
            &app(),
            serde_json::json!({
                "source": "extension",
                "url": "https://meet.google.com/abc-defg-hij",
                "title": "Platform standup",
                "strong": true
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["outcome"], "notified");
        assert!(body["notification_id"].is_string());
    }

    #[tokio::test]
    async fn a_lone_calendar_signal_waits_and_says_so() {
        let (status, body) = post_signal(
            &app(),
            serde_json::json!({ "source": "calendar", "external_id": "evt-1" }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["outcome"], "waiting");
        assert!(
            body["reason"].is_string(),
            "the extension logs this, which is what makes the rules debuggable from outside"
        );
    }

    #[tokio::test]
    async fn a_declined_meeting_stays_silent() {
        let (_, body) = post_signal(
            &app(),
            serde_json::json!({
                "source": "extension",
                "external_id": "evt-1",
                "declined": true,
                "strong": true
            }),
        )
        .await;

        assert_eq!(body["outcome"], "silent");
        assert!(body["reason"].as_str().unwrap().contains("declined"));
    }

    #[tokio::test]
    async fn the_same_meeting_is_not_queued_twice() {
        let app = app();
        let payload = serde_json::json!({
            "source": "extension",
            "url": "https://meet.google.com/abc",
            "strong": true
        });

        let (_, first) = post_signal(&app, payload.clone()).await;
        let (_, second) = post_signal(&app, payload).await;

        assert_eq!(first["outcome"], "notified");
        assert_eq!(
            second["outcome"], "silent",
            "three notifications for one standup and the feature gets turned off"
        );
    }

    /// A producer that forgets to say gets the cautious treatment.
    #[tokio::test]
    async fn a_signal_that_does_not_claim_strength_is_treated_as_weak() {
        let (_, body) = post_signal(
            &app(),
            serde_json::json!({ "source": "extension", "external_id": "evt-1" }),
        )
        .await;

        assert_eq!(body["outcome"], "waiting");
    }

    #[tokio::test]
    async fn an_unknown_source_is_refused() {
        let (status, _) = post_signal(
            &app(),
            serde_json::json!({ "source": "telepathy", "strong": true }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_platform_is_named_when_there_is_no_title() {
        let state = Arc::new(AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        ));
        let app = routes().with_state(Arc::clone(&state));

        post_signal(
            &app,
            serde_json::json!({
                "source": "extension",
                "url": "https://acme.zoom.us/j/1",
                "strong": true
            }),
        )
        .await;

        let db = state.db().await;
        let pending = NotificationRepository::new(&db)
            .pending_on(NotificationChannel::Desktop)
            .expect("pending");

        assert_eq!(pending.len(), 1);
        assert!(pending[0].body.contains("Zoom"), "{}", pending[0].body);
        assert!(
            pending[0].body.contains("Record it?"),
            "{}",
            pending[0].body
        );
    }
    /// "Why is nothing happening" needs an answer the person who asked can see.
    #[tokio::test]
    async fn the_status_says_which_sources_have_been_heard_from() {
        let app = app();

        let (status, before) = call(&app, get("/v1/signals/join")).await;
        assert_eq!(status, StatusCode::OK);
        let sources = before["sources"].as_array().expect("a list");
        assert_eq!(sources.len(), 2);
        assert!(
            sources
                .iter()
                .all(|s| s["last_seen_at"] == serde_json::Value::Null),
            "nothing has reported yet: {before}"
        );
        assert_eq!(before["calendar_connected"], false);
        assert!(before["blind_spot"]
            .as_str()
            .expect("a sentence")
            .contains("desktop Zoom"));

        post_signal(
            &app,
            serde_json::json!({ "source": "extension", "platform": "meet", "external_id": "x:abc", "strong": true }),
        )
        .await;

        let (_, after) = call(&app, get("/v1/signals/join")).await;
        let extension = after["sources"]
            .as_array()
            .expect("a list")
            .iter()
            .find(|s| s["source"] == "extension")
            .expect("the extension is listed");
        assert!(extension["last_seen_at"].is_string(), "{after}");
        assert_eq!(after["offers"], 1);
    }

    /// A declined meeting still proves the extension is talking to us.
    #[tokio::test]
    async fn a_silenced_signal_still_counts_as_the_source_being_alive() {
        let app = app();

        post_signal(
            &app,
            serde_json::json!({
                "source": "calendar",
                "platform": "meet",
                "external_id": "cal:1",
                "declined": true
            }),
        )
        .await;

        let (_, status) = call(&app, get("/v1/signals/join")).await;
        let calendar = status["sources"]
            .as_array()
            .expect("a list")
            .iter()
            .find(|s| s["source"] == "calendar")
            .expect("listed");
        assert!(calendar["last_seen_at"].is_string(), "{status}");
        assert_eq!(status["offers"], 0, "and it produced no offer");
    }

    /// The whole path the interface uses: an offer appears, is named, and is answered.
    #[tokio::test]
    async fn an_offer_can_be_accepted_and_comes_back_named() {
        let app = app();

        let (_, signalled) = post_signal(
            &app,
            serde_json::json!({
                "source": "extension",
                "platform": "zoom",
                "external_id": "x:abc",
                "strong": true
            }),
        )
        .await;
        let id = signalled["offer_id"]
            .as_str()
            .expect("an offer")
            .to_string();

        let (status, offers) = call(&app, get("/v1/signals/join/offers")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(offers.as_array().expect("a list").len(), 1);
        assert_eq!(offers[0]["platform"], "zoom");
        assert_eq!(offers[0]["title"], "Zoom call");
        assert!(offers[0]["notification_id"].is_string(), "{offers}");
        assert!(offers[0]["expires_in_secs"].as_i64().expect("a number") > 0);

        let (status, accepted) = call(
            &app,
            post(
                &format!("/v1/signals/join/offers/{id}/accept"),
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{accepted}");
        assert_eq!(accepted["title"], "Zoom call");
        assert_eq!(
            accepted["calendar_event_id"],
            serde_json::Value::Null,
            "the extension's opaque key links to nothing, so it must not be offered as a link"
        );

        // Answered, so it is gone.
        let (_, after) = call(&app, get("/v1/signals/join/offers")).await;
        assert!(after.as_array().expect("a list").is_empty(), "{after}");
    }

    /// A link that is *known* should never be guessed, and a calendar signal is the one path where
    /// it is known.
    #[tokio::test]
    async fn a_calendar_offer_carries_the_event_it_came_from() {
        let app = app();
        let event_id = notewise_storage::Id::new().to_string();

        // Two weak observations, so it notifies without waiting out the grace period.
        for source in ["calendar", "native"] {
            post_signal(
                &app,
                serde_json::json!({
                    "source": source,
                    "platform": "meet",
                    "external_id": format!("cal:{event_id}"),
                    "title": "Q3 planning"
                }),
            )
            .await;
        }

        let (_, offers) = call(&app, get("/v1/signals/join/offers")).await;
        let id = offers[0]["id"].as_str().expect("an offer");
        assert_eq!(offers[0]["title"], "Q3 planning");

        let (_, accepted) = call(
            &app,
            post(
                &format!("/v1/signals/join/offers/{id}/accept"),
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(accepted["calendar_event_id"], event_id);
    }

    #[tokio::test]
    async fn an_offer_can_be_dismissed_and_an_unknown_one_says_so() {
        let app = app();
        let (_, signalled) = post_signal(
            &app,
            serde_json::json!({
                "source": "extension",
                "platform": "teams",
                "external_id": "x:abc",
                "strong": true
            }),
        )
        .await;
        let id = signalled["offer_id"]
            .as_str()
            .expect("an offer")
            .to_string();

        let (status, body) = call(
            &app,
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/signals/join/offers/{id}"))
                .body(Body::empty())
                .expect("builds"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["dismissed"], true);

        assert!(call(&app, get("/v1/signals/join/offers"))
            .await
            .1
            .as_array()
            .expect("a list")
            .is_empty());
    }

    #[tokio::test]
    async fn accepting_an_offer_that_expired_is_a_404_rather_than_a_crash() {
        let app = app();
        let (status, _) = call(
            &app,
            post(
                &format!(
                    "/v1/signals/join/offers/{}/accept",
                    notewise_storage::Id::new()
                ),
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_malformed_offer_id_is_a_400() {
        let app = app();
        let (status, _) = call(
            &app,
            post(
                "/v1/signals/join/offers/not-an-id/accept",
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// The calendar poll, end to end against a stored event.
    #[tokio::test]
    async fn the_calendar_poll_offers_a_meeting_happening_now() {
        let state = Arc::new(AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        ));

        let now = Utc::now();
        {
            let db = state.db().await;
            let external = notewise_storage::ExternalItemRepository::new(&db)
                .upsert(notewise_storage::NewExternalItem {
                    connector_id: "google".into(),
                    external_id: "evt-1".into(),
                    url: None,
                    title: Some("Platform standup".into()),
                    remote_version: None,
                })
                .expect("an external item");

            notewise_storage::CalendarRepository::new(&db)
                .upsert(notewise_storage::NewCalendarEvent {
                    external_item_id: external.id,
                    calendar_id: "primary".into(),
                    provider_source: "google".into(),
                    title: Some("Platform standup".into()),
                    starts_at: now - Duration::minutes(1),
                    ends_at: now + Duration::minutes(25),
                    is_all_day: false,
                    location: None,
                    join_url: Some("https://meet.google.com/abc-defg-hij".into()),
                    organizer_email: None,
                    recurrence_key: None,
                    status: notewise_storage::EventStatus::Confirmed,
                })
                .expect("an event");
        }

        // A lone weak signal waits, which is the whole point of the grace period.
        assert_eq!(tick(&state).await.expect("a tick"), 0);

        // Seen again after the grace period, it speaks. The tracker's clock is real, so this drives
        // it by observing the same signal at a later time rather than by sleeping two minutes.
        let signal = {
            let db = state.db().await;
            let repo = notewise_storage::CalendarRepository::new(&db);
            let events = repo
                .between(now - Duration::hours(1), now + Duration::hours(1))
                .expect("events");
            calendar_signal(&events[0], &[], &[], now).expect("a signal")
        };

        let later = now + Duration::minutes(5);
        let decision = state.join_tracker().await.observe(&signal, later);
        assert!(decision.notifies(), "{decision:?}");
    }
}
