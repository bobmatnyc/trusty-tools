//! The build-lease keys of the `[builders]` table (#8261 increment two).
//!
//! Why: `BuildersConfig` shipped in trusty-mpm 1.7.0 as an exhaustive struct
//! with public fields, so adding a field to it is a semver-major change, and
//! this work ships in a patch release. The lease's own keys therefore live in a
//! separate, `#[non_exhaustive]` struct read from the SAME `[builders]` table of
//! `~/.trusty-mpm/config.toml`: operators write one section, and neither
//! `BuildersConfig` nor `MpmConfig` changes shape.
//!
//! What: [`BuildLeaseConfig`] holds `memory_pressure_max`, `min_available_pct`,
//! `lease_wait_secs`, `heavy_build_commands` and `count_foreign_builds`, each
//! optional with a documented default. [`BuildLeaseConfig::from_toml`] reads
//! them out of a whole config file; [`LEASE_KEYS`] names them for the
//! unknown-key report in `core::config_keys`.
//! Test: the `#[cfg(test)]` suite below.

use serde::{Deserialize, Serialize};
use trusty_common::memory_pressure::PressureLevel;

use crate::core::builders::BuildersConfigError;

/// Default for [`BuildLeaseConfig::min_available_pct`].
///
/// Why: an initial value, not a measurement — well under this host's quiet
/// reading (`kern.memorystatus_level` 95). Revisit from live data.
/// Test: `defaults_are_documented_values`.
pub const DEFAULT_MIN_AVAILABLE_PCT: f64 = 10.0;

/// Default for [`BuildLeaseConfig::lease_wait_secs`].
///
/// Why: below the Bash tool's default 120 s timeout, so the refusal naming the
/// holders reaches the agent instead of a silent kill.
/// Test: `defaults_are_documented_values`.
pub const DEFAULT_LEASE_WAIT_SECS: u64 = 90;

/// The longest wait accepted: the Bash tool's 600 s maximum timeout.
pub const MAX_LEASE_WAIT_SECS: u64 = 600;

/// Default heavy-build table (owner ruling 2026-09-24, plus cargo's own
/// aliases and the compiling cargo verbs and plugins).
///
/// Test: `defaults_are_documented_values`.
pub const DEFAULT_HEAVY_BUILD_COMMANDS: &[&str] = &[
    "cargo build",
    "cargo b",
    "cargo test",
    "cargo t",
    "cargo clippy",
    "cargo check",
    "cargo c",
    "cargo doc",
    "cargo d",
    "cargo install",
    "cargo run",
    "cargo r",
    "cargo bench",
    "cargo fix",
    "cargo rustc",
    "cargo rustdoc",
    "cargo publish",
    "cargo package",
    "cargo nextest",
    "cargo llvm-cov",
    "cargo miri",
];

/// The dotted config paths this struct reads, for `core::config_keys`.
pub const LEASE_KEYS: &[&str] = &[
    "builders.memory_pressure_max",
    "builders.min_available_pct",
    "builders.lease_wait_secs",
    "builders.heavy_build_commands",
    "builders.count_foreign_builds",
];

/// The lease keys of `[builders]`.
///
/// Test: `lease_keys_are_read_from_the_builders_table`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct BuildLeaseConfig {
    /// Highest pressure level at which a NEW build is admitted; `normal`.
    pub memory_pressure_max: Option<String>,
    /// Minimum available-memory percentage for a new build; 10.
    pub min_available_pct: Option<f64>,
    /// Seconds to wait for a slot; 90, at most 600.
    pub lease_wait_secs: Option<u64>,
    /// `program [subcommand]` entries the hook leases.
    pub heavy_build_commands: Option<Vec<String>>,
    /// Whether foreign compiler groups reduce the slots; true.
    pub count_foreign_builds: Option<bool>,
}

/// The file shape [`BuildLeaseConfig::from_toml`] reads: only `[builders]`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct File {
    builders: BuildLeaseConfig,
}

