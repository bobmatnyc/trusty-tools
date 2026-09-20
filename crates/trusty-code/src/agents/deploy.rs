//! Materialize the embedded agent roster to `<project>/.trusty-code/agents/`
//! with a manifest and recorded provenance (#2074, epic #2892).
//!
//! Why: before this, `crate::assets::DEFAULT_AGENTS` existed only in memory.
//! An operator could not read the prompt an agent actually runs, diff it, or
//! edit it, and nothing on disk recorded which files Trusty Code wrote versus
//! which the project owns. #2074 requires source provenance for every shipped
//! role, and provenance needs a file and a ledger to attach to.
//!
//! What: [`ensure_roster_deployed`] stages the compiled-in agent sources into a
//! throwaway directory and hands that directory to
//! `trusty_agents_common::agents::deployer::deploy_agents_filtered` — the same
//! writer trusty-mpm's `~/.claude/agents/` deploy uses, unchanged. That
//! deployer already takes `source_dir`/`target_dir` as plain parameters, so
//! reusing it needed no shared-library change. It brings the strict-YAML
//! validation of every composed agent, the per-agent compose-failure isolation,
//! the `.trusty-mpm-manifest.json` ledger with a recorded checksum and
//! [`Origin`], and the refusal to proceed on a corrupt ledger.
//!
//! ## Three policies this module adds on top of the shared deployer
//!
//! **A compatibility root that currently wins is never shadowed.** `.claude/`
//! and `.open-mpm/` remain readable discovery INPUTS (#5426), and
//! `crate::paths::agents_dir` prefers `.trusty-code/agents` over both. Writing
//! the roster into `.trusty-code/agents` while a project's `.claude/agents/`
//! holds its real catalog would silently demote that catalog, so the deploy is
//! skipped in that case — [`SkipReason::CompatRootWins`].
//!
//! **A hand-edited deployed file is authoritative.** The shared deployer treats
//! a checksum mismatch on an [`Origin::Bundled`] entry as corruption and repairs
//! it (#4408) — correct for trusty-mpm's machine-global `~/.claude/agents/`,
//! wrong for a project-local directory whose whole contract is
//! `load_all_agents`' "disk wins, never merged". This module therefore selects
//! against the ledger before deploying: a tracked file whose on-disk content no
//! longer matches its recorded checksum is deselected, so the deployer never
//! composes or writes it. Pristine files still refresh, and an UNTRACKED file is
//! left to the deployer's own adopt-or-skip branch. No deployer change was
//! needed for either policy. Since #7779 the policy covers REAL files only: a
//! symlinked entry in the agents directory refuses the deploy instead of being
//! read through, because the pinned handle opens every entry `O_NOFOLLOW` (the
//! `write_dir` module header states why that is uniform rather than per-tree).
//!
//! **Every write reaches the project through a pinned directory handle.** The
//! shared deployer writes through plain paths — correct for trusty-mpm's
//! machine-global `~/.claude/agents/`, where a symlinked `~/.claude` is a
//! legitimate setup, and wrong for a project directory a racing writer can swap
//! (#7779). It therefore runs against a private scratch copy, and the ledger and
//! the files it produced are published back `openat`-relative to
//! [`crate::paths::write_dir::NativeWriteDir`]. The project's own ledger lock is
//! taken through that same handle, so cross-process serialisation is unchanged.
//!
//! Test: `agents::deploy::deploy_tests::*`,
//! `tests/roster_deploy_e2e.rs`.
//!
//! [`Origin`]: trusty_agents_common::agents::manifest::Origin
//! [`Origin::Bundled`]: trusty_agents_common::agents::manifest::Origin::Bundled

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use trusty_agents_common::agents::builder::AgentBuildError;
use trusty_agents_common::agents::deployer::{DeployResult, deploy_agents_filtered};
use trusty_agents_common::agents::manifest::{AgentManifest, MANIFEST_FILE, ManifestLoad};

