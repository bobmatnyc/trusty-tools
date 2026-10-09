//! Policy value types: channels, message kinds, rate limits and routes.
//!
//! Why: the route table, the checks and the token bucket share one
//! vocabulary across gchat, Slack and Telegram (#8454 S1).
//! What: [`Channel`], [`MessageKind`], the already-parsed input specs
//! ([`RouteSpec`], [`RateLimitSpec`]) and their validated forms ([`Route`],
//! [`RateLimit`]). A validated value is built only by
//! [`ChannelPolicy::build`](crate::policy::ChannelPolicy::build).
//! Test: `src/policy/tests/build.rs`.

use std::collections::BTreeSet;
use std::fmt;

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

/// The largest bucket capacity a rate limit may set.
pub const MAX_CAPACITY: u32 = 10_000;
/// The largest refill rate a rate limit may set, in tokens per second.
pub const MAX_REFILL_PER_SEC: f64 = 10_000.0;

/// Rate-limit parameters as parsed, before validation.
///
/// Why: a parser hands over what the file says, including a negative or NaN
/// value, so validation can refuse it rather than the type hiding it.
/// What: `capacity` in tokens; `refill_per_sec` in tokens per second.
/// Test: `bucket_zero_or_nan_params_fail_build`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateLimitSpec {
    /// Bucket capacity in whole tokens; valid range `1..=10_000`.
    pub capacity: i64,
    /// Refill rate in tokens per second; valid range `(0, 10_000]`.
    pub refill_per_sec: f64,
}

/// A validated rate limit.
///
/// Why: the bucket must never run with a zero, negative, NaN or unbounded
/// parameter, so the only way to hold one is through validation.
/// What: capacity `1..=MAX_CAPACITY`, refill rate finite in
/// `(0, MAX_REFILL_PER_SEC]`. [`RateLimit::DEFAULT`] is 100 tokens refilled
/// at 100 per 60 s (#7457).
/// Test: `bucket_zero_or_nan_params_fail_build`,
/// `absent_rate_limit_uses_builtin_not_unlimited`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateLimit {
    capacity: u32,
    refill_per_sec: f64,
}

impl RateLimit {
    /// The built-in default: capacity 100, refill 100 per 60 s.
    pub const DEFAULT: RateLimit = RateLimit {
        capacity: 100,
        refill_per_sec: 100.0 / 60.0,
    };

    /// Bucket capacity in tokens.
    pub fn capacity(self) -> u32 {
        self.capacity
    }

    /// Refill rate in tokens per second.
    pub fn refill_per_sec(self) -> f64 {
        self.refill_per_sec
    }

    /// Validate a parsed spec; the error is the reason text.
    pub(crate) fn from_spec(spec: RateLimitSpec) -> Result<Self, String> {
        let capacity = u32::try_from(spec.capacity)
            .ok()
            .filter(|c| (1..=MAX_CAPACITY).contains(c))
            .ok_or_else(|| format!("capacity {} is outside 1..={MAX_CAPACITY}", spec.capacity))?;
        let r = spec.refill_per_sec;
        // #8454: NaN fails every comparison, so test the accepted range.
        if !(r > 0.0 && r <= MAX_REFILL_PER_SEC) {
            return Err(format!(
                "refill_per_sec {r} is outside (0, {MAX_REFILL_PER_SEC}]"
            ));
        }
        Ok(Self {
            capacity,
            refill_per_sec: r,
        })
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
