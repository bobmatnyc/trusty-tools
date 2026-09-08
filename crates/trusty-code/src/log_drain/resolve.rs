//! Turn [`super::config::LogDrainConfig`] into a runnable plan, or refuse
//! (#6537).
//!
//! Why: the validation and resolution logic is identical to trusty-agents'
//! own copy — see `trusty_common::log_drain::single_source` module docs for
//! why the two were consolidated into one implementation (#6537 code-review
//! fix round). This file is now the thin resolver adapter: it names this
//! daemon's own crate/include and delegates to
//! [`trusty_common::log_drain::resolve_single_source`]. The YAML file
//! adapter itself lives in [`super::config`].
//! What: [`resolve_log_drain`] names [`CRATE_NAME`]/[`DEFAULT_INCLUDE`] and
//! delegates.
//! Test: `tests` here covers this daemon's own naming; the shared
//! `trusty_common::log_drain::single_source::tests` covers every validation
//! and identity-resolution rule.

use std::path::Path;

pub use trusty_common::log_drain::{
    LogDrainSetting, ResolvedLogDrain, SingleSourceError as LogDrainConfigError,
};

use super::config::LogDrainConfig;

/// Default interval between drain passes, in seconds — 15 minutes, matching
/// trusty-mpm's own default (`trusty_mpm::core::trusty_tools_config::log_drain::DEFAULT_INTERVAL_SECS`).
pub const DEFAULT_INTERVAL_SECS: u64 = trusty_common::log_drain::DEFAULT_INTERVAL_SECS;

/// Include glob for tcode's own rotating file log (`crate::logging::init_tracing_with_file_log`).
const DEFAULT_INCLUDE: &str = "tcode.log*";

/// Producing crate name, as it appears in every drained object's key.
const CRATE_NAME: &str = "trusty-code";

/// Resolve the config into a runnable plan, or refuse.
///
/// `log_root` is `crate::logging::file_log_dir()` in production; tests pass a
/// tempdir. `project_root` is `binding.root()` — `None` for a projectless
/// `tcode serve`.
///
/// # Errors
/// See [`LogDrainConfigError`].
pub fn resolve_log_drain(
    cfg: &LogDrainConfig,
    log_root: &Path,
    project_root: Option<&Path>,
) -> Result<LogDrainSetting, LogDrainConfigError> {
    trusty_common::log_drain::resolve_single_source(
        cfg,
        log_root,
        project_root,
        CRATE_NAME,
        DEFAULT_INCLUDE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `resolve_log_drain` names this daemon's own crate/include, not some
    /// default — a smoke test over the thin adapter; every validation rule
    /// itself is covered by `trusty_common::log_drain::single_source::tests`.
    #[test]
    fn resolve_log_drain_names_this_crate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest_dir = tempfile::tempdir().expect("tempdir");
        let cfg = LogDrainConfig {
            enabled: Some(true),
            destination: Some(format!("file://{}", dest_dir.path().display())),
            owner: Some("acme".to_string()),
            project: Some("widgets".to_string()),
            ..LogDrainConfig::default()
        };
        let setting = resolve_log_drain(&cfg, dir.path(), None).expect("resolve");
        let LogDrainSetting::Enabled(plan) = setting else {
            panic!("expected Enabled");
        };
        assert_eq!(plan.source.crate_name, CRATE_NAME);
        assert_eq!(plan.source.root, dir.path());
        assert_eq!(plan.source.include, vec![DEFAULT_INCLUDE.to_string()]);
    }
}
