//! The machine-wide builder-slot cap and where its number comes from (#6892).
//!
//! Why: "at most 2 concurrent builders" lived in PM memory, per session. On
//! 2026-08-08 several independent `tm` sessions each honouring their own "2"
//! produced six concurrent `cargo` builds and crashed the host. A per-session
//! rule cannot prevent that, because the hazard — the machine's RAM and CPU —
//! is a property of the MACHINE, not of any one session. The cap therefore
//! belongs to the harness and is enforced once, machine wide.
//!
//! What: [`BuildersConfig`] is the `[builders]` section of
//! `~/.trusty-mpm/config.toml`; [`tier_default_max_concurrent`] derives the
//! shipped default from the host's [`MemoryTier`]; and
//! [`resolve_max_concurrent`] is the ONE place the two are combined.
//!
//! **The host root is the only layer that may set this, and that is enforced by
//! which loader is called.** [`resolve_max_concurrent`] calls
//! [`MpmConfig::load_default`] — never the effective-config loaders that fold
//! outer layers on — so neither a project's committed `.trusty-mpm.toml` nor
//! `TrustyToolsConfig`'s per-project YAML section can raise a cap that protects
//! the machine every project on it shares. A project that could raise its own
//! cap would reintroduce exactly the per-scope rule this module replaces.
//! `.trusty-mpm.toml` also parses with `deny_unknown_fields`, so a `[builders]`
//! section there is a hard error rather than a silent no-op.
//!
//! **The RAM-tier table is an inference, not a measured root cause.** The
//! 2026-08-08 crash was never attributed to RAM exhaustion rather than to disk
//! contention from six parallel `target/` writes on one volume (#6892 open item
//! 2). [`MemoryTier`] is used because it is the sizing input every other
//! trusty-* daemon already reads (#6879, #6845), and because an operator who
//! disagrees sets `builders.max_concurrent` and is done. Treat the numbers as a
//! starting point a measurement may move, not as a derivation.
//! Test: the `#[cfg(test)]` suite below.

use serde::{Deserialize, Serialize};
use trusty_common::machine_tier::{MemoryTier, detect_total_ram_mb};

use crate::core::config::MpmConfig;

/// Total RAM assumed when the host cannot be read.
///
/// Why: [`detect_total_ram_mb`] returns `None` only when the platform read
/// fails outright, which is rare and says nothing about the machine. Assuming
/// the SMALLEST supported configuration is the conservative direction for a cap
/// whose job is to stop the host being overcommitted — guessing high would
/// admit the builds this module exists to refuse.
/// What: 16 GB in megabytes, which [`MemoryTier::from_total_ram_mb`] resolves to
/// [`MemoryTier::Medium`] and therefore to a default cap of 2.
/// Test: `undetectable_ram_falls_back_to_the_medium_tier`.
const UNDETECTABLE_RAM_MB: u64 = 16 * 1024;

/// Default multiplier on logical cores for the load-average threshold (#8261).
///
/// Why: 2.0 is an owner ruling, not a derivation — "this host's quiet baseline
/// is about 1.27x cores, which is why the report's 1.5x was rejected" (2026-09-20).
/// A threshold under the machine's own idle baseline would refuse every builder
/// on a quiet host.
/// Test: `capacity_defaults_match_the_owner_ruling`.
pub const DEFAULT_LOAD_FACTOR: f64 = 2.0;

/// Default free-memory floor, megabytes (#8261).
///
/// Why: 8192 is the owner's number (2026-09-20). The incident behind #6892 was
/// memory exhaustion from six concurrent `cargo` builds, and a load average
/// does not measure memory pressure at all.
/// Test: `capacity_defaults_match_the_owner_ruling`.
pub const DEFAULT_FREE_MEMORY_FLOOR_MB: u64 = 8192;

/// Default root for the per-slot build directories (#8261).
///
/// Why: deliberately a SIBLING of `~/.trusty-tools/cargo-target` rather than a
/// child, so a slot directory can never be mistaken for the shared target
/// directory it was cloned from, and so deleting the pool cannot take the warm
/// shared cache with it.
/// Test: `capacity_defaults_match_the_owner_ruling`.
pub const DEFAULT_SLOT_POOL_ROOT: &str = "~/.trusty-tools/cargo-target-pool";

