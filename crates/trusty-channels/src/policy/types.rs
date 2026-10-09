//! Policy value types: channels, message kinds, rate limits and routes.
//!
//! Why: the route table, the checks and the rate limiter share one
//! vocabulary across gchat, Slack and Telegram (#8454 S1).
//! What: [`Channel`], [`MessageKind`], the already-parsed input specs
//! ([`RouteSpec`], [`RateLimitSpec`]) and their validated forms ([`Route`],
//! [`RateLimit`]). A validated value is built only by
//! [`ChannelPolicy::build`](crate::policy::ChannelPolicy::build).
//! Test: `src/policy/tests/build.rs`.

use std::collections::BTreeSet;
use std::fmt;
use std::time::Duration;

/// A chat channel a route can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Channel {
    /// Google Chat; a recipient is an email address.
    Gchat,
    /// Slack; a recipient is a user ID (`U…` or `W…`).
    Slack,
    /// Telegram; a recipient is a private chat id (a positive integer).
    Telegram,
}

impl Channel {
    /// Every channel, in declaration order.
    pub const ALL: [Channel; 3] = [Channel::Gchat, Channel::Slack, Channel::Telegram];

    /// The schema spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gchat => "gchat",
            Self::Slack => "slack",
            Self::Telegram => "telegram",
        }
    }

    /// True when a route on this channel may list `kind`.
    ///
    /// Why: `review_notice` is a gchat product feature; Slack and Telegram
    /// carry questions only (#8454 Architect Q3).
    /// What: `question` on every channel, `review_notice` on gchat only.
    /// Test: `review_notice_on_slack_or_telegram_fails_build`.
    pub fn allows_kind(self, kind: MessageKind) -> bool {
        match kind {
            MessageKind::Question => true,
            MessageKind::ReviewNotice => self == Self::Gchat,
        }
    }

    /// The form a recipient or sender is compared in.
    ///
    /// Why: a Chat email is case-insensitive (gchat's existing rule); a Slack
    /// or Telegram id is compared byte for byte.
    /// What: gchat trims and ASCII-lowercases; the others return `s` as is.
    /// Test: `exact_route_and_listed_kind_allow`, `unknown_route_denies`.
    pub(crate) fn normalize(self, s: &str) -> String {
        match self {
            Self::Gchat => s.trim().to_ascii_lowercase(),
            Self::Slack | Self::Telegram => s.to_string(),
        }
    }

    /// Check a route recipient's shape for this channel.
    ///
    /// Why: a route must name one person, never a channel, a group or a
    /// `@username` that can be re-registered (#8454 plan §3).
    /// What: gchat — an email; Slack — a user ID, `U` or `W` then upper-case
    /// ASCII letters and digits; Telegram — a positive decimal chat id with
    /// no sign and no leading zero. Returns the reason on failure.
    /// Test: `invalid_recipient_or_name_fails_build`.
    pub(crate) fn check_recipient(self, s: &str) -> Result<(), String> {
        let ok = match self {
            Self::Gchat => crate::gchat::routes::is_email(s),
            Self::Slack => {
                let mut chars = s.chars();
                matches!(chars.next(), Some('U' | 'W'))
                    && s.len() >= 2
                    && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            }
            Self::Telegram => {
                !s.starts_with('0')
                    && s.chars().all(|c| c.is_ascii_digit())
                    && s.parse::<i64>().is_ok_and(|id| id > 0)
            }
        };
        if ok {
            return Ok(());
        }
        let expected = match self {
            Self::Gchat => "an email address",
            Self::Slack => "a Slack user ID (U… or W…)",
            Self::Telegram => "a positive Telegram chat id",
        };
        Err(format!("recipient {s:?} is not {expected}"))
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A message kind a route can allow.
///
/// Why: separate from `gchat::MessageKind` until gchat adopts this module
/// (#8454 S3); the spellings match.
/// What: `question` (a reply binds to its id) and `review_notice` (gchat
/// only, no reply expected).
/// Test: `kind_not_listed_denies`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MessageKind {
    /// A question; a reply binds to its question id.
    Question,
    /// A notice that work waits for review; no reply expected.
    ReviewNotice,
}

impl MessageKind {
    /// Every kind, in schema order.
    pub const ALL: [MessageKind; 2] = [MessageKind::Question, MessageKind::ReviewNotice];

    /// The schema spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Question => "question",
            Self::ReviewNotice => "review_notice",
        }
    }
}

impl fmt::Display for MessageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The largest `limit` a rate limit may set. It also bounds each window's
/// admit log, so one binding holds at most this many timestamps.
pub const MAX_LIMIT: u32 = 10_000;
/// The longest `window_secs` a rate limit may set: one day.
///
/// Why: a longer window would hold an admit against a sender for days, and
/// no channel use needs it; the log size is bounded by [`MAX_LIMIT`] either way.
pub const MAX_WINDOW_SECS: u32 = 86_400;

/// Rate-limit parameters as parsed, before validation.
///
/// Why: a parser hands over what the file says, including a zero or negative
/// value, so validation can refuse it rather than the type hiding it.
/// What: at most `limit` admits in any window of `window_secs` seconds.
/// Test: `bucket_zero_or_out_of_range_params_fail_build`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitSpec {
    /// Admits allowed per window; valid range `1..=10_000`.
    pub limit: i64,
    /// Window length in seconds; valid range `1..=86_400`.
    pub window_secs: i64,
}

