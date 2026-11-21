//! Shadow-run setup: seed `<inbox>/shadow/` from the live state.
//!
//! Why: the shadow run sits beside the Python poller for 24 h and must start
//! from the same dedup state, or every standing alert reads as a diff. It
//! must never write the live files (#9779, plan section 3).
//! What: [`prepare_shadow`] creates the shadow directory and, when it holds
//! no state yet, copies the live `poll-state.json` into it. A later start
//! keeps the shadow's own state. The live file is only read.
//! Test: `shadow_seeds_state_once_and_never_writes_the_inbox`,
//! `shadow_seed_copy_failure_is_an_error`.
use std::fs;
use std::io;

use super::config::Config;

/// Creates the shadow directory and seeds its state; returns log lines.
///
/// # Errors
/// The directory cannot be created or the live state cannot be copied.
pub fn prepare_shadow(config: &Config) -> io::Result<Vec<String>> {
    let dir = config.shadow_dir();
    fs::create_dir_all(&dir)?;
    let shadow_state = config.state();
    let live_state = config.inbox.join("poll-state.json");
    if shadow_state.exists() || !live_state.exists() {
        return Ok(Vec::new());
    }
    fs::copy(&live_state, &shadow_state)?;
    Ok(vec![format!(
        "shadow: seeded {} from {}",
        shadow_state.display(),
        live_state.display()
    )])
}
