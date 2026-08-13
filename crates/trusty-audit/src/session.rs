//! The single API surface every front end drives.
//!
//! Why: #5502 fixes a constraint that does not lapse — every feature must be
//! exercisable from the CLI, forever, and the Tauri shell arriving in a later
//! milestone is a view over the same API rather than a place where a capability
//! can hide. A library with a CLI over it does not achieve that on its own; a
//! CLI that grows its own logic looks identical from the outside until the GUI
//! needs that logic and cannot reach it.
//!
//! So the capability set is a TYPE. [`Command`] enumerates everything this crate
//! can do, [`Session::execute`] is the only way to do any of it, and
//! [`Outcome`] is structured data rather than printed text — rendering belongs
//! to the front end. Adding a capability means adding a `Command` variant, and
//! `crate::cli`'s exhaustive match over `Command` then fails to compile until a
//! CLI invocation exists for it. That is the enforcement: not a rule someone
//! remembers, a build error.
//!
//! What: [`Session`] holds the resolved working directory and the manifest path;
//! `execute` dispatches. No method on `Session` is reachable except through
//! `execute`, so no front end can acquire a private path to a capability.
//! Test: `super::session_tests`, plus `crate::cli::cli_tests`.

use std::path::{Path, PathBuf};

use crate::config::EngagementConfig;
use crate::discover::{self, DiscoveredRepo};
use crate::error::AuditError;
use crate::manifest::{AuditManifest, RepositoryEntry};
use crate::run::{self, RunReport};
use crate::tools::{self, InstalledTool, RequiredTool, ToolStatus};
use crate::workdir::{Area, WorkDir};

/// Everything `trusty-audit` can be asked to do.
///
/// Why: see the module docs — the enum is what makes "every feature is
/// CLI-testable" mechanically checkable instead of aspirational.
/// What: one variant per capability. `#[non_exhaustive]` so later milestones add
/// variants without a breaking change.
/// Test: `crate::cli::cli_tests::every_command_variant_has_a_cli_invocation`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Command {
    /// What a bare invocation runs: the guided flow, starting at repo selection.
    Guided,
    /// Create the working directory and report its layout.
    WorkDir,
    /// Read the companion `manifest.toml` and report the engagement metadata.
    Manifest,
    /// Report which pinned tools are installed, and at which verified versions.
    Tools,
    /// Download and verify the pinned tool triple, or install none of it (#5495).
    InstallTools,
    /// List the repositories the engagement is configured to audit.
    Repos,
    /// List the repositories the recipient's `gh` credential can reach (#5487).
    DiscoverRepos,
    /// Run `tga audit` over the selected repositories (#5555).
    Run,
}

/// The working directory's layout, as reported to a front end.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WorkDirReport {
    /// The root — deleting this removes everything this client wrote.
    pub root: PathBuf,
    /// Each area and its path, in [`Area::ALL`] order.
    pub areas: Vec<(Area, PathBuf)>,
}

/// What the guided flow should do next.
///
/// Why: the guided flow is not an unattended hour-long sweep — it walks the
/// operator through the pre-sweep steps, and the epic (#5477) fixes their order:
/// pick repositories, then install tools, then run. Naming the next step as data
/// lets the CLI print it and the future GUI highlight it from one decision.
/// What: the three states the scaffold can distinguish.
/// Test: `super::session_tests::guided_asks_for_repositories_before_tools`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NextStep {
    /// No repositories are known yet; selection comes first (#5487, #5497).
    SelectRepositories,
    /// Repositories are known but tooling is missing (#5491, #5495).
    InstallTools(Vec<RequiredTool>),
    /// Repositories are selected and the pinned triple is installed; sweep (#5555).
    ReadyForRun,
}

/// The guided flow's view of the engagement.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct GuidedStatus {
    /// Working-directory root, now created.
    pub root: PathBuf,
    /// The companion manifest, when a previous run left one.
    pub manifest: Option<AuditManifest>,
    /// Per-tool install state.
    pub tools: Vec<ToolStatus>,
    /// What to do next.
    pub next: NextStep,
}

