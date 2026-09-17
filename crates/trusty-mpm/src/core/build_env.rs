//! The `build:` section of `~/.trusty-tools/trusty-mpm/config.yaml` (#6868).
//!
//! Why: every tm-provisioned agent worktree gets its own empty `target/`, so a
//! dispatched engineer pays a cold full-workspace build before it can run one
//! gate. Measured on the 16-core dev host on 2026-09-16: ~200 s cold in a fresh
//! worktree, 103 s when `CARGO_TARGET_DIR` pointed at a warm shared target
//! directory from another worktree, 17 s from that same path a second time.
//! sccache was neutral on the same tree, because a path-crate-heavy workspace
//! builds incrementally and incremental artifacts are not cacheable. The
//! machine-level knob that actually pays is therefore a shared target directory
//! per repo, and it needs a declarative home rather than an environment
//! variable each brief has to remember.
//!
//! What: [`BuildConfig`] is the YAML shape, [`ResolvedBuildEnv`] is what the
//! defaults resolve to, and [`paste_line`] renders the one line a PM can paste
//! verbatim into an engineer brief. Cargo's own target lock serialises
//! concurrent builds sharing the directory, which is the DESIRED behaviour here
//! — six concurrent cold builds crashed that host on 2026-08-08.
//!
//! Nothing in this module writes. The repair that creates the directory and
//! seeds the section lives in [`crate::core::build_env_repair`]; the `tm doctor`
//! row that reports it lives in `daemon::doctor_rust_build_env`.
//!
//! Test: `build_env_tests.rs`.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use trusty_common::github_path::GithubPath;
use trusty_common::workspace_layout::expand_tilde;

/// Directory under `~/.trusty-tools/` that holds every repo's shared target dir.
///
/// Why: one literal, so the default resolver and the doc that documents it
/// cannot drift.
/// What: `"cargo-target"`, joined as
/// `~/.trusty-tools/cargo-target/<owner>/<repo>`.
/// Test: `rust_build_env_defaults_target_dir_from_git_remote`.
pub const CARGO_TARGET_SUBDIR: &str = "cargo-target";

/// Floor on the resolved job count.
///
/// Why: half of a 2-core or 3-core host rounds to 1 or 0, and a zero-job cargo
/// invocation is a refusal, not a slow build. Two is the smallest count that is
/// still a build.
/// Test: `rust_build_env_defaults_build_jobs_to_half_cores_min_two`.
pub const MIN_BUILD_JOBS: u32 = 2;

/// The top-level YAML key this module owns.
///
/// Why: the repair decides "is the section absent?" by this exact key, and the
/// doctor row names it in its remediation. One literal for both.
/// Test: `rust_build_env_repair_never_overwrites_existing_build_keys`.
pub const BUILD_SECTION_KEY: &str = "build";

/// The `build:` section of trusty-mpm's crate config.
///
/// Why: the three machine-level facts a Rust build needs — where to put shared
/// artifacts, how many jobs the host tolerates, and whether sccache is wanted —
/// belong in the file the operator already edits rather than in each brief.
/// Every field is optional: an absent section resolves to the shipped defaults,
/// and the doctor row never fails on one.
/// What: `cargo_target_dir` may carry a leading `~`;
/// `build_jobs` of `0` is treated as absent (a build with no jobs is not a
/// preference an operator can have held); `sccache` defaults to `false`,
/// because wiring a `rustc-wrapper` is an operator decision tm never takes on
/// its own.
///
/// ```yaml
/// build:
///   cargo_target_dir: ~/.trusty-tools/cargo-target/bobmatnyc/trusty-tools
///   build_jobs: 8
///   sccache: false
/// ```
///
/// Test: `build_config_yaml_round_trips`, `an_absent_section_resolves_defaults`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
// #5204: adding a settings field must stay non-breaking under cargo-semver-checks.
#[non_exhaustive]
pub struct BuildConfig {
    /// Shared `CARGO_TARGET_DIR` for this machine. A leading `~` expands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_target_dir: Option<String>,
    /// `CARGO_BUILD_JOBS` for this machine. Absent or `0` → half the cores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_jobs: Option<u32>,
    /// Whether the operator wants `RUSTC_WRAPPER=sccache` on cargo invocations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sccache: Option<bool>,
}

