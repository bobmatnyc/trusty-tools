//! Resolve the path to the running `tcode` binary, for re-spawning itself
//! as `tcode serve --stdio` (#2060).
//!
//! Why: [`trusty_code::cli_client::StdioRpcClient::spawn`] takes an explicit
//! binary path rather than reading `std::env::current_exe()` itself, so
//! that library function stays a pure "spawn this path" primitive testable
//! with any binary. The CLI is the one caller that actually wants "myself"
//! — this tiny wrapper is where that policy lives, with an actionable error
//! message instead of a bare `std::io::Error` if resolution fails (a rare
//! but real possibility per `std::env::current_exe`'s own docs — deleted
//! binary, unusual sandboxing).
//! Test: exercised implicitly by every `tests/cli_e2e.rs` case (they all
//! spawn the real compiled binary, which must resolve itself successfully).

use std::path::PathBuf;

use anyhow::{Context, Result};

/// Resolve the path to the currently-running `tcode` binary.
///
/// Why: see module docs.
/// What: thin wrapper over `std::env::current_exe()` with a CLI-appropriate
/// error message.
pub fn resolve() -> Result<PathBuf> {
    std::env::current_exe().context(
        "tcode: could not resolve its own executable path (needed to spawn `tcode serve --stdio`)",
    )
}
