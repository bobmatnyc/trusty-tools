//! Chat-channel enforcement in the daemon (#8433, #8454).
//!
//! Why: the route policy lives in trusty-channels; the daemon needs one
//! fail-closed view of it that every channel path reads.
//! What: [`crate::channels::snapshot`] — the policy snapshot;
//! [`crate::channels::readiness`] — the egress guard's inline load (#9741);
//! [`crate::channels::question_table`] — the durable question table (#8433 P2);
//! [`crate::channels::audit_log`] — the route-change and channel audit (#9832);
//! [`crate::channels::ask_owner`] — one question DM to a route's owner, posted
//! by [`crate::channels::slack_bot`] (#9856); [`crate::channels::inbound`] —
//! an owner's reply judged, bound and resolved, and [`crate::channels::deliver`]
//! — the answer delivered to its asker, tainted first (#9873);
//! [`crate::channels::socket_mode`] — the daemon's Socket Mode source,
//! claiming each event id before its ack (#9923).
//! Test: `snapshot_tests.rs`, `bin/tm/commands/egress_guard_policy_tests.rs`,
//! `question_table_tests.rs`, `audit_log_tests.rs`, `ask_owner_tests.rs`,
//! `inbound_tests.rs`, `deliver_tests.rs`, `socket_mode_tests.rs`,
//! `inbound_claims_tests.rs`.

pub mod ask_owner;
pub mod audit_log;
// #9873: the inbound decision and the delivery (P5).
pub mod deliver;
pub mod inbound;
pub mod question_table;
pub mod readiness;
pub mod slack_bot;
pub mod snapshot;
// #9923: the Socket Mode inbound source and its durable event claim (P6).
pub mod socket_mode;