/// The widest `load_factor` an operator may set.
///
/// Why: a range check exists so a typo (`load_factor = 200` for a percentage)
/// is refused with the key named rather than silently disabling the load check
/// for the life of the machine. 64x cores is past any plausible intent.
/// Test: `an_out_of_range_load_factor_is_refused_naming_the_key`.
const MAX_LOAD_FACTOR: f64 = 64.0;

/// The narrowest `load_factor` an operator may set.
///
/// Why: at or below zero the threshold is unreachable and every builder is
/// refused forever — a config that bricks the harness, not a conservative one.
/// Test: `a_zero_load_factor_is_refused_naming_the_key`.
const MIN_LOAD_FACTOR: f64 = 0.1;

/// The largest `free_memory_floor_mb` an operator may set: 1 TiB.
///
/// Why: same reasoning as [`MAX_LOAD_FACTOR`] — a floor above any real machine's
/// RAM refuses every builder permanently, and is far more likely a unit mistake
/// (bytes written into a megabyte key) than an intention.
/// Test: `an_out_of_range_memory_floor_is_refused_naming_the_key`.
const MAX_FREE_MEMORY_FLOOR_MB: u64 = 1024 * 1024;

/// A `[builders]` value the operator set that this harness will not act on.
///
/// Why (#8261): these keys gate whether the machine admits a build at all, so a
/// value that cannot be honoured must be REFUSED with the key named, never
/// clamped into range behind the operator's back. Clamping would leave an
/// operator who wrote `load_factor = 200` believing the load check was doing
/// something.
/// What: one variant, carrying the key, the value seen, and the accepted range.
/// `thiserror` because this is library code.
/// Test: `an_out_of_range_load_factor_is_refused_naming_the_key`,
/// `an_out_of_range_memory_floor_is_refused_naming_the_key`,
/// `an_empty_slot_pool_root_is_refused_naming_the_key`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BuildersConfigError {
    /// A key's value is outside the range this harness accepts.
    #[error(
        "builders.{key} = {value} in ~/.trusty-mpm/config.toml is out of range ({allowed}); \
         the harness will not act on it"
    )]
    OutOfRange {
        /// The offending key, without the `builders.` prefix.
        key: &'static str,
        /// The value as the operator wrote it.
        value: String,
        /// The accepted range, in words.
        allowed: &'static str,
    },
}

/// The `[builders]` section of `~/.trusty-mpm/config.toml`.
///
/// Why: the tier-derived default is a guess about a machine, and an operator
/// who has measured their own knows better. This section is that override, and
/// it is the only one — see the module doc for why no project layer may set it.
/// What: four optional keys, each falling back to a documented default when
/// absent. `max_concurrent` keeps its name and its home and now means the hard
/// CEILING that the capacity formula may never exceed (#8261), rather than the
/// fixed count it was under #6892.
///
/// **No `deny_unknown_fields` here (#8261 critic round).** It made a typo under
/// `[builders]` fail the whole `MpmConfig` parse, and `MpmConfig::load` turns a
/// parse failure into `MpmConfig::default()` behind one warn — so a single
/// misspelled key silently discarded every other section of the operator's
/// config, `max_concurrent` included. `config_keys::diff_into` already reports
/// unknown nested keys on the Ok arm, which is the surface that survives the
/// lenient parse (#5207) instead of destroying it.
/// Test: `absent_section_uses_the_tier_default`, `an_explicit_cap_wins`,
/// `an_explicit_zero_admits_no_builder`, `capacity_defaults_match_the_owner_ruling`,
/// `a_typo_under_builders_does_not_discard_the_rest_of_the_config`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildersConfig {
    /// The hard ceiling on concurrent builders for this machine (#8261).
    ///
    /// `None` (the default) → [`tier_default_max_concurrent`] for the host's
    /// detected [`MemoryTier`]. `Some(n)` → exactly `n`, including `0`, which
    /// admits no builder at all: an operator who writes that has said something
    /// deliberate, and silently reading it as "unlimited" would invert their
    /// meaning at the one setting where being wrong costs the whole machine.
    ///
    /// Since #8261 this is the CEILING, not the count: the effective N is
    /// derived from measured capacity and can be lower, never higher.
    pub max_concurrent: Option<u32>,

    /// Multiplier on logical cores for the 1-minute load-average threshold.
    ///
    /// `None` → [`DEFAULT_LOAD_FACTOR`]. A machine whose 1-minute load average
    /// is at or below `logical_cores * load_factor` is considered to have room.
    pub load_factor: Option<f64>,

    /// Free memory a machine must still have, in megabytes, to admit a builder.
    ///
    /// `None` → [`DEFAULT_FREE_MEMORY_FLOOR_MB`]. Read against the OS
    /// "available" figure, which counts reclaimable pages.
    pub free_memory_floor_mb: Option<u64>,

    /// Where the per-slot build directories live.
    ///
    /// `None` → [`DEFAULT_SLOT_POOL_ROOT`]. A leading `~` expands against the
    /// home directory, the same as `build.cargo_target_dir` already does.
    pub slot_pool_root: Option<String>,
}