impl BuildLeaseConfig {
    /// Read the lease keys out of a whole `config.toml` text.
    ///
    /// What: a text that does not parse, or a `[builders]` value of the wrong
    /// type, yields the defaults with a warning — the same lenient posture as
    /// `MpmConfig::load`.
    /// Test: `lease_keys_are_read_from_the_builders_table`.
    #[must_use]
    pub fn from_toml(raw: &str) -> Self {
        toml::from_str::<File>(raw)
            .map(|f| f.builders)
            .unwrap_or_else(|err| {
                tracing::warn!("build-lease keys in [builders] unreadable ({err}); using defaults");
                Self::default()
            })
    }

    /// `~/.trusty-mpm/config.toml`'s lease keys, or the defaults.
    #[must_use]
    pub fn load_default() -> Self {
        dirs::home_dir()
            .and_then(|h| std::fs::read_to_string(h.join(".trusty-mpm/config.toml")).ok())
            .map(|raw| Self::from_toml(&raw))
            .unwrap_or_default()
    }

    /// Refuse a value the lease will not act on, naming the key.
    ///
    /// # Errors
    ///
    /// [`BuildersConfigError::OutOfRange`] for the first bad key.
    ///
    /// Test: `out_of_range_keys_are_refused_naming_the_key`.
    pub fn validate(&self) -> Result<(), BuildersConfigError> {
        if let Some(level) = self.memory_pressure_max.as_deref()
            && PressureLevel::parse(level).is_none()
        {
            return Err(out_of_range(
                "memory_pressure_max",
                format!("{level:?}"),
                "one of normal, warn, critical",
            ));
        }
        if let Some(pct) = self.min_available_pct
            && !(pct.is_finite() && (0.0..=100.0).contains(&pct))
        {
            return Err(out_of_range(
                "min_available_pct",
                pct.to_string(),
                "a percentage in 0..=100",
            ));
        }
        if let Some(secs) = self.lease_wait_secs
            && !(1..=MAX_LEASE_WAIT_SECS).contains(&secs)
        {
            return Err(out_of_range(
                "lease_wait_secs",
                secs.to_string(),
                "seconds in 1..=600 (the Bash tool's maximum timeout)",
            ));
        }
        Ok(())
    }

    /// The pressure ceiling for a new build; an unparseable value reads `normal`.
    #[must_use]
    pub fn effective_memory_pressure_max(&self) -> PressureLevel {
        self.memory_pressure_max
            .as_deref()
            .and_then(PressureLevel::parse)
            .unwrap_or(PressureLevel::Normal)
    }

    /// The available-memory floor, percent.
    #[must_use]
    pub fn effective_min_available_pct(&self) -> f64 {
        self.min_available_pct.unwrap_or(DEFAULT_MIN_AVAILABLE_PCT)
    }

    /// The slot wait, clamped to `1..=600` seconds whatever was configured.
    ///
    /// Test: `the_wait_is_clamped_to_the_bash_tool_range`.
    #[must_use]
    pub fn effective_lease_wait(&self) -> std::time::Duration {
        std::time::Duration::from_secs(clamp_wait(
            self.lease_wait_secs.unwrap_or(DEFAULT_LEASE_WAIT_SECS),
        ))
    }

    /// Whether foreign compiler groups reduce the slots.
    #[must_use]
    pub fn effective_count_foreign_builds(&self) -> bool {
        self.count_foreign_builds.unwrap_or(true)
    }

    /// The heavy-build table as `(program, optional subcommand)` pairs.
    ///
    /// Test: `defaults_are_documented_values`, `a_configured_heavy_list_replaces_the_default`.
    #[must_use]
    pub fn effective_heavy_build_commands(&self) -> Vec<(String, Option<String>)> {
        let owned: Vec<String> = match &self.heavy_build_commands {
            Some(list) => list.clone(),
            None => DEFAULT_HEAVY_BUILD_COMMANDS
                .iter()
                .map(ToString::to_string)
                .collect(),
        };
        owned
            .iter()
            .filter_map(|entry| {
                let mut words = entry.split_whitespace();
                let program = words.next()?.to_string();
                Some((program, words.next().map(str::to_string)))
            })
            .collect()
    }
}

