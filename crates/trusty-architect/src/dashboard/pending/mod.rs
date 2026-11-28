//! Python's `datetime.fromisoformat` for the dashboard ports (#10403).
//!
//! Why: every derive rule and task checker reads stored timestamps the way
//! `dashboard_derive._ts` does, so one parser serves them all.
//! What: [`from_iso`] and [`from_iso_parts`]. #12029 P2e deleted the serde
//! `pending` and `stale_tasks` copies this module held: the owner events
//! and the stale list are [`super::pyderive`]'s, read live through
//! [`super::pyanswers`].
//! Test: the `isotime` unit test.

mod isotime;

// See #12028: the task checker ports parse timestamps the same way.
pub(crate) use isotime::{from_iso, from_iso_parts};
