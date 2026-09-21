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

use std::path::Path;

use serde::{Deserialize, Serialize};
use trusty_common::machine_tier::{MemoryTier, detect_total_ram_mb};

use crate::core::config::MpmConfig;
use crate::core::manifest::framework::detected_stack_engineers;

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

/// The `[builders]` section of `~/.trusty-mpm/config.toml`.
///
/// Why: the tier-derived default is a guess about a machine, and an operator
/// who has measured their own knows better. This section is that override, and
/// it is the only one — see the module doc for why no project layer may set it.
/// What: one optional key. An absent `[builders]` section, and an absent
/// `max_concurrent` within a present one, both leave
/// [`tier_default_max_concurrent`] in charge.
/// Test: `absent_section_uses_the_tier_default`, `an_explicit_cap_wins`,
/// `an_explicit_zero_admits_no_builder`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildersConfig {
    /// How many builds may run on this machine at once.
    ///
    /// `None` (the default) → [`tier_default_max_concurrent`] for the host's
    /// detected [`MemoryTier`]. `Some(n)` → exactly `n`, including `0`, which
    /// admits no builder at all: an operator who writes that has said something
    /// deliberate, and silently reading it as "unlimited" would invert their
    /// meaning at the one setting where being wrong costs the whole machine.
    pub max_concurrent: Option<u32>,
    /// How long a freshly dispatched agent holds a slot before its build shows
    /// up in the process table (#8297).
    ///
    /// `None` → [`DEFAULT_GRACE_SECS`]. `Some(0)` disables the grace lease
    /// entirely, which counts running compilers and nothing else — a deliberate
    /// choice for a host that would rather over-admit than delay a dispatch.
    pub grace_secs: Option<u64>,
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

    /// This section's grace window, with the shipped default folded in.
    ///
    /// Why: same split as [`Self::effective_max_concurrent`] — the precedence
    /// is assertable without a home directory or a config file.
    /// What: `grace_secs` when set, `0` included, else [`DEFAULT_GRACE_SECS`].
    /// Test: `an_absent_grace_window_uses_the_default`,
    /// `an_explicit_zero_grace_counts_only_running_compilers`.
    #[must_use]
    pub fn effective_grace_secs(&self) -> u64 {
        self.grace_secs.unwrap_or(DEFAULT_GRACE_SECS)
    }
}

/// How long a dispatch holds a slot before its compiler has to appear (#8297).
///
/// Why: the process table cannot see a build that has not started. An agent
/// dispatched at T spends its first minutes reading files, and a cap counting
/// only live compilers would admit every dispatch in that window — the exact
/// 2026-08-08 shape, where several sessions each believing they were under the
/// limit produced six concurrent `cargo` builds and crashed the host. The grace
/// lease is what closes that gap: a fresh dispatch counts as a build until one
/// of its own appears (which CONVERTS the lease rather than adding to it) or
/// the window passes without one.
/// What: five minutes, in seconds. Above the slowest observed dispatch-to-first-
/// `rustc` here (~90 s, dominated by the agent reading its brief) with room for
/// a cold worktree, and short enough that an agent which never compiles frees
/// the slot inside one PM turn.
/// Test: `an_absent_grace_window_uses_the_default`.
pub const DEFAULT_GRACE_SECS: u64 = 300;

/// Detected stack stems whose toolchain runs a compiler.
///
/// Why: the grace lease must be taken in a project that COULD start a build and
/// nowhere else, or every research and ticketing dispatch into a TypeScript
/// repository would hold a machine slot for five minutes — the adaptive-crm
/// refusal in a new shape. The owner ruling of 2026-09-21 fixes the axis: the
/// question is asked of the PROJECT's detected stack, never of the agent type.
/// What: matched against the stems
/// [`detected_stack_engineers`] reports, which are the bundled manifest's own
/// `language`/`framework` names. The roster ships no C/C++ stem; when one is
/// added it belongs here. `elixir`/`phoenix` do not — the ruling names phoenix
/// among the stacks the cap must never hold.
/// Test: `a_rust_project_compiles`, `a_python_project_does_not_compile`,
/// `every_compiled_stem_is_a_detectable_stack`.
// #8297: project-level, never agent type.
const COMPILED_STACK_STEMS: &[&str] = &[
    "dart-engineer",
    "dotnet-engineer",
    "golang-engineer",
    "java-engineer",
    "rust-engineer",
    "tauri-engineer",
];