/// The result of one [`Command`].
///
/// Why: structured, not a `String`. The CLI renders it; the Tauri shell will
/// render the same values differently. If this were text, the GUI would have to
/// either parse it or grow its own path to the data — the second is how a
/// capability ends up existing only in the GUI.
/// What: one variant per [`Command`] variant.
/// Test: `super::session_tests`.
#[derive(Debug)]
#[non_exhaustive]
pub enum Outcome {
    /// From [`Command::Guided`].
    Guided(GuidedStatus),
    /// From [`Command::WorkDir`].
    WorkDir(WorkDirReport),
    /// From [`Command::Manifest`].
    Manifest(AuditManifest),
    /// From [`Command::Tools`].
    Tools(Vec<ToolStatus>),
    /// From [`Command::InstallTools`] — the exact triple now on disk.
    Installed(Vec<InstalledTool>),
    /// From [`Command::Repos`].
    Repos(Vec<RepositoryEntry>),
    /// From [`Command::DiscoverRepos`] — what the credential can reach, not
    /// what the engagement selected.
    Discovered(Vec<DiscoveredRepo>),
    /// From [`Command::Run`] — per-repository results and the sweep's verdict.
    ///
    /// A non-[`RunStatus::AllSucceeded`](crate::run::RunStatus::AllSucceeded)
    /// report is an ORDINARY `Ok` here: the sweep ran and some of it failed, and
    /// the failures are data the front end must show. It is `crate::cli::exit_code`
    /// that turns that into a non-zero process exit, so a caller cannot report
    /// success without having read the status (#5555).
    Run(RunReport),
}

/// One audit engagement, rooted at a working directory.
///
/// Why: the front ends share this and nothing else.
/// What: the resolved working directory, the path of the companion
/// `manifest.toml` (which defaults inside the working directory's output area),
/// and the path of the engagement config that arrived with the handoff package.
/// Both are overridable, so an operator can point at files a previous run or a
/// different package left elsewhere.
/// Test: `super::session_tests`.
#[derive(Debug, Clone)]
pub struct Session {
    work: WorkDir,
    manifest_path: PathBuf,
    config_path: PathBuf,
}

impl Session {
    /// Build a session over `work`, with the default companion-file locations.
    ///
    /// The engagement config travels with the handoff package rather than with
    /// the working directory, so a front end that knows where the package was
    /// unzipped should say so via [`Session::with_config_path`]; the CLI does
    /// (`EngagementConfig::resolve_path`). The default here is the working
    /// directory's root, which is where the file ends up if the recipient never
    /// moves anything.
    pub fn new(work: WorkDir) -> Self {
        let manifest_path = work.path(Area::Output).join(AuditManifest::FILE_NAME);
        let config_path = work.root().join(EngagementConfig::FILE_NAME);
        Self {
            work,
            manifest_path,
            config_path,
        }
    }

