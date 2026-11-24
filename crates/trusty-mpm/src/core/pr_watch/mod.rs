//! Local PR watch: the tracked-PR table and the head-aware CI transition rule
//! (#11673).
//!
//! Why: a PM waiting on a PR's required checks stalled until someone relayed
//! the result (#11595, #2610). Nothing recorded "CI ended on PR N at head H",
//! and `tm wait --for check` dies with the PM's turn. This module holds the
//! state and the pure rules a daemon loop needs to turn a CI result into one
//! event and one wake per transition, with no inbound endpoint.
//! What: [`table`] is the durable table (`tm pr watch add|ls|rm` writes it,
//! the daemon reads and advances it). [`verdict`] judges the required
//! contexts on one head and names the transition. [`gh`] reads one PR's
//! head, base and rollup, and its required contexts, through `gh`. The loop
//! itself is `daemon::services::pr_watch_loop`.
//!
//! The table is read only by `tm pr watch`, `tm wait --for check --watch` and
//! that loop. Merge pins, `tm pr queue-check`, `tm pr merge` and the critic
//! and review gates never read it (AC6).
//! Test: `tests.rs`; `daemon/services/pr_watch_loop_tests.rs`.

pub mod gh;
pub mod table;
pub mod verdict;

#[cfg(test)]
mod tests;