/// Could a dispatch into `project_dir` start a compiler at all (#8297)?
///
/// Why: the one gate on the grace lease. A project whose markers name no
/// compiled stack has no build for a fresh dispatch to be holding a slot
/// against, and charging it one is what refused a `terraform fmt` dispatch in
/// adaptive-crm for hours.
/// What: `true` when [`detected_stack_engineers`] reports any stem in
/// [`COMPILED_STACK_STEMS`]. An UNDETECTED project answers `false`: nothing is
/// counted and — because the cap only ever denies what it counts — nothing is
/// denied either, so the answer is consistent in both directions. Detection is
/// memoized per project by `core::manifest::detect_memo`, so the repeated calls
/// one claim makes across sessions pay for one walk.
/// Test: `a_rust_project_compiles`, `a_python_project_does_not_compile`,
/// `an_unmarked_project_does_not_compile`.
#[must_use]
pub fn project_stack_compiles(project_dir: &Path) -> bool {
    detected_stack_engineers(project_dir)
        .engineers
        .iter()
        .any(|stem| COMPILED_STACK_STEMS.contains(&stem.as_str()))
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

/// This host's effective grace window (#8297).
///
/// Why: resolved in the counting process for the same reason the cap is — a
/// `tm` older than the daemon must not argue for a window the live leases were
/// not admitted under.
/// What: [`MpmConfig::load_default`] — the host root — through
/// [`BuildersConfig::effective_grace_secs`]. Never an effective-config loader;
/// see the module doc.
/// Test: `builder_cap_resolves_through_load_default_only` (a source-text guard
/// on this file), plus the pure cases on
/// [`BuildersConfig::effective_grace_secs`].
#[must_use]
pub fn resolve_grace_secs() -> u64 {
    MpmConfig::load_default().builders.effective_grace_secs()
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

    /// #8297: the dispatch-before-build window has a shipped default.
    #[test]
    fn an_absent_grace_window_uses_the_default() {
        let cfg = BuildersConfig::default();
        assert_eq!(cfg.grace_secs, None);
        assert_eq!(cfg.effective_grace_secs(), DEFAULT_GRACE_SECS);
        assert_eq!(DEFAULT_GRACE_SECS, 300);
    }

    /// `0` turns the grace lease off — running compilers and nothing else.
    #[test]
    fn an_explicit_zero_grace_counts_only_running_compilers() {
        let cfg = BuildersConfig {
            grace_secs: Some(0),
            ..BuildersConfig::default()
        };
        assert_eq!(cfg.effective_grace_secs(), 0);
    }

    /// A Cargo project can start a compiler, so a dispatch into it may hold a
    /// grace lease (#8297).
    #[test]
    fn a_rust_project_compiles() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n")
            .expect("write marker");
        assert!(project_stack_compiles(dir.path()));
    }

    /// The adaptive-crm case: an interpreted project holds no slot at all.
    #[test]
    fn a_python_project_does_not_compile() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("pyproject.toml"), "[project]\nname='x'\n")
            .expect("write marker");
        std::fs::write(dir.path().join("package.json"), "{}").expect("write marker");
        assert!(
            !project_stack_compiles(dir.path()),
            "python/typescript markers must not buy a grace lease"
        );
    }

    /// An undetected project is not counted, and therefore never denied.
    #[test]
    fn an_unmarked_project_does_not_compile() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!project_stack_compiles(dir.path()));
    }

    /// Every compiled stem must be a stem detection can actually report, or the
    /// list silently stops matching after a manifest rename.
    #[test]
    fn every_compiled_stem_is_a_detectable_stack() {
        let categories = crate::core::manifest::framework::framework_agent_categories()
            .expect("the bundled manifest is valid");
        for stem in COMPILED_STACK_STEMS {
            assert!(
                categories
                    .language
                    .iter()
                    .chain(categories.framework.iter())
                    .any(|e| e.stem == *stem),
                "{stem} is not a stem the bundled manifest can detect"
            );
        }
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