/// Clamp a wait to `1..=MAX_LEASE_WAIT_SECS` seconds.
///
/// Test: `the_wait_is_clamped_to_the_bash_tool_range`.
#[must_use]
pub fn clamp_wait(secs: u64) -> u64 {
    secs.clamp(1, MAX_LEASE_WAIT_SECS)
}

fn out_of_range(key: &'static str, value: String, allowed: &'static str) -> BuildersConfigError {
    BuildersConfigError::OutOfRange {
        key,
        value,
        allowed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_documented_values() {
        let cfg = BuildLeaseConfig::default();
        assert_eq!(cfg.effective_memory_pressure_max(), PressureLevel::Normal);
        assert!(
            (cfg.effective_min_available_pct() - DEFAULT_MIN_AVAILABLE_PCT).abs() < f64::EPSILON
        );
        assert_eq!(
            cfg.effective_lease_wait().as_secs(),
            DEFAULT_LEASE_WAIT_SECS
        );
        const { assert!(DEFAULT_LEASE_WAIT_SECS < 120) };
        assert!(cfg.effective_count_foreign_builds());
        let heavy = cfg.effective_heavy_build_commands();
        assert_eq!(heavy.len(), DEFAULT_HEAVY_BUILD_COMMANDS.len());
        for sub in [
            "test", "t", "b", "nextest", "miri", "llvm-cov", "fix", "publish",
        ] {
            assert!(
                heavy.contains(&("cargo".to_string(), Some(sub.to_string()))),
                "{sub}"
            );
        }
    }

    #[test]
    fn a_configured_heavy_list_replaces_the_default() {
        let cfg = BuildLeaseConfig {
            heavy_build_commands: Some(vec!["make".into(), "cargo nextest".into(), "  ".into()]),
            ..BuildLeaseConfig::default()
        };
        assert_eq!(
            cfg.effective_heavy_build_commands(),
            vec![
                ("make".to_string(), None),
                ("cargo".to_string(), Some("nextest".to_string()))
            ]
        );
    }

    #[test]
    fn lease_keys_are_read_from_the_builders_table() {
        let cfg = BuildLeaseConfig::from_toml(
            "[builders]\nmax_concurrent = 4\nfree_memory_floor_mb = 8192\n\
             memory_pressure_max = \"warn\"\nlease_wait_secs = 30\n[hooks]\nprompt_context = false\n",
        );
        assert_eq!(cfg.effective_memory_pressure_max(), PressureLevel::Warn);
        assert_eq!(cfg.effective_lease_wait().as_secs(), 30);
        assert_eq!(
            BuildLeaseConfig::from_toml("not toml ["),
            BuildLeaseConfig::default()
        );
    }

    #[test]
    fn out_of_range_keys_are_refused_naming_the_key() {
        for (cfg, key) in [
            (
                BuildLeaseConfig {
                    memory_pressure_max: Some("high".into()),
                    ..Default::default()
                },
                "memory_pressure_max",
            ),
            (
                BuildLeaseConfig {
                    min_available_pct: Some(120.0),
                    ..Default::default()
                },
                "min_available_pct",
            ),
            (
                BuildLeaseConfig {
                    lease_wait_secs: Some(0),
                    ..Default::default()
                },
                "lease_wait_secs",
            ),
            (
                BuildLeaseConfig {
                    lease_wait_secs: Some(601),
                    ..Default::default()
                },
                "lease_wait_secs",
            ),
        ] {
            let err = cfg.validate().expect_err("out of range");
            assert!(err.to_string().contains(key), "{err}");
        }
        assert!(BuildLeaseConfig::default().validate().is_ok());
    }

    #[test]
    fn the_wait_is_clamped_to_the_bash_tool_range() {
        assert_eq!(clamp_wait(0), 1);
        assert_eq!(clamp_wait(10_000), MAX_LEASE_WAIT_SECS);
        let cfg = BuildLeaseConfig {
            lease_wait_secs: Some(5_000),
            ..Default::default()
        };
        assert_eq!(cfg.effective_lease_wait().as_secs(), MAX_LEASE_WAIT_SECS);
    }
}