use crate::agents::skill_refs::SKILL_REFS_DIRNAME;
use crate::assets::{DEFAULT_AGENTS, EMBEDDED_TM_AGENT_SOURCES, EmbeddedAgent};
use crate::paths::write_dir::NativeWriteDir;
use crate::paths::{self, WriteTargetError};

/// The `agents` subdirectory name, shared by every configuration root.
///
/// Why: named once so the write target cannot drift from the read target
/// `crate::paths::agents_dir` resolves.
/// What: `"agents"`.
/// Test: `roster_target_is_under_the_native_config_dir`.
pub const AGENTS_DIRNAME: &str = "agents";

/// Why a roster deploy could not run.
///
/// Why: each variant has a different fix, and collapsing them into one string
/// would hide which. A refused write target means the project layout is wrong; a
/// corrupt ledger means ownership cannot be established and must be repaired
/// before anything is written.
/// What: the four failure modes [`ensure_roster_deployed`] can return.
/// Test: `corrupt_manifest_is_reported_and_nothing_is_written`.
#[derive(Debug, thiserror::Error)]
pub enum RosterDeployError {
    /// The computed target is not a legal Trusty Code write target.
    #[error("refusing to materialize the agent roster: {0}")]
    WriteTarget(#[from] WriteTargetError),
    /// The deployed-agent ledger exists but could not be read as one.
    #[error(
        "the deployed-agent manifest at {path} could not be read ({detail}); \
         refusing to deploy, because treating it as empty would reclassify every \
         managed file as user-owned. Repair or delete that file to continue."
    )]
    ManifestCorrupt {
        /// The ledger's path.
        path: PathBuf,
        /// The underlying parse or I/O failure.
        detail: String,
    },
    /// Staging the compiled-in sources into a scratch directory failed.
    #[error("staging the embedded agent roster failed: {0}")]
    Stage(#[source] std::io::Error),
    /// A file the deploy produced could not be read back out of the scratch
    /// directory, so it could not be published (#7779).
    #[error(
        "the deployed agent roster could not be published: {name} was not readable in the \
         scratch directory ({source}); refusing to publish, because a ledger recording a \
         file that was never written reports a deploy that did not happen."
    )]
    Publish {
        /// The scratch file that could not be read.
        name: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The shared deployer itself failed.
    #[error("deploying the embedded agent roster failed: {0}")]
    Deploy(#[source] AgentBuildError),
}

/// Why a deploy was skipped rather than attempted.
///
/// Why: "nothing was written" is a legitimate outcome the caller must be able to
/// log accurately, and it is not an error.
/// What: the single skip condition today — a compatibility root currently
/// resolves the agents directory.
/// Test: `deploy_is_skipped_when_claude_agents_dir_wins`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// `.claude/agents/` or `.open-mpm/agents/` currently wins discovery.
    CompatRootWins,
}

/// What one [`ensure_roster_deployed`] call did.
///
/// Why: the caller logs a different line for each, and a test asserts on which
/// branch ran rather than inferring it from the filesystem.
/// What: either a skip with its reason, or the deployer's own
/// [`DeployResult`] alongside the directory it wrote into.
/// Test: `fresh_project_materializes_the_whole_roster`,
/// `deploy_is_skipped_when_claude_agents_dir_wins`.
#[derive(Debug)]
pub enum RosterDeploy {
    /// Nothing was written.
    Skipped(SkipReason),
    /// The shared deployer ran against `target`.
    Deployed {
        /// `<project>/.trusty-code/agents`.
        target: PathBuf,
        /// The deployer's per-file outcome lists. Boxed because
        /// `DeployResult` is two orders of magnitude larger than the skip
        /// variant, which `clippy::large_enum_variant` denies.
        result: Box<DeployResult>,
    },
}

