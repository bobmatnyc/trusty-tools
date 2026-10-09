//! Channel policy: who each chat channel may message, with which kinds,
//! which inbound messages may answer an open question, and how fast.
//!
//! Why: #8454 makes every channel deny by default. A send needs a reviewed
//! route that names the recipient; an inbound message may only answer an
//! open question; a flood is rate limited (#7457). gchat enforces this today
//! with its own types; this module is the channel-neutral form that gchat,
//! Slack and Telegram adopt in later slices.
//! What: S1 is loader-free. [`ChannelPolicy::build`] validates
//! already-parsed [`PolicySpec`] data; [`ChannelPolicy::check_egress`] and
//! [`ChannelPolicy::check_inbound`] are pure checks; [`RateLimiter`] holds
//! the in-memory sliding-window logs. Reading and parsing a policy file is S2.
//! Test: `src/policy/tests/`.

mod bucket;
mod check;
mod error;
mod table;
mod types;

#[cfg(test)]
mod tests;

pub use bucket::{BucketDecision, Clock, MonotonicClock, RateLimiter, SlidingWindow};
pub use check::{
    DenyReason, DropReason, EgressDecision, InboundDecision, QuestionId, QuestionState,
};
pub use error::PolicyError;
pub use table::{ChannelPolicy, PolicySpec};
pub use types::{
    Channel, MessageKind, RateLimit, RateLimitSpec, Route, RouteSpec, MAX_LIMIT, MAX_WINDOW_SECS,
};
