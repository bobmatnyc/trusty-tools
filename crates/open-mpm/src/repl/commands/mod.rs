//! Slash-command dispatch + all `*_into` handler methods for the REPL.
//!
//! Why: `try_handle_slash` is the central jump table the REPL bridges
//! against. Keeping the dispatch and per-command handlers together — but
//! out of `mod.rs` — makes the command surface easy to scan without
//! drowning the lifecycle code.
//! What: `impl OpenMpmRepl` block hosting `try_handle_slash` plus every
//! `*_into` writer used by the dispatch arms. The `/config` and
//! `/service` arms are factored into `handle_config_command_into` and
//! `handle_service_command_into` to keep the dispatch body readable.
//! Test: Comprehensive coverage lives in `mod.rs::tests` (see
//! `try_handle_slash_*` cases) which still reach in here via `use super::*`.

mod connect;
mod dispatch;
mod help;
mod integrations;
mod logs;
mod routing;

// `try_handle_slash` (dispatch) calls `write_help`; re-export it so the
// split submodules share it (#357).
pub(crate) use help::write_help;