/// What the `build:` section resolves to once defaults are applied.
///
/// Why: resolution is separated from the shape so the fallback rules are
/// asserted once — the pattern
/// [`crate::core::startup_context::ResolvedStartupContext`] established.
/// What: the three values [`paste_line`] renders and the doctor row reports.
/// Test: `rust_build_env_paste_line_matches_resolved_values`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBuildEnv {
    /// Absolute shared target directory.
    pub cargo_target_dir: PathBuf,
    /// Resolved `CARGO_BUILD_JOBS`.
    pub build_jobs: u32,
    /// Whether sccache is requested.
    pub sccache: bool,
    /// True when `cargo_target_dir` came from the config rather than the
    /// git-remote default — the doctor row says which, so an operator reading
    /// an unexpected path knows where to change it.
    pub target_dir_configured: bool,
}

/// Why the build environment could not be resolved at all.
///
/// Why: the default target directory is derived from the repo's `origin`
/// remote, and there is no honest fallback when that cannot be read — a shared
/// bucket for "every repo with no remote" would collide two unrelated
/// workspaces onto one cargo lock. The check reports this as UNDETERMINED
/// rather than picking a path (Fail-Open Check: a probe that learned nothing
/// must not read healthy).
/// What: one variant today.
/// Test: `resolution_without_a_remote_or_a_config_is_an_error`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildEnvError {
    /// No `build.cargo_target_dir` was configured and the project has no
    /// parseable `origin` remote to derive `<owner>/<repo>` from.
    #[error(
        "no `build.cargo_target_dir` is configured and the project has no parseable `origin` \
         remote to derive `<owner>/<repo>` from"
    )]
    NoRepoIdentity,
}

/// The engineer stem whose presence means "this project is Rust".
///
/// Why: the bundled manifest keys stack detection by engineer stem, so this is
/// the same token `core::stack_profile` renders into the PM prompt — the row
/// and the prompt can never disagree about what a Rust project is.
/// Test: `rust_build_env_skips_on_a_non_rust_project`.
pub const RUST_ENGINEER: &str = "rust-engineer";

/// Does `project_dir` detect as a Rust project?
///
/// Why: the `rust_build_env` doctor row and its `--fix` arm must gate on the
/// SAME answer, or `--fix` creates a cargo target directory for a project the
/// row said was not applicable.
/// What: `Some(true)` / `Some(false)` from
/// [`crate::core::manifest::framework::detected_stack_engineers`], and `None`
/// when a resource cap cut detection short — a scan that stopped early has not
/// shown the project is free of Rust, so callers fail closed on it rather than
/// reading `None` as "no".
/// Test: `rust_build_env_skips_on_a_non_rust_project`.
pub fn project_is_rust(project_dir: &Path) -> Option<bool> {
    let detection = crate::core::manifest::framework::detected_stack_engineers(project_dir);
    if detection.engineers.iter().any(|e| e == RUST_ENGINEER) {
        return Some(true);
    }
    // #7781: `truncated` is the fail-closed signal.
    if detection.truncated {
        return None;
    }
    Some(false)
}

/// Cores this host reports.
///
/// Why: the one line in this module that asks the machine anything, kept
/// separate so [`default_build_jobs`] stays pure and assertable at every
/// boundary.
/// What: `std::thread::available_parallelism`, falling back to 1 when the
/// platform declines to answer — which floors the job count at
/// [`MIN_BUILD_JOBS`] rather than at zero.
/// Test: covered through [`default_build_jobs`]'s own tests.
pub fn host_cores() -> usize {
    std::thread::available_parallelism().map_or(1, NonZeroUsize::get)
}

/// Half the cores, never below [`MIN_BUILD_JOBS`].
///
/// Why: a Rust build is CPU and RAM bound and this host runs more than one
/// agent at a time; handing every dispatch the full core count is what produced
/// the 2026-08-08 crash (six concurrent cold builds, load average 36). Half
/// leaves room for a sibling without serialising the fleet.
/// What: `max(cores / 2, MIN_BUILD_JOBS)`, saturating rather than wrapping on a
/// core count that does not fit a `u32`.
/// Test: `rust_build_env_defaults_build_jobs_to_half_cores_min_two`.
pub fn default_build_jobs(cores: usize) -> u32 {
    let half = u32::try_from(cores / 2).unwrap_or(u32::MAX);
    half.max(MIN_BUILD_JOBS)
}

/// The shared target directory a repo gets when nothing is configured.
///
/// Why: keyed by `<owner>/<repo>` so two checkouts of the SAME repo — the main
/// checkout and every agent worktree — share one warm directory, while two
/// different repos never contend on one cargo lock.
/// What: `<home>/.trusty-tools/cargo-target/<owner>/<repo>`. `home` is explicit
/// so every test is hermetic.
/// Test: `rust_build_env_defaults_target_dir_from_git_remote`.
pub fn default_cargo_target_dir_at(home: &Path, identity: &GithubPath) -> PathBuf {
    home.join(trusty_common::crate_config::TRUSTY_TOOLS_DIR)
        .join(CARGO_TARGET_SUBDIR)
        .join(&identity.owner)
        .join(&identity.repo)
}