/// A validated rate limit: at most `limit` admits in any `window_secs`.
///
/// Why: the window must never run with a zero, negative or unbounded
/// parameter, so the only way to hold one is through validation.
/// What: `limit` in `1..=MAX_LIMIT`, `window_secs` in `1..=MAX_WINDOW_SECS`.
/// [`RateLimit::DEFAULT`] is 100 admits per 60 s (#7457).
/// Test: `bucket_zero_or_out_of_range_params_fail_build`,
/// `absent_rate_limit_uses_builtin_not_unlimited`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    limit: u32,
    window_secs: u32,
}

impl RateLimit {
    /// The built-in default: 100 admits in any 60 s.
    pub const DEFAULT: RateLimit = RateLimit {
        limit: 100,
        window_secs: 60,
    };

    /// Admits allowed in any one window.
    pub fn limit(self) -> u32 {
        self.limit
    }

    /// The window length in seconds.
    pub fn window_secs(self) -> u32 {
        self.window_secs
    }

    /// The window length.
    pub fn window(self) -> Duration {
        Duration::from_secs(u64::from(self.window_secs))
    }

    /// Validate a parsed spec; the error is the reason text.
    pub(crate) fn from_spec(spec: RateLimitSpec) -> Result<Self, String> {
        let in_range = |v: i64, max: u32| u32::try_from(v).ok().filter(|v| (1..=max).contains(v));
        let limit = in_range(spec.limit, MAX_LIMIT)
            .ok_or_else(|| format!("limit {} is outside 1..={MAX_LIMIT}", spec.limit))?;
        let window_secs = in_range(spec.window_secs, MAX_WINDOW_SECS).ok_or_else(|| {
            format!(
                "window_secs {} is outside 1..={MAX_WINDOW_SECS}",
                spec.window_secs
            )
        })?;
        Ok(Self { limit, window_secs })
    }

    /// The first parameter in which `self` is looser than `default`, as
    /// `(field, self's value, default's value)`; `None` when it is not.
    ///
    /// Why: a route may lower the default, never raise it (#8454 Q6). A raise
    /// is defined so that a route never admits more than the default in ANY
    /// window: the route's `limit` must not exceed the default's, and its
    /// `window_secs` must not be shorter. A pure admits-per-second compare
    /// was rejected: 200 per 240 s is a lower rate than 100 per 60 s but
    /// admits 200 in one burst. Requiring the same window was rejected as
    /// needlessly strict: 100 per 120 s is strictly tighter.
    /// What: `limit` first, then `window_secs`; an equal limit is no raise.
    /// Test: `route_cannot_raise_rate_limit`, `equal_route_limit_is_not_a_raise`.
    pub(crate) fn looser_than(self, default: RateLimit) -> Option<(&'static str, u32, u32)> {
        if self.limit > default.limit {
            return Some(("limit", self.limit, default.limit));
        }
        if self.window_secs < default.window_secs {
            return Some(("window_secs", self.window_secs, default.window_secs));
        }
        None
    }
}

/// One route as parsed, before validation.
///
/// Why: the input to [`ChannelPolicy::build`](crate::policy::ChannelPolicy::build);
/// S1 takes routes already parsed, the file loader is S2.
/// What: plain data; nothing is checked until the policy is built.
/// Test: `overlapping_routes_fail_build_and_name_both`.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteSpec {
    /// The channel this route sends on.
    pub channel: Channel,
    /// Unique route name (`[A-Za-z0-9_-]+`); the bucket's binding id.
    pub name: String,
    /// The one recipient this route names.
    pub recipient: String,
    /// Allowed kinds; must not be empty.
    pub kinds: Vec<MessageKind>,
    /// A per-route limit; may lower the default, never raise it.
    pub rate_limit: Option<RateLimitSpec>,
}

/// One validated route.
///
/// Why: fields are private so a route reaches a check or a bucket only
/// through [`ChannelPolicy::build`](crate::policy::ChannelPolicy::build).
/// What: name, channel, normalized recipient, a non-empty kind set and the
/// effective rate limit.
/// Test: `exact_route_and_listed_kind_allow`, `route_cannot_raise_rate_limit`.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub(crate) name: String,
    pub(crate) channel: Channel,
    pub(crate) recipient: String,
    pub(crate) kinds: BTreeSet<MessageKind>,
    pub(crate) rate_limit: RateLimit,
}

impl Route {
    /// The route name, also its bucket binding id.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The channel this route sends on.
    pub fn channel(&self) -> Channel {
        self.channel
    }

    /// The normalized recipient.
    pub fn recipient(&self) -> &str {
        &self.recipient
    }

    /// The allowed kinds; never empty.
    pub fn kinds(&self) -> &BTreeSet<MessageKind> {
        &self.kinds
    }

    /// True when this route allows `kind`.
    pub fn allows(&self, kind: MessageKind) -> bool {
        self.kinds.contains(&kind)
    }

    /// The effective rate limit: the route's own, else the policy default.
    pub fn rate_limit(&self) -> RateLimit {
        self.rate_limit
    }
}