impl BuildersConfig {
    /// This section's cap, with the tier default folded in.
    ///
    /// Why: kept separate from [`resolve_max_concurrent`] so the precedence is
    /// assertable without a home directory, a config file, or a real machine.
    /// What: `max_concurrent` when set — `0` included — else
    /// [`tier_default_max_concurrent`] of `tier`.
    /// Test: `absent_section_uses_the_tier_default`, `an_explicit_cap_wins`,
    /// `an_explicit_zero_admits_no_builder`.
    #[must_use]
    pub fn effective_max_concurrent(&self, tier: MemoryTier) -> u32 {
        self.max_concurrent
            .unwrap_or_else(|| tier_default_max_concurrent(tier))
    }

    /// The load-average multiplier this section resolves to (#8261).
    ///
    /// Why: one accessor per key so the default lives with the key and no
    /// caller re-derives it. Callers must [`Self::validate`] first — this
    /// returns the configured value as written.
    /// Test: `capacity_defaults_match_the_owner_ruling`.
    #[must_use]
    pub fn effective_load_factor(&self) -> f64 {
        self.load_factor.unwrap_or(DEFAULT_LOAD_FACTOR)
    }

    /// The free-memory floor this section resolves to, in BYTES (#8261).
    ///
    /// Why: the config key is megabytes because that is what an operator writes;
    /// every comparison downstream is against
    /// [`MemoryMetrics::available_bytes`](trusty_common::host_metrics::MemoryMetrics),
    /// so the conversion belongs here rather than at each comparison site.
    /// What: saturating, so a floor near `u64::MAX` cannot wrap to a floor of
    /// nearly zero — the direction that would admit builds rather than refuse
    /// them. [`Self::validate`] rejects such a value first regardless.
    /// Test: `capacity_defaults_match_the_owner_ruling`,
    /// `a_huge_memory_floor_saturates_rather_than_wrapping`.
    #[must_use]
    pub fn effective_free_memory_floor_bytes(&self) -> u64 {
        self.free_memory_floor_mb
            .unwrap_or(DEFAULT_FREE_MEMORY_FLOOR_MB)
            .saturating_mul(1024 * 1024)
    }

    /// The slot-pool root this section resolves to, tilde expanded (#8261).
    ///
    /// Why: the pool root is a path an operator types, and `~` is what they
    /// type. Expansion uses the same helper `build.cargo_target_dir` does, so
    /// the two keys cannot disagree about what `~` means.
    /// What: `home` is explicit so every test is hermetic and nothing under this
    /// function can reach the real home directory.
    /// Test: `the_slot_pool_root_expands_a_leading_tilde`.
    #[must_use]
    pub fn effective_slot_pool_root(&self, home: &std::path::Path) -> std::path::PathBuf {
        let template = self
            .slot_pool_root
            .as_deref()
            .unwrap_or(DEFAULT_SLOT_POOL_ROOT);
        trusty_common::workspace_layout::expand_tilde(template, home)
    }