/// What the deployed-agent ledger looks like right now.
///
/// Why: `tcode paths show` answers "which root wins"; after #2074 it must also
/// answer "is there a deployed roster, and is its ledger readable" — the
/// question an operator debugging a stale or hand-edited agent asks next.
/// What: absent (nothing deployed yet), present with a managed-file count, or
/// corrupt with the underlying detail.
/// Test: `manifest_status_reports_absent_then_present`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestStatus {
    /// No ledger file exists — the roster has not been deployed here.
    Absent,
    /// The ledger parsed; `managed` files are tracked.
    Present {
        /// How many deployed files the ledger tracks.
        managed: usize,
    },
    /// The ledger exists but could not be read.
    Corrupt {
        /// The underlying parse or I/O failure.
        detail: String,
    },
}

impl ManifestStatus {
    /// A stable lowercase token for logs, JSON diagnostics, and CLI output.
    ///
    /// Why: the human and JSON renderings of `tcode paths show` must agree, and
    /// a `Debug` rendering is not a wire contract.
    /// What: `"absent"`, `"present"`, `"corrupt"`.
    /// Test: `manifest_status_reports_absent_then_present`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Present { .. } => "present",
            Self::Corrupt { .. } => "corrupt",
        }
    }
}

/// The ledger path and its current state for a project.
///
/// Why: `tcode paths show` needs both, and deriving the path a second time at
/// the call site would be a second implementation of the layout rule.
/// What: `<project>/.trusty-code/agents/.trusty-mpm-manifest.json` and what
/// reading it yields. Absence is distinguished from an empty ledger by testing
/// the file itself — `AgentManifest::load_checked` reports both as `Ok`.
/// Test: `manifest_status_reports_absent_then_present`,
/// `manifest_status_reports_corrupt`.
pub fn roster_manifest_status(project_root: &Path) -> (PathBuf, ManifestStatus) {
    let path = roster_target(project_root).join(MANIFEST_FILE);
    if !path.exists() {
        return (path, ManifestStatus::Absent);
    }
    let status = match AgentManifest::load_checked(&roster_target(project_root)) {
        ManifestLoad::Ok(m) => ManifestStatus::Present {
            managed: m.managed.len(),
        },
        ManifestLoad::Corrupt(detail) => ManifestStatus::Corrupt { detail },
    };
    (path, status)
}

/// `<project>/.trusty-code/agents` — the only directory this module writes.
///
/// Why: the write target is deliberately NOT `crate::paths::agents_dir`'s
/// result, which may legitimately resolve into `.claude/`. The read path and the
/// write path are different functions on purpose (#5426).
/// What: [`crate::paths::native_child`] for [`AGENTS_DIRNAME`].
/// Test: `roster_target_is_under_the_native_config_dir`.
pub fn roster_target(project_root: &Path) -> PathBuf {
    paths::native_child(project_root, AGENTS_DIRNAME)
}

