//! The caller variables a Linux-spawned trusty-search daemon keeps (#11085).
//!
//! Why: `trusty_common::linux_lifecycle::start` clears the spawned daemon's
//! environment down to `DaemonDescriptor::BASE_ENV` plus the descriptor's
//! `env_passthrough` (#11098). The self-spawn it replaces, and every bridge
//! auto-start, inherit the caller's whole environment, so any variable the
//! daemon reads that is missing here makes a daemon started by `start` or
//! `restart` behave differently from one a bridge starts.
//! What: [`ENV_PASSTHROUGH`] is `trusty_common::linux_lifecycle::passthrough::
//! SEARCH_DAEMON`, derived from every `env::var` / `var_os` read in the daemon
//! (this crate's `src/`, the trusty-common code it calls at runtime, and the
//! embedder sidecars it spawns, which inherit its environment), minus the
//! names in [`EXCLUDED`]. The list lives in trusty-common because a
//! trusty-console started by the verbs passes it too: its stdio bridge
//! auto-starts this daemon with the console's environment. Credential-shaped
//! names are never listed.
//! Test: `env_passthrough_carries_every_daemon_setting_and_no_credential`,
//! `env_passthrough_covers_every_variable_this_crate_reads`.

/// Names copied from the caller's environment, when set, into the daemon's.
pub(crate) const ENV_PASSTHROUGH: &[&str] =
    trusty_common::linux_lifecycle::passthrough::SEARCH_DAEMON;

/// Names the daemon code reads that are deliberately NOT passed, with why.
///
/// `env_passthrough_covers_every_variable_this_crate_reads` requires every
/// name this crate's `src/` reads to be in [`ENV_PASSTHROUGH`], in
/// `BASE_ENV`, or here.
#[cfg(test)]
pub(crate) const EXCLUDED: &[(&str, &str)] = &[
    ("OPENROUTER_API_KEY", "credential; owner decision (#11085)"),
    (
        "TRUSTY_EXIT_WITH_PARENT",
        "parent-death stamp; #11098 drops it",
    ),
    ("TRUSTY_INDEX", "CLI index selector, read by the client"),
    (
        "TRUSTY_SEARCH_SOCKET",
        "client socket override; the daemon binds --socket",
    ),
    ("TRUSTY_SEARCH_DATA_DIR", "client launchd probe only"),
    ("TRUSTY_SEARCH_NO_HTTP", "retired by #9536; not read"),
    ("TRUSTY_SEARCH_ISOLATED_CHILD", "test re-exec marker"),
    ("TRUSTY_SEARCH_TEST_ASSUME_LAUNCHD_SUPERVISED", "test seam"),
    ("TRUSTY_RSS_PROBE_CHILD", "test re-exec marker"),
    ("TRUSTY_MAX_INDEX_FILES_ABSENT_4356", "test marker"),
    ("PRINT_SNAPSHOT_10498", "test snapshot printer"),
];