/// Apply the defaults to a (possibly absent) `build:` section.
///
/// Why: the one place the precedence is decided — **config value > derived
/// default** — so the doctor row, the repair, and the paste line can never
/// disagree about what this machine is actually configured to do.
/// What: `build_jobs` falls back to [`default_build_jobs`] (a configured `0` is
/// treated as absent); `sccache` falls back to `false`; `cargo_target_dir`
/// falls back to [`default_cargo_target_dir_at`] and needs `identity`, whose
/// absence is [`BuildEnvError::NoRepoIdentity`] rather than a guessed path.
///
/// # Errors
///
/// [`BuildEnvError::NoRepoIdentity`] when neither a configured directory nor a
/// repo identity is available.
///
/// Test: `an_absent_section_resolves_defaults`,
/// `a_configured_target_dir_wins_over_the_remote_default`,
/// `resolution_without_a_remote_or_a_config_is_an_error`,
/// `a_zeroed_build_jobs_falls_back_to_the_default`.
pub fn resolve_build_env(
    config: Option<&BuildConfig>,
    home: &Path,
    identity: Option<&GithubPath>,
    cores: usize,
) -> Result<ResolvedBuildEnv, BuildEnvError> {
    let build_jobs = config
        .and_then(|c| c.build_jobs)
        .filter(|jobs| *jobs > 0)
        .unwrap_or_else(|| default_build_jobs(cores));
    let sccache = config.and_then(|c| c.sccache).unwrap_or(false);
    let configured = config.and_then(|c| c.cargo_target_dir.as_deref());
    let cargo_target_dir = match configured {
        Some(template) => expand_tilde(template, home),
        None => {
            let identity = identity.ok_or(BuildEnvError::NoRepoIdentity)?;
            default_cargo_target_dir_at(home, identity)
        }
    };
    Ok(ResolvedBuildEnv {
        cargo_target_dir,
        build_jobs,
        sccache,
        target_dir_configured: configured.is_some(),
    })
}

/// The line a PM pastes verbatim into an engineer brief.
///
/// Why (#6868 closure condition 3): the whole point of resolving these values
/// is that a dispatched agent prefixes them inline on every cargo invocation —
/// an agent's shell environment does not persist between tool calls, so an
/// `export` protects nothing. One line, already correct for this machine,
/// removes the step where a brief author remembers three variable names.
/// What: `CARGO_TARGET_DIR=<dir> CARGO_BUILD_JOBS=<n> [RUSTC_WRAPPER=sccache]
/// SKIP_UI_BUILD=1`, with the wrapper present only when the config asked for
/// sccache.
/// Test: `rust_build_env_paste_line_matches_resolved_values`,
/// `the_paste_line_omits_the_wrapper_when_sccache_is_off`.
pub fn paste_line(env: &ResolvedBuildEnv) -> String {
    let wrapper = if env.sccache {
        " RUSTC_WRAPPER=sccache"
    } else {
        ""
    };
    format!(
        "CARGO_TARGET_DIR={} CARGO_BUILD_JOBS={}{wrapper} SKIP_UI_BUILD=1",
        env.cargo_target_dir.display(),
        env.build_jobs
    )
}

/// The `build:` block `tm doctor --fix` seeds into an absent section.
///
/// Why: the repair writes the RESOLVED values rather than an empty stub, so an
/// operator who runs `--fix` once can afterwards read their machine's actual
/// settings out of the file and edit them there.
/// What: a four-line YAML block ending in a newline, with the directory
/// single-quoted so a path containing `#` or `:` still parses. It is rendered
/// as text rather than serialised from [`BuildConfig`] because the repair
/// APPENDS it to a file whose other keys and comments must survive verbatim.
/// Test: `rust_build_env_repair_creates_dir_and_writes_defaults_once`,
/// `the_seeded_block_parses_back_to_the_resolved_values`.
pub fn default_build_section_yaml(env: &ResolvedBuildEnv) -> String {
    format!(
        "{BUILD_SECTION_KEY}:\n  cargo_target_dir: '{}'\n  build_jobs: {}\n  sccache: {}\n",
        env.cargo_target_dir
            .display()
            .to_string()
            .replace('\'', "''"),
        env.build_jobs,
        env.sccache
    )
}

#[cfg(test)]
#[path = "build_env_tests.rs"]
mod tests;