/// Materialize the embedded roster into `<project>/.trusty-code/agents/`.
///
/// Why: #2074's provenance requirement — every shipped role needs a file, a
/// recorded checksum, and an [`Origin`] on disk, not just an in-memory
/// projection. `crate::agents::load_embedded_default_agents` stays the zero-disk
/// fallback for a projectless session, which has no project root to write to.
///
/// What, in order:
/// 1. Skip when `.claude/agents/` or `.open-mpm/agents/` currently wins
///    discovery — see this module's docs.
/// 2. PIN the agents directory with [`crate::paths::write_dir::NativeWriteDir`],
///    which applies the unchanged ADR-0044 membership rule and then holds the
///    directory open on a descriptor. Every write below is `openat`-relative to
///    it, so a symlink swapped in afterwards cannot redirect one (#7779).
/// 3. Take the project's ledger lock through the pinned handle, so concurrent
///    `tcode` daemons still serialise on the same sidecar the shared deployer
///    would have used. It is a blocking `LOCK_EX` held across the whole
///    mirror/stage/compose/publish sequence — wider than the shared deployer's,
///    which takes it around the compose only, and deliberately so: the mirror
///    that decides "hand-edited" and the publish that acts on that decision have
///    to see the same directory.
/// 4. Refuse to proceed on a corrupt ledger, naming the file. Never reset it.
/// 5. Stage every compiled-in source into a scratch directory, including the
///    five `BASE-*` templates so the deployer's own `extends:` composer resolves
///    the chains from disk exactly as it does for trusty-mpm.
/// 6. PIN the skill-refs tree — after step 4, so a refused deploy leaves no
///    empty directory behind — and materialize the roster's skill pointers.
/// 7. Deploy into a second scratch directory seeded from the pinned handle,
///    selecting the dispatchable roster only and deselecting any tracked file
///    the user has since edited, then publish the result back through the handle.
///
/// Test: `fresh_project_materializes_the_whole_roster`,
/// `hand_edited_agent_survives_a_second_deploy`,
/// `corrupt_manifest_is_reported_and_nothing_is_written`,
/// `symlinked_roster_file_is_refused_before_any_write`,
/// `deploy_is_skipped_when_claude_agents_dir_wins`,
/// `base_templates_are_never_deployed`,
/// `symlinked_skill_refs_dir_is_refused_before_any_write`,
/// `symlinked_skill_folder_is_refused_before_any_write`,
/// `symlinked_skill_ref_file_is_refused_before_any_write`,
/// `tests/roster_deploy_e2e.rs`.
///
/// [`Origin`]: trusty_agents_common::agents::manifest::Origin
pub fn ensure_roster_deployed(project_root: &Path) -> Result<RosterDeploy, RosterDeployError> {
    if !paths::agents_dir(project_root).source.is_native() {
        return Ok(RosterDeploy::Skipped(SkipReason::CompatRootWins));
    }

    // #7779: both write targets are PINNED here, not merely checked. Opening
    // them creates every missing component with `mkdirat`/`O_NOFOLLOW`, so the
    // `create_dir_all` a racing writer used to beat is gone and every write
    // below resolves against the descriptor rather than the path.
    let agents = NativeWriteDir::open(project_root, Path::new(AGENTS_DIRNAME))?;
    let target = agents.path().to_path_buf();

    // #7779: the shared deployer serialises writers on this sidecar. It now runs
    // against a scratch directory, so the PROJECT's lock is taken here instead —
    // dropping it would silently unserialise two concurrent `tcode` daemons.
    let _ledger = agents.lock_exclusive(&format!("{MANIFEST_FILE}.lock"))?;

    // #7779: the deployer writes through plain paths, which is correct for
    // trusty-mpm's machine-global `~/.claude/agents` but reopens this race on a
    // project directory. It therefore runs against a private scratch copy of the
    // ledger and the roster files it may read, and everything it produced is
    // published back through the pinned handle.
    let shadow = tempfile::tempdir().map_err(RosterDeployError::Stage)?;
    mirror_into_scratch(&agents, shadow.path())?;

    // Establish ownership BEFORE staging or writing anything. The deployer makes
    // the same check under its own lock; doing it here too is what lets a
    // corrupt ledger be reported without a scratch directory ever being built.
    let manifest = match AgentManifest::load_checked(shadow.path()) {
        ManifestLoad::Ok(m) => m,
        ManifestLoad::Corrupt(detail) => {
            return Err(RosterDeployError::ManifestCorrupt {
                path: target.join(MANIFEST_FILE),
                detail,
            });
        }
    };

    let staged = stage_embedded_sources()?;
    let roster: HashSet<&str> = DEFAULT_AGENTS.iter().map(EmbeddedAgent::name).collect();

    // #7779: pinned only now. Opening a handle CREATES its directory, so pinning
    // this one beside the agents dir left an empty `skill-refs/` behind whenever
    // the corrupt-ledger refusal above fired — and `paths::resolve_project_entry`
    // reads an empty readable directory as Usable.
    let skill_refs = NativeWriteDir::open(project_root, Path::new(SKILL_REFS_DIRNAME))?;

    // #7727: the roster's skill pointers resolve to files this project holds.
    super::skill_refs::materialize_skill_refs(&skill_refs)?;
    let result = deploy_agents_filtered(staged.path(), shadow.path(), skill_refs.path(), |stem| {
        roster.contains(stem) && !is_user_edited(&manifest, shadow.path(), stem)
    })
    .map_err(RosterDeployError::Deploy)?;
    publish_from_scratch(&agents, shadow.path(), &result)?;

    Ok(RosterDeploy::Deployed {
        target,
        result: Box::new(result),
    })
}

