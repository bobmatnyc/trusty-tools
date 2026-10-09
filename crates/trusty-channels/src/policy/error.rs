//! Why a [`ChannelPolicy`](crate::policy::ChannelPolicy) refused to build.
//!
//! Why: a policy that drops a bad route and keeps the rest would hide the
//! mistake; every load rule fails the whole build with a matchable error
//! (#8454 plan §4).
//! What: [`PolicyError`]. An `entry` names the input route as
//! `routes[<index>] <channel> "<name>"`, or `rate_limit` for the default.
//! Test: `overlapping_routes_fail_build_and_name_both`,
//! `bucket_zero_or_nan_params_fail_build`.

use crate::policy::types::{Channel, MessageKind};

/// A load rule the route data broke. The policy is not built.
///
/// Why: callers (S2's loader, `tm doctor`) report which rule and which entry.
/// What: a duplicate names both entries; no variant carries a credential.
/// Test: `overlapping_routes_fail_build_and_name_both`,
/// `empty_kinds_fails_build`, `review_notice_on_slack_or_telegram_fails_build`,
/// `invalid_recipient_or_name_fails_build`,
/// `bucket_zero_or_nan_params_fail_build`, `route_cannot_raise_rate_limit`.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PolicyError {
    /// Two routes share a name, or a (channel, recipient) pair.
    #[error("{first} and {second} share the {field} {value:?}")]
    Duplicate {
        /// The earlier entry.
        first: String,
        /// The later entry.
        second: String,
        /// `name` or `recipient`.
        field: &'static str,
        /// The shared value (the recipient in normalized form).
        value: String,
    },
    /// A route lists no kinds.
    #[error("{entry}: kinds is empty")]
    EmptyKinds {
        /// The route entry.
        entry: String,
    },
    /// A route lists a kind its channel does not carry.
    #[error("{entry}: kind {kind} is not carried on {channel}")]
    KindNotOnChannel {
        /// The route entry.
        entry: String,
        /// The refused kind.
        kind: MessageKind,
        /// The route's channel.
        channel: Channel,
    },
    /// A route's name or recipient is malformed.
    #[error("{entry}: {reason}")]
    InvalidRoute {
        /// The route entry.
        entry: String,
        /// What is wrong.
        reason: String,
    },
    /// A rate-limit parameter is zero, negative, NaN or out of range.
    #[error("{entry}: rate limit {reason}")]
    InvalidRateLimit {
        /// `rate_limit`, or the route entry.
        entry: String,
        /// What is wrong.
        reason: String,
    },
    /// A route sets a rate limit above the default.
    #[error("{entry}: rate limit {field} {route} is above the default {default}")]
    RateLimitRaised {
        /// The route entry.
        entry: String,
        /// `capacity` or `refill_per_sec`.
        field: &'static str,
        /// The route's value.
        route: f64,
        /// The default's value.
        default: f64,
    },
}
