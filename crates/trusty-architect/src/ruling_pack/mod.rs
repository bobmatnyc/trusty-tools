//! The port of `ruling_pack.py`: a compact context pack before the Architect rules (#12028 P1i).
//!
//! Why: a ruling rests on prior rulings (memory), facts (search and exact
//! grep) and one direct read-only check; the script gathers the first two,
//! names the third, and labels the confidence. Epic #12025 moves it to Rust
//! once its recordings replay with zero diffs.
//! What: [`cli::run`] is the command line. The project resolves through the
//! key map (key, alias, palace, index id, repo); `trusty-memory serve` is
//! asked over MCP stdio for prior rulings on the project palace,
//! `supervisor`, and `cto` for an entry with the CTO flag; `trusty-search`
//! and `rg -F` give facts; a backend that is down is reported and counts as
//! not found. The pack goes to stdout and, unless `--no-write`, to
//! `<checkout>/local/packs/<utc>-<key>.md`.
//! Known gap: a key map of a shape the script would crash on (a non-string
//! `aliases` entry) is read leniently, not as the script's traceback; a
//! recall that times out waits the script's 25 s per reply line.
//! Test: `tests/replay_ruling_pack.rs`, `tests/ruling_pack_smoke.rs`,
//! `src/ruling_pack/tests.rs`.

mod backends;
pub mod cli;
mod pack;
mod pyerr;
mod scan;

#[cfg(test)]
mod tests;