/// Copy the ledger and every roster file the deployer may read into `scratch`.
///
/// Why: the deployer decides adopt / refresh / preserve by reading the target
/// directory, so running it against an empty scratch tree would reclassify every
/// existing file as new and overwrite a hand-edited agent.
/// What: reads `<MANIFEST_FILE>` and `<name>.md` for every roster name through
/// the PINNED handle — the same descriptor the publish writes back through, so
/// the snapshot and the publish cannot disagree about which directory they mean.
/// Absent entries are simply not copied.
///
/// A SYMLINKED entry refuses the whole deploy rather than being followed, which
/// narrows this module's "a hand-edited deployed file is authoritative" policy
/// to real files (#7779 round 2; rationale in the `write_dir` module header).
/// Test: `agents::deploy::deploy_tests::hand_edited_agent_survives_a_second_deploy`,
/// `agents::deploy::deploy_tests::symlinked_roster_file_is_refused_before_any_write`.
fn mirror_into_scratch(agents: &NativeWriteDir, scratch: &Path) -> Result<(), RosterDeployError> {
    let mut names = vec![MANIFEST_FILE.to_string()];
    names.extend(DEFAULT_AGENTS.iter().map(|a| format!("{}.md", a.name())));
    for name in names {
        if let Some(bytes) = agents.read(&name)? {
            std::fs::write(scratch.join(&name), bytes).map_err(RosterDeployError::Stage)?;
        }
    }
    Ok(())
}

/// Publish what the deploy produced back through the pinned handle.
///
/// Why: the only writes that reach the project directory. Doing them
/// `openat`-relative to the validated descriptor is what makes "the path
/// validated is the path written" true for the agents directory (#7779).
/// What: the ledger (always — an adoption changes it without rewriting a file)
/// plus every file the deployer reported as deployed. Files it left alone are
/// not republished, so a preserved hand-edit is not even rewritten with its own
/// bytes. A swap detected mid-publish aborts with
/// [`WriteTargetError::Unpinned`]; nothing was written outside the directory.
///
/// Every file is READ before any is written. #7779: a `continue` on an
/// unreadable scratch file dropped that file while the ledger published its
/// checksum anyway, so [`ensure_roster_deployed`] returned `Deployed` and
/// [`deploy_and_log`] logged a file that is not on disk — fail-open. Reading
/// first makes the run all-or-nothing up to the first `atomic_write`.
/// Test: `agents::deploy::deploy_tests::fresh_project_materializes_the_whole_roster`,
/// `agents::deploy::deploy_tests::swapped_agents_dir_never_reaches_the_victim`,
/// `agents::deploy::deploy_tests::unreadable_scratch_file_fails_the_publish`.
fn publish_from_scratch(
    agents: &NativeWriteDir,
    scratch: &Path,
    result: &DeployResult,
) -> Result<(), RosterDeployError> {
    let mut names: Vec<&str> = result.deployed.iter().map(String::as_str).collect();
    names.push(MANIFEST_FILE);
    let mut pending = Vec::with_capacity(names.len());
    for name in names {
        let bytes =
            std::fs::read(scratch.join(name)).map_err(|source| RosterDeployError::Publish {
                name: name.to_string(),
                source,
            })?;
        pending.push((name, bytes));
    }
    for (name, bytes) in pending {
        agents.atomic_write(name, &bytes)?;
    }
    Ok(())
}

