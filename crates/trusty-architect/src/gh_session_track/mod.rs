//! The port of `gh-session-track.py`: open PRs and issues mapped onto dashboard sessions (#12087).
//!
//! Why: the Architect runs the script each pass so the Sessions section shows,
//! per session, its open PRs (one state word each) and an issue summary;
//! epic #12025 moves it to Rust once its recordings replay with zero diffs.
//! What: [`cli::run`] is the command line. `gh` makes four read-only queries
//! (open PRs, open issues, PRs merged in seven days, issues closed in seven
//! days); `plan` maps each item to one session and counts; `render` prints
//! the plan and builds the store ops; `write` writes them as one
//! all-or-nothing [`crate::dashboard::Store::batch`], retrying a version
//! conflict up to three times. The store's change log and its `updated`
//! restamp (#10678) are the Rust store's own: the replay comparison leaves
//! the log out ([`crate::replay::skip_written_file`]) and the write
//! recordings seed `updated` at the recorded instant.
//! Known gap: the script's crate-label sessions, fallback session and
//! project mark are instance data; the port derives the mark from the repo
//! and maps no crate label and no fallback (#12087 open question). It reads
//! no `DASHBOARD_SCHEMA`, and its store refuses a secret-shaped value the
//! script would write.
//! Test: `tests/replay_gh_session_track.rs`, `tests/gh_session_track_smoke.rs`,
//! `src/gh_session_track/tests.rs`.

pub mod cli;
mod gh;
mod plan;
mod render;
pub mod write;

pub use plan::SessionMap;

#[cfg(test)]
mod tests;
