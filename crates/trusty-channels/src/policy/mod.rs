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
//! the in-memory sliding-window logs. S2a adds the two-file model, still
//! without I/O: [`parse_host`] reads the host ceiling (the `channels:`
//! section of `config.yaml`), [`parse_project_file`] a project's
//! `routes.toml` v1 or v2, and [`merge`] combines them into a
//! [`LoadReport`]. Reading the files is S2b.
//! Test: `src/policy/tests/`.

mod bucket;
mod check;
mod error;
mod host;
mod merge;
mod project_file;
mod report;
mod table;
mod types;

#[cfg(test)]
mod tests;

pub use bucket::{BucketDecision, Clock, MonotonicClock, RateLimiter, SlidingWindow};
pub use check::{
    DenyReason, DropReason, EgressDecision, InboundDecision, QuestionId, QuestionState,
};
pub use error::PolicyError;
pub use host::{parse_host, HostCeiling, HostChannel, HostError, HOST_SCHEMA_VERSION};
pub use merge::{merge, ProjectInput};
pub use project_file::{parse_project_file, ProjectFile, ProjectFileError, ProjectRoute};
pub use report::{FileState, FileStatus, Finding, FindingScope, LoadReport, Origin};
pub use table::{ChannelPolicy, PolicySpec};
pub use types::{
    Channel, MessageKind, RateLimit, RateLimitSpec, Route, RouteSpec, MAX_LIMIT, MAX_WINDOW_SECS,
};