/// Run [`ensure_roster_deployed`] for a bound project and log what happened.
///
/// Why: two entry points materialize the roster — the daemon's router build and
/// the legacy in-process `run-task` — and both want the same behaviour: deploy
/// if we can, report loudly if we cannot, and never abort the run. A failed
/// deploy is recoverable, because `crate::agents::load_embedded_default_agents`
/// still answers every resolution from memory; aborting would turn a repairable
/// ledger into an unusable harness.
/// What: `None` when `project_root` is `None` (a projectless session writes
/// nothing and keeps the in-memory embed). Otherwise the outcome, with a
/// `tracing::error!` naming the failure when one occurred. #2074.
/// Test: `deploy_and_log_is_a_no_op_without_a_project`,
/// `deploy_and_log_reports_a_corrupt_ledger_without_panicking`.
pub fn deploy_and_log(project_root: Option<&Path>) -> Option<RosterDeploy> {
    let root = project_root?;
    match ensure_roster_deployed(root) {
        Ok(RosterDeploy::Skipped(reason)) => {
            tracing::debug!(
                project = %root.display(),
                ?reason,
                "agent roster not materialized"
            );
            Some(RosterDeploy::Skipped(reason))
        }
        Ok(RosterDeploy::Deployed { target, result }) => {
            tracing::info!(
                target = %target.display(),
                deployed = result.deployed.len(),
                unchanged = result.unchanged.len(),
                preserved = result.skipped.len(),
                failed = result.failed.len(),
                "agent roster materialized (#2074)"
            );
            Some(RosterDeploy::Deployed { target, result })
        }
        Err(e) => {
            tracing::error!(
                project = %root.display(),
                "could not materialize the agent roster; continuing with the \
                 in-memory embedded roster: {e}"
            );
            None
        }
    }
}

/// Whether a tracked deployed file has been edited since Trusty Code wrote it.
///
/// Why: the "hand-edited file is authoritative" policy from this module's docs.
/// The shared deployer would repair such a file (#4408); a project-local
/// directory must preserve it instead.
/// What: `true` only when the ledger tracks `<stem>.md` AND the file on disk no
/// longer matches its recorded checksum. An untracked file returns `false` so
/// the deployer's own adopt-or-skip branch decides; an unreadable or absent file
/// returns `false` so it is (re)deployed.
/// Test: `hand_edited_agent_survives_a_second_deploy`,
/// `untracked_file_is_left_to_the_deployer`.
fn is_user_edited(manifest: &AgentManifest, target: &Path, stem: &str) -> bool {
    let filename = format!("{stem}.md");
    if !manifest.is_managed(&filename) {
        return false;
    }
    match std::fs::read_to_string(target.join(&filename)) {
        Ok(current) => !manifest.checksum_matches(&filename, &current),
        Err(_) => false,
    }
}

/// Write every compiled-in agent source into a throwaway directory.
///
/// Why: the shared deployer composes from a source DIRECTORY, and tcode's roster
/// is a set of `&'static str`s. Staging them is what lets the deployer be reused
/// verbatim instead of growing a second, pre-composed entry point — a shared
/// library change this slice deliberately avoided.
/// What: one `.md` per [`EMBEDDED_TM_AGENT_SOURCES`] entry (keyed by its
/// original filename, so `extends: base-qa` resolves against `BASE-QA.md`) plus
/// one per [`EmbeddedAgent::Direct`] in [`DEFAULT_AGENTS`]. The `TempDir` is
/// returned so the caller keeps it alive across the deploy; dropping it removes
/// the scratch tree.
/// Test: `staged_sources_cover_every_roster_name_and_base_template`.
fn stage_embedded_sources() -> Result<tempfile::TempDir, RosterDeployError> {
    let dir = tempfile::tempdir().map_err(RosterDeployError::Stage)?;
    for (filename, content) in EMBEDDED_TM_AGENT_SOURCES {
        std::fs::write(dir.path().join(filename), content).map_err(RosterDeployError::Stage)?;
    }
    for embedded in DEFAULT_AGENTS.iter() {
        if let EmbeddedAgent::Direct { name, md } = embedded {
            std::fs::write(dir.path().join(format!("{name}.md")), md)
                .map_err(RosterDeployError::Stage)?;
        }
    }
    Ok(dir)
}

#[cfg(test)]
#[path = "deploy_tests.rs"]
mod deploy_tests;