    /// Point at a manifest outside the working directory.
    #[must_use]
    pub fn with_manifest_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.manifest_path = path.into();
        self
    }

    /// Point at the engagement config that came with the handoff package.
    #[must_use]
    pub fn with_config_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_path = path.into();
        self
    }

    /// The working directory this session owns.
    pub fn work_dir(&self) -> &WorkDir {
        &self.work
    }

    /// Where this session expects the companion manifest.
    pub fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    /// Where this session expects the engagement config.
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Run one capability.
    ///
    /// Why: the single entry point. Every front end goes through here, so a
    /// capability cannot exist for one and not the other.
    ///
    /// It is `async` because one capability — installing the pinned tools —
    /// downloads over the network, and `trusty-installer`'s entry point is
    /// async. Blocking on a runtime inside a sync `execute` would work for the
    /// CLI and then panic inside the Tauri shell, which calls from an async
    /// context; making the seam itself async is what keeps the two front ends
    /// on one path (#5495).
    ///
    /// What: dispatches to a private helper per variant. Each helper creates the
    /// working directory first when it needs it, so a first run works with no
    /// setup step the operator has to remember.
    /// Test: `super::session_tests`.
    ///
    /// # Errors
    ///
    /// [`AuditError`] from the underlying operation — a working directory that
    /// cannot be created, a companion file that cannot be read or parsed, a
    /// pinned install that refused, or [`AuditError::NotImplemented`] for a
    /// capability a later milestone lands.
    pub async fn execute(&self, command: Command) -> Result<Outcome, AuditError> {
        match command {
            Command::Guided => self.guided().map(Outcome::Guided),
            Command::WorkDir => self.work_dir_report().map(Outcome::WorkDir),
            Command::Manifest => AuditManifest::load(&self.manifest_path).map(Outcome::Manifest),
            Command::Tools => tools::status(&self.work).map(Outcome::Tools),
            Command::InstallTools => self.install_tools().await.map(Outcome::Installed),
            Command::Repos => self.repos().map(Outcome::Repos),
            // #5487: discovery asks GitHub, so it takes nothing from the
            // session's own state — it is on `Session` because `execute` is the
            // only door any front end gets (#5502).
            Command::DiscoverRepos => discover::discover(discover::DEFAULT_LIMIT)
                .await
                .map(Outcome::Discovered),
            Command::Run => self.run().await.map(Outcome::Run),
        }
    }

    /// Read the engagement's key and pins, then sweep the selected repositories.
    ///
    /// The config is loaded for the same reason [`Session::install_tools`] loads
    /// it: it carries the OpenRouter key `tga audit`'s report render needs, and
    /// an absent config is a refusal rather than a run that will fail an hour in
    /// (#5555).
    async fn run(&self) -> Result<RunReport, AuditError> {
        let config = EngagementConfig::load(&self.config_path)?;
        run::sweep(&self.work, &config).await
    }

    /// Read the engagement's pins, then install exactly those.
    ///
    /// The config is loaded rather than defaulted: an absent or unreadable
    /// engagement config is a refusal, because the alternative — installing
    /// "latest" — is the version-skew defect #5454 closed (#5495).
    async fn install_tools(&self) -> Result<Vec<InstalledTool>, AuditError> {
        let config = EngagementConfig::load(&self.config_path)?;
        tools::install(&self.work, &config.tools).await
    }

    fn work_dir_report(&self) -> Result<WorkDirReport, AuditError> {
        self.work.create()?;
        Ok(WorkDirReport {
            root: self.work.root().to_path_buf(),
            areas: self.work.layout(),
        })
    }

    fn repos(&self) -> Result<Vec<RepositoryEntry>, AuditError> {
        Ok(AuditManifest::load_if_present(&self.manifest_path)?
            .map(|m| m.repositories)
            .unwrap_or_default())
    }

    fn guided(&self) -> Result<GuidedStatus, AuditError> {
        self.work.create()?;
        let manifest = AuditManifest::load_if_present(&self.manifest_path)?;
        let tools = tools::status(&self.work)?;

        // #5502: the epic's pre-sweep order is repo selection, then tooling —
        // so a missing repository set outranks a missing binary.
        let repos_known = manifest
            .as_ref()
            .is_some_and(|m| !m.repositories.is_empty());
        let missing: Vec<RequiredTool> = tools
            .iter()
            .filter(|s| !s.installed)
            .map(|s| s.tool)
            .collect();

        let next = if !repos_known {
            NextStep::SelectRepositories
        } else if !missing.is_empty() {
            NextStep::InstallTools(missing)
        } else {
            NextStep::ReadyForRun
        };

        Ok(GuidedStatus {
            root: self.work.root().to_path_buf(),
            manifest,
            tools,
            next,
        })
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;

    const MANIFEST: &str = r#"
[report]
title = "Acme — Technical Due Diligence"

[[repositories]]
name = "acme-api"
path = "/work/repos/acme-api"
"#;

    fn session_in(dir: &Path) -> Session {
        Session::new(WorkDir::new(dir.join("work")))
    }

    fn write_manifest(session: &Session, text: &str) {
        let path = session.manifest_path();
        std::fs::create_dir_all(path.parent().expect("manifest has a parent")).expect("mkdir");
        std::fs::write(path, text).expect("write manifest");
    }

    /// An engagement config carrying a version that was never published, so the
    /// install refuses at the release lookup without a plausible download.
    const UNPUBLISHABLE_CONFIG: &str = r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks."

[tools]
tga = "0.0.0-never-published"
trusty-analyze = "0.0.0-never-published"
trusty-review = "0.0.0-never-published"
"#;

    fn session_with_config(dir: &Path, text: &str) -> Session {
        let path = dir.join("engagement.toml");
        std::fs::write(&path, text).expect("write engagement config");
        session_in(dir).with_config_path(path)
    }

    #[tokio::test]
    async fn work_dir_command_creates_the_tree_and_reports_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());

        let Outcome::WorkDir(report) = session.execute(Command::WorkDir).await.expect("runs")
        else {
            panic!("WorkDir command must yield a WorkDir outcome");
        };
        assert_eq!(report.areas.len(), Area::ALL.len());
        for (_, path) in &report.areas {
            assert!(path.is_dir(), "{} was not created", path.display());
        }
    }

    #[tokio::test]
    async fn guided_asks_for_repositories_before_tools() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());

        let Outcome::Guided(status) = session.execute(Command::Guided).await.expect("runs") else {
            panic!("Guided command must yield a Guided outcome");
        };
        // No manifest and no tools: selection is still the step that comes first.
        assert_eq!(status.next, NextStep::SelectRepositories);
        assert!(status.manifest.is_none());
        assert!(status.tools.iter().all(|s| !s.installed));
    }

    #[tokio::test]
    async fn guided_asks_for_tools_once_repositories_are_known() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());
        session
            .execute(Command::WorkDir)
            .await
            .expect("create tree");
        write_manifest(&session, MANIFEST);

        let Outcome::Guided(status) = session.execute(Command::Guided).await.expect("runs") else {
            panic!("Guided command must yield a Guided outcome");
        };
        assert_eq!(
            status.next,
            NextStep::InstallTools(RequiredTool::ALL.to_vec())
        );
    }

    #[tokio::test]
    async fn guided_is_ready_once_repositories_and_tools_are_both_in_place() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());
        session
            .execute(Command::WorkDir)
            .await
            .expect("create tree");
        write_manifest(&session, MANIFEST);
        for tool in RequiredTool::ALL {
            std::fs::write(tool.path_in(session.work_dir()), b"stub").expect("stub binary");
        }

        let Outcome::Guided(status) = session.execute(Command::Guided).await.expect("runs") else {
            panic!("Guided command must yield a Guided outcome");
        };
        assert_eq!(status.next, NextStep::ReadyForRun);
    }

    #[tokio::test]
    async fn repos_reads_the_manifest_rather_than_a_second_copy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());
        session
            .execute(Command::WorkDir)
            .await
            .expect("create tree");
        write_manifest(&session, MANIFEST);

        let Outcome::Repos(repos) = session.execute(Command::Repos).await.expect("runs") else {
            panic!("Repos command must yield a Repos outcome");
        };
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].name, "acme-api");
    }

    #[tokio::test]
    async fn repos_is_empty_rather_than_an_error_before_any_run() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());
        let Outcome::Repos(repos) = session.execute(Command::Repos).await.expect("runs") else {
            panic!("Repos command must yield a Repos outcome");
        };
        assert!(repos.is_empty());
    }

    #[tokio::test]
    async fn manifest_command_is_an_error_when_the_file_is_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());
        let err = session
            .execute(Command::Manifest)
            .await
            .expect_err("asking for a manifest that is not there is an error");
        assert!(matches!(err, AuditError::Read { .. }));
    }

    #[tokio::test]
    async fn the_manifest_path_can_point_outside_the_working_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let elsewhere = tmp.path().join("previous-run/manifest.toml");
        std::fs::create_dir_all(elsewhere.parent().expect("parent")).expect("mkdir");
        std::fs::write(&elsewhere, MANIFEST).expect("write");

        let session = session_in(tmp.path()).with_manifest_path(&elsewhere);
        let Outcome::Manifest(manifest) = session.execute(Command::Manifest).await.expect("runs")
        else {
            panic!("Manifest command must yield a Manifest outcome");
        };
        assert_eq!(manifest.report.title, "Acme — Technical Due Diligence");
    }

    /// No config means no pins, and no pins must never mean "fetch latest".
    #[tokio::test]
    async fn installing_without_an_engagement_config_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());

        let err = session
            .execute(Command::InstallTools)
            .await
            .expect_err("there is nothing to pin to");
        assert!(matches!(err, AuditError::Read { .. }), "{err:?}");
        assert!(
            tools::read_record(session.work_dir())
                .expect("no record")
                .is_empty()
        );
    }

    /// A config the generator wrote without the triple is refused at parse time.
    #[tokio::test]
    async fn installing_from_a_config_with_no_pins_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_with_config(
            tmp.path(),
            "openrouter_key = \"sk-or-v1-x\"\ninstructions = \"assess\"\n",
        );

        let err = session
            .execute(Command::InstallTools)
            .await
            .expect_err("an unpinned config must not install anything");
        assert!(matches!(err, AuditError::Parse { .. }), "{err:?}");
    }

    /// The whole path against the real release host: a version that cannot
    /// resolve installs nothing, records nothing, and hands back the
    /// installer's own reason.
    ///
    /// `#[ignore]` because it reaches the network — `cargo test -p trusty-audit
    /// -- --include-ignored` runs it. The offline half of the same guarantee is
    /// the two tests above and `crate::tools::tool_tests`.
    #[tokio::test]
    #[ignore = "reaches the GitHub release API; run with --include-ignored"]
    async fn an_unresolvable_pin_installs_nothing_and_records_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_with_config(tmp.path(), UNPUBLISHABLE_CONFIG);

        let err = session
            .execute(Command::InstallTools)
            .await
            .expect_err("a version that was never published cannot install");
        // Which refusal depends on the host — unreachable network is
        // ReleaseLookupFailed, reachable is VersionNotPublished, a non-Tier-1
        // host is UnsupportedTarget. All are `Install`; all install nothing.
        assert!(matches!(err, AuditError::Install { .. }), "{err:?}");

        let statuses = tools::status(session.work_dir()).expect("status reads");
        assert!(
            statuses.iter().all(|s| !s.installed && s.version.is_none()),
            "a refused install must leave the tools area empty: {statuses:?}"
        );
        assert!(
            tools::read_record(session.work_dir())
                .expect("no record")
                .is_empty(),
            "a refused install must not record a version"
        );
    }
}
