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
//! pointing it at `<project>/.trusty-code/agents/` needed no shared-library
//! change. It brings the atomic write-temp-then-rename, the strict-YAML
//! validation of every composed agent, the per-agent compose-failure isolation,
//! the `.trusty-mpm-manifest.json` ledger with a recorded checksum and
//! [`Origin`], and the refusal to proceed on a corrupt ledger.
//!
//! ## Two policies this module adds on top of the shared deployer
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
//! needed for either policy.
//!
//! Test: `agents::deploy::tests::*`,
//! `tests/roster_deploy_e2e.rs`.
//!
//! [`Origin`]: trusty_agents_common::agents::manifest::Origin
//! [`Origin::Bundled`]: trusty_agents_common::agents::manifest::Origin::Bundled

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use trusty_agents_common::agents::builder::AgentBuildError;
use trusty_agents_common::agents::deployer::{DeployResult, deploy_agents_filtered};
use trusty_agents_common::agents::manifest::{AgentManifest, MANIFEST_FILE, ManifestLoad};

use crate::assets::{DEFAULT_AGENTS, EMBEDDED_TM_AGENT_SOURCES, EmbeddedAgent};
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
/// 2. Refuse a target that is not genuinely beneath `<project>/.trusty-code/`
///    ([`crate::paths::check_native_write_target`], symlink-safe).
/// 3. Refuse to proceed on a corrupt ledger, naming the file. Never reset it.
/// 4. Stage every compiled-in source into a scratch directory, including the
///    five `BASE-*` templates so the deployer's own `extends:` composer resolves
///    the chains from disk exactly as it does for trusty-mpm.
/// 5. Deploy, selecting the dispatchable roster only, and deselecting any
///    tracked file the user has since edited.
///
/// Test: `fresh_project_materializes_the_whole_roster`,
/// `hand_edited_agent_survives_a_second_deploy`,
/// `corrupt_manifest_is_reported_and_nothing_is_written`,
/// `deploy_is_skipped_when_claude_agents_dir_wins`,
/// `base_templates_are_never_deployed`,
/// `tests/roster_deploy_e2e.rs`.
///
/// [`Origin`]: trusty_agents_common::agents::manifest::Origin
pub fn ensure_roster_deployed(project_root: &Path) -> Result<RosterDeploy, RosterDeployError> {
    if !paths::agents_dir(project_root).source.is_native() {
        return Ok(RosterDeploy::Skipped(SkipReason::CompatRootWins));
    }

    let target = roster_target(project_root);
    paths::check_native_write_target(project_root, &target)?;

    // Establish ownership BEFORE staging or writing anything. The deployer makes
    // the same check under its own lock; doing it here too is what lets a
    // corrupt ledger be reported without a scratch directory ever being built.
    let manifest = match AgentManifest::load_checked(&target) {
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

    let result = deploy_agents_filtered(staged.path(), &target, |stem| {
        roster.contains(stem) && !is_user_edited(&manifest, &target, stem)
    })
    .map_err(RosterDeployError::Deploy)?;

    Ok(RosterDeploy::Deployed {
        target,
        result: Box::new(result),
    })
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
    for embedded in DEFAULT_AGENTS {
        if let EmbeddedAgent::Direct { name, md } = embedded {
            std::fs::write(dir.path().join(format!("{name}.md")), md)
                .map_err(RosterDeployError::Stage)?;
        }
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_agents_common::agents::manifest::Origin;

    /// Every dispatchable roster name, for the count assertions below.
    fn roster_names() -> Vec<&'static str> {
        DEFAULT_AGENTS.iter().map(EmbeddedAgent::name).collect()
    }

    /// `.md` filenames actually present in a deployed target directory.
    fn deployed_md_files(target: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(target)
            .expect("read_dir on the deploy target")
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|n| n.ends_with(".md"))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn roster_target_is_under_the_native_config_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            roster_target(tmp.path()),
            tmp.path().join(".trusty-code").join(AGENTS_DIRNAME)
        );
    }

    #[test]
    fn staged_sources_cover_every_roster_name_and_base_template() {
        let staged = stage_embedded_sources().expect("stage");
        for name in roster_names() {
            assert!(
                staged.path().join(format!("{name}.md")).is_file(),
                "roster agent '{name}' must have a staged source file"
            );
        }
        for base in crate::assets::BASE_AGENT_NAMES {
            let map = trusty_agents_common::agents::builder::build_source_map(staged.path());
            assert!(
                map.contains_key(*base),
                "base template '{base}' must be stageable for extends resolution"
            );
        }
    }

    #[test]
    fn fresh_project_materializes_the_whole_roster() {
        let project = tempfile::tempdir().expect("project tempdir");
        let outcome = ensure_roster_deployed(project.path()).expect("deploy must succeed");
        let RosterDeploy::Deployed { target, result } = outcome else {
            panic!("a clean project must deploy, got: {outcome:?}");
        };

        assert!(result.failed.is_empty(), "no agent may fail: {result:?}");
        // Asserted from the filesystem, not from the deployer's own report.
        let on_disk = deployed_md_files(&target);
        assert_eq!(
            on_disk.len(),
            roster_names().len(),
            "every roster agent must land on disk, got: {on_disk:?}"
        );
        for name in roster_names() {
            assert!(
                target.join(format!("{name}.md")).is_file(),
                "'{name}.md' must exist on disk after the first deploy"
            );
        }

        // The manifest exists beside them and records framework ownership.
        let (manifest_path, status) = roster_manifest_status(project.path());
        assert!(manifest_path.is_file(), "the ledger must exist beside them");
        assert_eq!(
            status,
            ManifestStatus::Present {
                managed: roster_names().len()
            }
        );
        assert_eq!(
            AgentManifest::load(&target).managed["engineer.md"].origin,
            Origin::Bundled
        );
    }

    #[test]
    fn base_templates_are_never_deployed() {
        let project = tempfile::tempdir().expect("project tempdir");
        ensure_roster_deployed(project.path()).expect("deploy");
        for base in crate::assets::BASE_AGENT_NAMES {
            let deployed = deployed_md_files(&roster_target(project.path()));
            assert!(
                !deployed
                    .iter()
                    .any(|n| n.to_lowercase() == format!("{base}.md")),
                "composition base '{base}' must never be deployed as a dispatchable agent"
            );
        }
    }

    #[test]
    fn second_deploy_of_an_untouched_roster_writes_nothing() {
        let project = tempfile::tempdir().expect("project tempdir");
        ensure_roster_deployed(project.path()).expect("first deploy");
        let outcome = ensure_roster_deployed(project.path()).expect("second deploy");
        let RosterDeploy::Deployed { result, .. } = outcome else {
            panic!("second deploy must still run, got: {outcome:?}");
        };
        assert!(
            result.deployed.is_empty(),
            "an untouched roster must need no rewrite: {result:?}"
        );
        assert_eq!(result.unchanged.len(), roster_names().len());
    }

    #[test]
    fn hand_edited_agent_survives_a_second_deploy() {
        let project = tempfile::tempdir().expect("project tempdir");
        ensure_roster_deployed(project.path()).expect("first deploy");
        let target = roster_target(project.path());
        let edited = target.join("engineer.md");

        let hand_edit = "---\nname: engineer\nmodel: marker/hand-edited\n---\n\nMine now.\n";
        std::fs::write(&edited, hand_edit).expect("hand-edit the deployed agent");

        // Twice, to prove the preservation is stable rather than one-shot.
        for _ in 0..2 {
            let outcome = ensure_roster_deployed(project.path()).expect("re-deploy");
            let RosterDeploy::Deployed { result, .. } = outcome else {
                panic!("re-deploy must run, got: {outcome:?}");
            };
            assert!(
                !result.deployed.contains(&"engineer.md".to_string())
                    && !result.repaired.contains(&"engineer.md".to_string()),
                "a hand-edited agent must never be rewritten: {result:?}"
            );
            assert_eq!(
                std::fs::read_to_string(&edited).expect("read back"),
                hand_edit,
                "the hand edit must survive byte-identical"
            );
        }

        // Its neighbours are untouched by the carve-out.
        assert!(target.join("rust-engineer.md").is_file());
    }

    #[test]
    fn untracked_file_is_left_to_the_deployer() {
        let project = tempfile::tempdir().expect("project tempdir");
        let target = roster_target(project.path());
        std::fs::create_dir_all(&target).expect("mkdir target");
        let squatter = "---\nname: engineer\n---\n\nProject-owned, never deployed by us.\n";
        std::fs::write(target.join("engineer.md"), squatter).expect("write untracked file");

        let outcome = ensure_roster_deployed(project.path()).expect("deploy");
        let RosterDeploy::Deployed { result, .. } = outcome else {
            panic!("deploy must run, got: {outcome:?}");
        };
        assert!(
            result
                .untracked_modified
                .contains(&"engineer.md".to_string()),
            "an untracked, differing file is the deployer's own skip branch: {result:?}"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("engineer.md")).expect("read back"),
            squatter
        );
    }

    #[test]
    fn corrupt_manifest_is_reported_and_nothing_is_written() {
        let project = tempfile::tempdir().expect("project tempdir");
        let target = roster_target(project.path());
        std::fs::create_dir_all(&target).expect("mkdir target");
        std::fs::write(target.join(MANIFEST_FILE), b"not valid json{{{").expect("corrupt ledger");

        let err = ensure_roster_deployed(project.path()).expect_err("must refuse");
        assert!(
            matches!(err, RosterDeployError::ManifestCorrupt { .. }),
            "got: {err:?}"
        );
        assert!(
            err.to_string().contains("refusing to deploy"),
            "the message must say nothing was written: {err}"
        );
        assert!(
            deployed_md_files(&target).is_empty(),
            "a corrupt ledger must leave the directory untouched"
        );
        // And the ledger itself was never reset.
        assert_eq!(
            std::fs::read_to_string(target.join(MANIFEST_FILE)).expect("read back"),
            "not valid json{{{"
        );
    }

    #[test]
    fn manifest_status_reports_absent_then_present() {
        let project = tempfile::tempdir().expect("project tempdir");
        let (path, status) = roster_manifest_status(project.path());
        assert_eq!(status, ManifestStatus::Absent);
        assert_eq!(status.as_str(), "absent");
        assert!(!path.exists());

        ensure_roster_deployed(project.path()).expect("deploy");
        let (_, status) = roster_manifest_status(project.path());
        assert_eq!(status.as_str(), "present");
        assert!(matches!(status, ManifestStatus::Present { managed } if managed > 0));
    }

    #[test]
    fn manifest_status_reports_corrupt() {
        let project = tempfile::tempdir().expect("project tempdir");
        let target = roster_target(project.path());
        std::fs::create_dir_all(&target).expect("mkdir target");
        std::fs::write(target.join(MANIFEST_FILE), b"{{{").expect("corrupt ledger");
        let (_, status) = roster_manifest_status(project.path());
        assert_eq!(status.as_str(), "corrupt");
    }

    #[test]
    fn deploy_is_skipped_when_claude_agents_dir_wins() {
        let project = tempfile::tempdir().expect("project tempdir");
        let claude = project.path().join(".claude").join(AGENTS_DIRNAME);
        std::fs::create_dir_all(&claude).expect("mkdir .claude/agents");
        std::fs::write(
            claude.join("engineer.md"),
            "---\nname: engineer\n---\n\nProject catalog.\n",
        )
        .expect("write .claude agent");

        let outcome = ensure_roster_deployed(project.path()).expect("skip is not an error");
        assert!(
            matches!(outcome, RosterDeploy::Skipped(SkipReason::CompatRootWins)),
            "a winning .claude/agents/ must never be shadowed, got: {outcome:?}"
        );
        assert!(
            !roster_target(project.path()).exists(),
            "nothing may be written when the deploy is skipped"
        );
    }

    #[test]
    fn deploy_and_log_is_a_no_op_without_a_project() {
        assert!(deploy_and_log(None).is_none());
    }

    #[test]
    fn deploy_and_log_reports_a_corrupt_ledger_without_panicking() {
        crate::test_support::begin_capture();

        let project = tempfile::tempdir().expect("project tempdir");
        let target = roster_target(project.path());
        std::fs::create_dir_all(&target).expect("mkdir target");
        std::fs::write(target.join(MANIFEST_FILE), b"{{{").expect("corrupt ledger");
        assert!(
            deploy_and_log(Some(project.path())).is_none(),
            "a corrupt ledger must degrade to the in-memory roster, not abort"
        );

        // Degrading silently is the failure this branch exists to prevent, so
        // the assertion is on the ERROR event itself — downgrading it to
        // `debug!` must fail this test, not just change a log line.
        let errors = crate::test_support::captured_at_least(tracing::Level::ERROR);
        assert!(
            errors
                .iter()
                .any(|m| m.contains("could not materialize the agent roster")),
            "the corrupt ledger must be reported at ERROR level, got: {errors:?}"
        );
    }

    #[test]
    fn deployed_agents_load_back_through_the_disk_loader() {
        let project = tempfile::tempdir().expect("project tempdir");
        ensure_roster_deployed(project.path()).expect("deploy");
        let loaded = crate::agents::load_all_agents(&roster_target(project.path()));
        assert_eq!(
            loaded.len(),
            roster_names().len(),
            "every deployed file must parse through the disk loader"
        );
        let restricted = loaded
            .iter()
            .find(|c| c.agent.name == "code-critic")
            .expect("code-critic must be deployed");
        let allowed = restricted
            .tools
            .as_ref()
            .and_then(|t| t.allowed.as_ref())
            .expect("code-critic carries a tcode-local tool allowlist");
        assert!(
            !allowed.contains(&"write_file".to_string()),
            "the tcode-local read-only restriction must survive materialization: {allowed:?}"
        );
    }
}