    /// Refuse a value this harness will not act on (#8261).
    ///
    /// Why: these keys gate whether the machine builds, so an out-of-range value
    /// must be named rather than clamped — see [`BuildersConfigError`]. Called
    /// at the resolution site, so a bad value is reported once per decision with
    /// the key in the message, not swallowed at parse time.
    /// What: checks `load_factor` is finite and within
    /// `MIN_LOAD_FACTOR..=MAX_LOAD_FACTOR`, `free_memory_floor_mb` is at or
    /// under [`MAX_FREE_MEMORY_FLOOR_MB`], and `slot_pool_root`, when set, is
    /// not blank. `max_concurrent` needs no check: every `u32` including `0` is
    /// a meaning this section already documents.
    ///
    /// # Errors
    ///
    /// [`BuildersConfigError::OutOfRange`] naming the offending key.
    ///
    /// Test: `an_out_of_range_load_factor_is_refused_naming_the_key`,
    /// `a_zero_load_factor_is_refused_naming_the_key`,
    /// `an_out_of_range_memory_floor_is_refused_naming_the_key`,
    /// `an_empty_slot_pool_root_is_refused_naming_the_key`,
    /// `a_default_section_validates`.
    pub fn validate(&self) -> Result<(), BuildersConfigError> {
        if let Some(factor) = self.load_factor
            && !(factor.is_finite() && (MIN_LOAD_FACTOR..=MAX_LOAD_FACTOR).contains(&factor))
        {
            return Err(BuildersConfigError::OutOfRange {
                key: "load_factor",
                value: factor.to_string(),
                allowed: "a finite multiplier in 0.1..=64.0 times logical cores",
            });
        }
        if let Some(floor_mb) = self.free_memory_floor_mb
            && floor_mb > MAX_FREE_MEMORY_FLOOR_MB
        {
            return Err(BuildersConfigError::OutOfRange {
                key: "free_memory_floor_mb",
                value: floor_mb.to_string(),
                allowed: "megabytes, at most 1048576 (1 TiB)",
            });
        }
        if let Some(root) = self.slot_pool_root.as_deref()
            && root.trim().is_empty()
        {
            return Err(BuildersConfigError::OutOfRange {
                key: "slot_pool_root",
                value: format!("{root:?}"),
                allowed: "a non-empty directory path",
            });
        }
        Ok(())
    }
}

/// The shipped builder cap for a host of this memory tier.
///
/// Why: one builder is one `cargo` invocation's worth of RAM, and the tiers are
/// already the suite's shared answer to "how big is this machine" (#6820). The
/// table is deliberately flat rather than a formula: four bands, four numbers,
/// nothing to mis-derive.
/// What: Degraded (<16 GB) → 1, Medium (16–31 GB) → 2, Large (32–63 GB) → 3,
/// XLarge (>=64 GB) → 4. Never `0` — a machine that can run trusty-mpm at all
/// can run one builder, and a default of `0` would deny every engineer dispatch
/// on a host whose operator never opened the config file.
/// Test: `tier_defaults_are_one_per_band`.
#[must_use]
pub fn tier_default_max_concurrent(tier: MemoryTier) -> u32 {
    match tier {
        MemoryTier::Degraded => 1,
        MemoryTier::Medium => 2,
        MemoryTier::Large => 3,
        MemoryTier::XLarge => 4,
    }
}

/// This host's effective builder cap.
///
/// Why: the ONE resolution site, so "which loader answers this" is a single
/// reviewable line rather than a property re-argued at each caller. The daemon
/// is its only caller — the cap must be decided by the process that does the
/// counting, or a `tm` older than the daemon could argue for a different number
/// than the one the live leases were admitted under.
/// What: [`MpmConfig::load_default`] — the host root,
/// `~/.trusty-mpm/config.toml` — folded with [`host_memory_tier`]. Never an
/// effective-config loader; see the module doc.
/// Test: `builder_cap_resolves_through_load_default_only` (a source-text guard
/// on this file), plus the pure cases on
/// [`BuildersConfig::effective_max_concurrent`].
#[must_use]
pub fn resolve_max_concurrent() -> u32 {
    MpmConfig::load_default()
        .builders
        .effective_max_concurrent(host_memory_tier())
}

