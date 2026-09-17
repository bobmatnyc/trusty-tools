//! What an isolated `trusty-search start` may touch (#8149, #8176).
//!
//! Why: two decisions decide whether a second daemon is actually isolated from
//! the first, and both used to be inline `if` statements inside `handle_start`
//! that no test could reach without booting a daemon. #8149: which directory
//! every per-instance path — the RPC socket above all — is derived from.
//! #8176: whether a daemon started against a brand-new data directory is
//! allowed to walk the machine and reindex colocated stores it has never seen.
//!
//! What: two pure resolvers plus the freshness predicate the second one reads.
//! Neither touches process env; `handle_start` supplies the values and stamps
//! the result, so a test drives every combination without mutating a shared
//! environment.
//!
//! Test: `data_dir_flag_wins_over_an_inherited_env_value`,
//! `fresh_data_dir_does_not_auto_discover_without_opt_in`,
//! `socket_path_follows_trusty_data_dir_not_home`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::Result;

/// The environment variable that isolates one daemon instance's data.
///
/// Restated from [`crate::service::socket`]'s private constant of the same
/// value because this module stamps it and that one reads it;
/// `socket_path_follows_trusty_data_dir_not_home` pins the two to one string.
pub(crate) const DATA_DIR_ENV: &str = "TRUSTY_DATA_DIR";

/// The data directory this start must use, flag first.
///
/// Why (#8149): a second daemon that named its own `--data-dir` still resolved
/// every per-instance path from an inherited `TRUSTY_DATA_DIR` — so it bound
/// the FIRST daemon's socket and captured RPC traffic meant for it.
/// `handle_start` stamped the flag into the environment only when the variable
/// was unset, which is precedence exactly backwards from #1182's stated intent
/// and from every other `trusty-search` flag. clap already resolves
/// `--data-dir`'s own `env = "TRUSTY_DATA_DIR"` fallback, so a `Some` flag here
/// is either the CLI value or the identical env value; either way it is the one
/// to use.
/// What: the flag when present, otherwise the environment value; an empty
/// environment value is treated as unset, matching
/// `trusty_common::resolve_data_dir`'s guard for the same shape. Both forms
/// must be absolute — a relative data dir resolves against the daemon's cwd
/// (`/` under launchd), so it is refused rather than silently resolved.
///
/// # Errors
///
/// When the resolved directory is not absolute.
///
/// Test: `data_dir_flag_wins_over_an_inherited_env_value`,
/// `an_empty_data_dir_env_value_is_treated_as_unset`,
/// `a_relative_data_dir_is_refused`,
/// `socket_path_follows_trusty_data_dir_not_home`.
pub(crate) fn resolve_data_dir_override(
    env_value: Option<OsString>,
    flag: Option<&Path>,
) -> Result<Option<PathBuf>> {
    let from_env = env_value.filter(|v| !v.is_empty()).map(PathBuf::from);
    // #8149: the flag wins. An inherited value only applies when no flag came.
    let Some(dir) = flag.map(Path::to_path_buf).or(from_env) else {
        return Ok(None);
    };
    anyhow::ensure!(
        dir.is_absolute(),
        "--data-dir / {DATA_DIR_ENV} must be an absolute path (got: {})",
        dir.display()
    );
    Ok(Some(dir))
}

/// Has this data directory never held a daemon's state?
///
/// Why (#8176): a daemon pointed at a brand-new directory has no registry to
/// warm-boot from, so every root it indexes is one it discovered by walking the
/// machine. That is the case the auto-discovery default must not apply to.
/// What: true when the directory is absent, or present and empty. An
/// unreadable directory reads as NOT fresh — the safe answer when the caller
/// is about to use freshness to grant a scan is the one that withholds it.
/// Test: `fresh_data_dir_does_not_auto_discover_without_opt_in`.
pub(crate) fn data_dir_is_fresh(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_none(),
        // Absent is fresh; unreadable is not — see above.
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Whether this start may run the auto-discovery scan.
///
/// Why (#8176): a foreground daemon started for a throwaway test with a fresh
/// `--data-dir` force-reindexed the colocated `.trusty-search/` stores of ~21
/// unrelated repositories, because auto-discovery was on by default on every
/// data dir and `--no-auto-discover` was the only way off. An isolated instance
/// asking for a clean room got the whole machine instead.
/// What: `--no-auto-discover` (or `TRUSTY_NO_AUTO_DISCOVER`) refuses the scan
/// outright; `--auto-discover` is the explicit opt-in that grants it on any
/// data dir; otherwise a fresh isolated data dir refuses and the machine's
/// default data dir keeps the pre-#8176 behaviour. The two flags are declared
/// `conflicts_with` one another, and this resolver still refuses first, so a
/// caller that reaches it with both set fails closed.
/// Test: `fresh_data_dir_does_not_auto_discover_without_opt_in`,
/// `auto_discover_opt_in_beats_a_fresh_data_dir`,
/// `the_default_data_dir_still_auto_discovers`.
pub(crate) fn auto_discover_enabled(
    no_auto_discover: bool,
    auto_discover: bool,
    fresh_isolated_data_dir: bool,
) -> bool {
    if no_auto_discover {
        return false;
    }
    if auto_discover {
        return true;
    }
    !fresh_isolated_data_dir
}

#[cfg(test)]
#[path = "isolation_tests.rs"]
mod tests;
