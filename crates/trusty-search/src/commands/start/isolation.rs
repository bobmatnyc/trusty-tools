//! What an isolated `trusty-search start` may touch (#8149, #8176).
//!
//! Why: two decisions decide whether a second daemon is actually isolated from
//! the first, and both used to be inline `if` statements inside `handle_start`
//! that no test could reach without booting a daemon. #8149: which directory
//! every per-instance path — the RPC socket above all — is derived from.
//! #8176: whether a daemon started against an explicit data directory is
//! allowed to walk the machine and reindex colocated stores it has never seen.
//!
//! What: two pure resolvers, folded into a `StartPlan`. Nothing here touches
//! process env or the filesystem; `handle_start` supplies the values and
//! stamps the result, so a test drives every combination without mutating a
//! shared environment.
//!
//! Test: `data_dir_flag_wins_over_an_inherited_env_value`,
//! `an_explicit_data_dir_never_auto_discovers_on_any_start`,
//! `spawn_forwards_the_parents_auto_discover_decision`,
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

/// Whether this start may run the auto-discovery scan.
///
/// Why (#8176): a foreground daemon started for a throwaway test with its own
/// `--data-dir` force-reindexed the colocated `.trusty-search/` stores of ~21
/// unrelated repositories, because auto-discovery was on by default on every
/// data dir and `--no-auto-discover` was the only way off. An isolated instance
/// asking for a clean room got the whole machine instead. The decision cannot
/// read the directory's contents: the first start leaves `daemon.lock` and
/// `indexes.toml` behind, so a "fresh dir" rule withholds the scan once and
/// grants it on every later start.
/// What: `--no-auto-discover` (or `TRUSTY_NO_AUTO_DISCOVER`) refuses the scan
/// outright; `--auto-discover` is the explicit opt-in that grants it on any
/// data dir; otherwise an explicit data dir refuses on every start and the
/// machine's default data dir keeps the pre-#8176 behaviour. The two flags are
/// declared `conflicts_with` one another, and this resolver still refuses
/// first, so a caller that reaches it with both set fails closed.
/// Test: `an_explicit_data_dir_never_auto_discovers_on_any_start`,
/// `auto_discover_opt_in_beats_an_explicit_data_dir`,
/// `the_default_data_dir_still_auto_discovers`.
pub(crate) fn auto_discover_enabled(
    no_auto_discover: bool,
    auto_discover: bool,
    explicit_data_dir: bool,
) -> bool {
    if no_auto_discover {
        return false;
    }
    if auto_discover {
        return true;
    }
    // #8176: an explicit data dir never scans by default, first start or not.
    !explicit_data_dir
}

/// The auto-discover flag the background self-spawn must carry, if any.
///
/// Why (#8176): the detached child re-resolves the decision on its own. It
/// sees the same explicit data dir, so it reaches the same answer today, but
/// forwarding the parent's decision keeps the child from re-deciding it if
/// the child's inputs ever drift from the parent's.
/// What: `--no-auto-discover` when the parent refused the scan,
/// `--auto-discover` when the parent granted it through the opt-in, and
/// nothing otherwise, so the child reaches the parent's own decision. The
/// opt-in is forwarded only when the caller passed it, because the child
/// inherits `TRUSTY_NO_AUTO_DISCOVER` and the two flags conflict.
/// Test: `spawn_forwards_the_parents_auto_discover_decision`.
pub(crate) fn spawn_auto_discover_arg(discover: bool, auto_discover: bool) -> Option<&'static str> {
    match (discover, auto_discover) {
        (false, _) => Some("--no-auto-discover"),
        (true, true) => Some("--auto-discover"),
        (true, false) => None,
    }
}

/// The one auto-discover decision, in the shape each `handle_start` site reads.
///
/// Why (#8176): the decision feeds three sites — warm boot's colocated scan,
/// the post-boot auto-discovery spawn, and the background self-spawn — and
/// the first takes it negated. Naming each site's reading here lets a test pin
/// all three, so a call site cannot drift from the decision unnoticed.
/// What: `Copy`, so the boot task can move it; each method is one site's view.
/// Test: `start_plan_wires_every_scan_to_one_decision`,
/// `handle_start_reads_the_plan_at_every_scan_site`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Discovery {
    granted: bool,
    opted_in: bool,
}

impl Discovery {
    /// The `no_auto_discover` argument `restore_indexes` takes (#3929).
    pub(crate) fn warm_boot_skips_colocated(self) -> bool {
        !self.granted
    }

    /// Whether to spawn `auto_discover_and_index` after warm boot.
    pub(crate) fn runs_auto_discover(self) -> bool {
        self.granted
    }

    /// The flag forwarded to the background child; see [`spawn_auto_discover_arg`].
    pub(crate) fn spawn_arg(self) -> Option<&'static str> {
        spawn_auto_discover_arg(self.granted, self.opted_in)
    }
}

/// What `handle_start` resolved before booting: the data dir and the scan
/// decision.
///
/// Why (#8149, #8176): both decisions are pure functions of the CLI and the
/// inherited environment. Resolving them in one place keeps `handle_start`
/// free of inline policy no test can reach.
/// What: the data dir from [`resolve_data_dir_override`], and a [`Discovery`]
/// that refuses the scan whenever that data dir is explicit, unless
/// `--auto-discover` opts in.
///
/// # Errors
///
/// When the resolved data dir is not absolute.
///
/// Test: `an_explicit_data_dir_never_auto_discovers_on_any_start`,
/// `start_plan_wires_every_scan_to_one_decision`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartPlan {
    pub(crate) data_dir: Option<PathBuf>,
    pub(crate) discovery: Discovery,
}

impl StartPlan {
    /// Resolve the data dir and the scan decision from flags and environment.
    pub(crate) fn resolve(
        env_value: Option<OsString>,
        flag: Option<&Path>,
        no_auto_discover: bool,
        auto_discover: bool,
    ) -> Result<Self> {
        let data_dir = resolve_data_dir_override(env_value, flag)?;
        // #8176: whether the dir is explicit decides, never what it contains.
        let granted = auto_discover_enabled(no_auto_discover, auto_discover, data_dir.is_some());
        Ok(Self {
            data_dir,
            discovery: Discovery {
                granted,
                opted_in: auto_discover,
            },
        })
    }
}

#[cfg(test)]
#[path = "isolation_tests.rs"]
mod tests;