/// The memory tier of the machine this process is running on.
///
/// Why: split out so the fallback is nameable and testable on its own — a
/// platform read that fails must not resolve to the largest tier.
/// What: [`MemoryTier::from_total_ram_mb`] of [`detect_total_ram_mb`], or of
/// [`UNDETECTABLE_RAM_MB`] when the host cannot be read.
/// Test: `undetectable_ram_falls_back_to_the_medium_tier`.
#[must_use]
pub fn host_memory_tier() -> MemoryTier {
    MemoryTier::from_total_ram_mb(detect_total_ram_mb().unwrap_or(UNDETECTABLE_RAM_MB))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_defaults_are_one_per_band() {
        assert_eq!(tier_default_max_concurrent(MemoryTier::Degraded), 1);
        assert_eq!(tier_default_max_concurrent(MemoryTier::Medium), 2);
        assert_eq!(tier_default_max_concurrent(MemoryTier::Large), 3);
        assert_eq!(tier_default_max_concurrent(MemoryTier::XLarge), 4);
    }

    #[test]
    fn absent_section_uses_the_tier_default() {
        let cfg = BuildersConfig::default();
        assert_eq!(cfg.max_concurrent, None);
        assert_eq!(cfg.effective_max_concurrent(MemoryTier::Large), 3);
    }

    #[test]
    fn an_explicit_cap_wins() {
        let cfg = BuildersConfig {
            max_concurrent: Some(7),
            ..BuildersConfig::default()
        };
        // The tier is ignored entirely once the operator has spoken.
        assert_eq!(cfg.effective_max_concurrent(MemoryTier::Degraded), 7);
        assert_eq!(cfg.effective_max_concurrent(MemoryTier::XLarge), 7);
    }

    #[test]
    fn an_explicit_zero_admits_no_builder() {
        // `0` is a deliberate statement, not "unset" — see the field doc.
        let cfg = BuildersConfig {
            max_concurrent: Some(0),
            ..BuildersConfig::default()
        };
        assert_eq!(cfg.effective_max_concurrent(MemoryTier::XLarge), 0);
    }

    /// #8261: the three new keys' defaults ARE the owner's 2026-09-20 ruling.
    /// Pinned as one test because the ruling is one statement.
    #[test]
    fn capacity_defaults_match_the_owner_ruling() {
        let cfg = BuildersConfig::default();
        assert_eq!(cfg.effective_load_factor(), 2.0);
        assert_eq!(
            cfg.effective_free_memory_floor_bytes(),
            8192 * 1024 * 1024,
            "8192 MB, expressed in bytes for the comparison site"
        );
        assert_eq!(
            cfg.effective_slot_pool_root(std::path::Path::new("/home/x")),
            std::path::Path::new("/home/x/.trusty-tools/cargo-target-pool"),
        );
    }

    #[test]
    fn the_slot_pool_root_expands_a_leading_tilde() {
        let cfg = BuildersConfig {
            slot_pool_root: Some("~/pools/builds".to_string()),
            ..BuildersConfig::default()
        };
        assert_eq!(
            cfg.effective_slot_pool_root(std::path::Path::new("/home/x")),
            std::path::Path::new("/home/x/pools/builds"),
        );
    }

    #[test]
    fn a_huge_memory_floor_saturates_rather_than_wrapping() {
        // Wrapping would turn "refuse everything" into "admit everything" —
        // the one direction this cap must never fail in.
        let cfg = BuildersConfig {
            free_memory_floor_mb: Some(u64::MAX),
            ..BuildersConfig::default()
        };
        assert_eq!(cfg.effective_free_memory_floor_bytes(), u64::MAX);
    }

    #[test]
    fn a_default_section_validates() {
        assert_eq!(BuildersConfig::default().validate(), Ok(()));
    }

    #[test]
    fn an_out_of_range_load_factor_is_refused_naming_the_key() {
        let cfg = BuildersConfig {
            // The percentage typo the range check exists for.
            load_factor: Some(200.0),
            ..BuildersConfig::default()
        };
        let err = cfg.validate().expect_err("200x cores is not a multiplier");
        let msg = format!("{err}");
        assert!(msg.contains("builders.load_factor"), "{msg}");
        assert!(
            msg.contains("200"),
            "the message must show the value: {msg}"
        );
    }

    #[test]
    fn a_zero_load_factor_is_refused_naming_the_key() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let cfg = BuildersConfig {
                load_factor: Some(bad),
                ..BuildersConfig::default()
            };
            let err = cfg
                .validate()
                .expect_err("a factor that refuses every builder forever is not a config");
            assert!(format!("{err}").contains("builders.load_factor"), "{err}");
        }
    }

    #[test]
    fn an_out_of_range_memory_floor_is_refused_naming_the_key() {
        let cfg = BuildersConfig {
            // Bytes written into a megabyte key.
            free_memory_floor_mb: Some(8 * 1024 * 1024 * 1024),
            ..BuildersConfig::default()
        };
        let err = cfg
            .validate()
            .expect_err("a floor above any real host's RAM");
        assert!(
            format!("{err}").contains("builders.free_memory_floor_mb"),
            "{err}"
        );
    }

    #[test]
    fn an_empty_slot_pool_root_is_refused_naming_the_key() {
        let cfg = BuildersConfig {
            slot_pool_root: Some("   ".to_string()),
            ..BuildersConfig::default()
        };
        let err = cfg.validate().expect_err("a blank path is not a pool root");
        assert!(
            format!("{err}").contains("builders.slot_pool_root"),
            "{err}"
        );
    }

    /// #8261 critic round: `deny_unknown_fields` on this section made ONE typo
    /// fail the whole `MpmConfig` parse, and `load` answers a parse failure with
    /// `MpmConfig::default()` — so a misspelled `load_factorr` discarded every
    /// other section the operator had written, and `max_concurrent` with it.
    /// Fails with `deny_unknown_fields` restored.
    #[test]
    fn a_typo_under_builders_does_not_discard_the_rest_of_the_config() {
        let root = tempfile::tempdir().expect("temp root");
        std::fs::write(
            root.path().join("config.toml"),
            "[agents]\nsources = [\"bundled\"]\n\n\
             [builders]\nmax_concurrent = 6\nload_factorr = 2.0\n",
        )
        .expect("write config");

        let cfg = crate::core::config::MpmConfig::load(root.path());

        assert_eq!(
            cfg.agents.sources,
            vec!["bundled".to_string()],
            "an unrelated section must survive a typo under [builders]"
        );
        assert_eq!(
            cfg.builders.max_concurrent,
            Some(6),
            "the keys spelled correctly beside the typo must survive it too"
        );
    }

    #[test]
    fn the_four_keys_round_trip_from_toml() {
        let cfg = toml::from_str::<BuildersConfig>(
            "max_concurrent = 6\n\
             load_factor = 1.75\n\
             free_memory_floor_mb = 4096\n\
             slot_pool_root = \"/pools\"\n",
        )
        .expect("all four keys parse");
        assert_eq!(cfg.validate(), Ok(()));
        assert_eq!(cfg.effective_max_concurrent(MemoryTier::Medium), 6);
        assert_eq!(cfg.effective_load_factor(), 1.75);
        assert_eq!(cfg.effective_free_memory_floor_bytes(), 4096 * 1024 * 1024);
        assert_eq!(
            cfg.effective_slot_pool_root(std::path::Path::new("/home/x")),
            std::path::Path::new("/pools"),
        );
    }

    #[test]
    fn undetectable_ram_falls_back_to_the_medium_tier() {
        assert_eq!(
            MemoryTier::from_total_ram_mb(UNDETECTABLE_RAM_MB),
            MemoryTier::Medium,
            "an unreadable host must not resolve to the largest tier"
        );
    }

    /// Criterion 9's host-root half, which is a property of the code rather
    /// than of a file: the cap is loaded from the HOST root only. The
    /// effective-config loaders fold the project's `.trusty-mpm.toml` and
    /// `TrustyToolsConfig`'s per-project YAML on top, and a project that could
    /// raise its own cap would reintroduce the per-scope rule #6892 replaces.
    /// Source-text guards are the established idiom here — see
    /// `launch_paths_prepare_through_the_isolated_seam`.
    #[test]
    fn builder_cap_resolves_through_load_default_only() {
        const SRC: &str = include_str!("builders.rs");
        // Scan the PRODUCTION half only, with comment lines dropped: the module
        // doc and this test both NAME the forbidden loaders in order to say they
        // must not be called, and a guard that trips on its own error message
        // would have to be deleted to pass.
        let (production, _) = SRC
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("this file ends in a #[cfg(test)] mod tests block");
        let code: String = production
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect();
        assert!(
            code.contains("MpmConfig::load_default()"),
            "the cap must be read from ~/.trusty-mpm/config.toml"
        );
        for forbidden in ["load_effective(", "load_effective_default("] {
            assert!(
                !code.contains(forbidden),
                "no project layer may override builders.max_concurrent (#6892), \
                 but this file calls {forbidden}"
            );
        }
        for layer in ["ProjectLevelConfig", "TrustyToolsConfig"] {
            assert!(
                !code.contains(layer),
                "the project layers must not be consulted here at all, but {layer} appears"
            );
        }
    }

    /// Criterion 9's other half, proved against the schema that would have to
    /// accept the key. `.trusty-mpm.toml` parses with `deny_unknown_fields`, so
    /// a `[builders]` section there is REJECTED — the operator learns
    /// immediately rather than believing a cap that never applied.
    #[test]
    fn project_config_rejects_a_builders_section() {
        let err = crate::core::project_config::ProjectLevelConfig::from_toml(
            "[builders]\nmax_concurrent = 9\n",
            std::path::Path::new("/repo/.trusty-mpm.toml"),
        )
        .expect_err("a project file must not be able to set the machine's builder cap");
        assert!(format!("{err}").contains(".trusty-mpm.toml"), "{err}");
    }
}
