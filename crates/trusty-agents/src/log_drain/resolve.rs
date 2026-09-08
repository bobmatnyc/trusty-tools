//! The `[log_drain]` section of `~/.trusty-agents/config.toml`, and its
//! resolution into a runnable plan (#6537).
//!
//! Why: the validation and resolution logic is identical to trusty-code's own
//! copy — see `trusty_common::log_drain::single_source` module docs for why
//! the two were consolidated into one implementation (#6537 code-review fix
//! round). This file is now the thin TOML-section adapter: a type alias onto
//! [`trusty_common::log_drain::SingleSourceSection`] (the two formats
//! serialize the same eight fields, so no mapping code is needed) plus this
//! daemon's own crate name and include glob.
//! What: [`LogDrainConfig`] is the on-disk shape; [`resolve_log_drain`] names
//! [`CRATE_NAME`]/[`DEFAULT_INCLUDE`] and delegates to
//! [`trusty_common::log_drain::resolve_single_source`].
//! Test: `tests` here covers the TOML round-trip and this daemon's own
//! naming; `trusty_common::log_drain::single_source::tests` covers every
//! validation and identity-resolution rule.

use std::path::Path;

pub use trusty_common::log_drain::{
    LogDrainSetting, ResolvedLogDrain, SingleSourceError as LogDrainConfigError,
};

/// The `[log_drain]` section. Every field optional, same names as
/// trusty-mpm's `log_drain:` section
/// (`trusty_mpm::core::trusty_tools_config::log_drain::LogDrainConfig`).
///
/// Why: modelled here as a bare alias, not parsed independently —
/// `GlobalConfig::save()` re-serializes only the fields declared on the
/// aliased struct, so an unmodelled `[log_drain]` table would be silently
/// dropped by any unrelated `mcp_*`-tool write.
/// Test: `tests::toml_round_trips_every_field`.
pub type LogDrainConfig = trusty_common::log_drain::SingleSourceSection;

/// Include glob for this daemon's own rotating log
/// (`service::daemon_log_path`'s `daemon-<hash>-<port>.log` plus its
/// `.log.1` rotation backup).
const DEFAULT_INCLUDE: &str = "daemon-*.log*";

/// Producing crate name, as it appears in every drained object's key.
const CRATE_NAME: &str = "trusty-agents";

/// Resolve the config into a runnable plan, or refuse.
///
/// `log_root` is `service::log_dir()` in production; tests pass a tempdir.
/// `project_root` is the current project's root (`ctrl::detect_self_project()`
/// in production) — `None` when this daemon serves no particular project.
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

    /// `GlobalConfig::save()` round-trips only fields it knows about — this
    /// pins the section's own TOML shape so a future field addition here
    /// stays additive.
    #[test]
    fn toml_round_trips_every_field() {
        let cfg = LogDrainConfig {
            enabled: Some(true),
            destination: Some("s3://bucket/prefix".to_string()),
            interval_secs: Some(300),
            max_file_bytes: Some(1024),
            max_wire_bytes: Some(2048),
            secrets: vec!["shh".to_string()],
            owner: Some("acme".to_string()),
            project: Some("widgets".to_string()),
        };
        let toml_str = toml::to_string(&cfg).expect("serialize");
        let back: LogDrainConfig = toml::from_str(&toml_str).expect("deserialize");
        assert_eq!(cfg, back);
    }

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
