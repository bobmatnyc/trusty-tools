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

use crate::chain::{self, ChainOptions, ChainReport};
use crate::clone::{self, CloneOptions, CloneReport};
use crate::config::{EngagementConfig, SecretKey};
use crate::discover::{self, DiscoveredRepo};
use crate::distribute::{self, DistributeOptions, InstallPackage};
use crate::error::AuditError;
use crate::manifest::{AuditManifest, RepositoryEntry};
use crate::package::{self, ReturnPackage};
use crate::progress::{Progress, ProgressSink};
use crate::registry::{self, Registration, Registry, Removal, TargetKind, TargetList};
use crate::run::{self, RunOptions, RunReport};
use crate::tools::{self, InstalledTool, RequiredTool, ToolStatus};
use crate::validate;
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
    /// Clone the named repositories into the working directory (#5215).
    ///
    /// Carries its argument because acquisition acts on a SELECTION — what the
    /// picker chose, which is not derivable from the session's own state.
    CloneRepos {
        /// `owner/name` per repository, in the order to acquire them.
        repos: Vec<String>,
        /// How to clone.
        options: CloneOptions,
    },
    /// Register one audit target, after proving it can be read (#5822).
    ///
    /// Carries its argument for the same reason [`Command::CloneRepos`] does:
    /// what to register is the operator's input, not derivable from the
    /// session's state. `kind` is the verb they used, so `add repo jira:ACME`
    /// is a refusal rather than a board.
    AddTarget {
        /// Which verb asked — `repo` or `board`.
        kind: TargetKind,
        /// The spec, unparsed. [`registry::parse`] owns what it may be.
        spec: String,
    },
    /// List the registered audit targets (#5822).
    ListTargets,
    /// Drop one registered target (#5822). Accepts either spec shape.
    RemoveTarget {
        /// `owner/name` or `provider:key`.
        spec: String,
    },
    /// Run `tga audit` over the selected repositories (#5555).
    ///
    /// Carries its options because resume is an operator decision, not a
    /// property of the session: a re-run skips what an earlier one recorded as
    /// audited, and [`RunOptions::fresh`] is how that is overruled (#5494).
    Run(RunOptions),
    /// Assemble the unencrypted deliverable to send back (#5499).
    ///
    /// Carries its argument because the recipient chooses where the file lands —
    /// the CLI's answer to the save dialog the Tauri shell will offer. `None`
    /// means [`crate::package::default_destination`].
    Package {
        /// Where to write the zip, or `None` for the default inside the work dir.
        destination: Option<PathBuf>,
    },
    /// Drive the whole engagement in one call: install, materialize the
    /// registered targets, collect and analyze each one, package (#5824).
    ///
    /// This is an ADDITION, not a replacement — the four capabilities it chains
    /// stay individually invocable, because an operator debugging one phase
    /// needs to run just that phase. See [`crate::chain`] for the partial-success
    /// policy and what each phase is.
    Audit(ChainOptions),
    /// Assemble the install package that goes TO a client (#5825).
    ///
    /// The one capability here the AUDITOR runs rather than the recipient: it
    /// builds the zip every other capability presupposes has already been
    /// extracted. Carries its options because the output directory and the
    /// binary to ship are both operator choices with defaults.
    ///
    /// Deliberately NOT [`Command::Package`] with a direction flag — the two
    /// travel opposite ways and disagree about the credential, so they are two
    /// variants dispatching to two modules. See [`crate::distribute`].
    Distribute(DistributeOptions),
}

/// How hard a front end should look for the inference credential (#5868).
///
/// Why: only the front end can prompt, so only the front end can decide to.
/// But WHICH capabilities need a key is a property of the capability, not of
/// the front end — leaving it to each one is how the CLI and the Tauri shell
/// end up disagreeing about when a client gets asked. So the answer is data on
/// [`Command`], and every front end reads the same answer.
/// What: three tiers, ordered by how many sources they permit.
/// Test: `super::session_tests::only_the_inference_capabilities_need_a_credential`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CredentialNeed {
    /// The capability sends nothing for inference. Never ask, never look.
    None,
    /// An exported variable is used when it is set, and nothing else is.
    ///
    /// Never prompts: these capabilities already have their own fallback and a
    /// client running them is not the one who holds the key.
    Environment,
    /// The capability cannot run without a key, so every source is tried —
    /// including asking, when there is a terminal.
    Required,
}

impl Command {
    /// Which credential sources a front end should consult for this command.
    ///
    /// Why: an exhaustive match, so a new [`Command`] variant fails to compile
    /// until someone decides whether it needs a key. That is the same
    /// enforcement `crate::cli`'s match over `Command` already provides for CLI
    /// reachability — a rule the build checks rather than one to remember.
    /// What: [`CredentialNeed::Required`] for the two capabilities that run
    /// inference; [`CredentialNeed::Environment`] for the two that handle a key
    /// without needing to obtain one; [`CredentialNeed::None`] for the rest.
    /// Test: `super::session_tests::only_the_inference_capabilities_need_a_credential`.
    pub fn credential_need(&self) -> CredentialNeed {
        match self {
            // The sweep and the chain both hand the key to a `tga audit` child.
            Self::Run(_) | Self::Audit(_) => CredentialNeed::Required,
            // `package` scans the deliverable for the key that was in play, and
            // `distribute` writes one into the config it generates. Neither may
            // prompt: packaging must not stop to ask, and the auditor running
            // `distribute` supplies a key through the environment or the
            // template, which is the contract #5825 set.
            Self::Package { .. } | Self::Distribute(_) => CredentialNeed::Environment,
            Self::Guided
            | Self::WorkDir
            | Self::Manifest
            | Self::Tools
            | Self::InstallTools
            | Self::Repos
            | Self::DiscoverRepos
            | Self::CloneRepos { .. }
            | Self::AddTarget { .. }
            | Self::ListTargets
            | Self::RemoveTarget { .. } => CredentialNeed::None,
        }
    }
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
    ///
    /// Since #5797 the flow installs rather than stopping here, so this state
    /// means installing did not happen: `--no-install`, or no engagement config
    /// to take pins from. It is reachable, not vestigial.
    InstallTools(Vec<RequiredTool>),
    /// Repositories are selected and the pinned triple is installed; sweep (#5555).
    ReadyForRun,
    /// A sweep has finished and audited something; assemble the return package
    /// and send it back (#5499). The last step of the engagement.
    ReturnPackage,
}

/// The guided flow's view of the engagement.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct GuidedStatus {
    /// Working-directory root, now created.
    pub root: PathBuf,
    /// The companion manifest, when a previous run left one.
    pub manifest: Option<AuditManifest>,
    /// Per-tool install state, read AFTER any auto-install this flow performed.
    pub tools: Vec<ToolStatus>,
    /// What this flow installed on its way here, when it installed anything.
    ///
    /// `None` covers both "nothing needed installing" and "auto-install is
    /// off" — the front end distinguishes those from [`GuidedStatus::tools`],
    /// which says what is actually on disk. This field exists so the flow can
    /// report a download the operator did not ask for and would otherwise see
    /// only as a pause (#5797).
    pub installed: Option<Vec<InstalledTool>>,
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
    /// From [`Command::CloneRepos`].
    Cloned(CloneReport),
    /// From [`Command::AddTarget`] — what is registered, and whether this call
    /// is what registered it.
    Registered(Registration),
    /// From [`Command::ListTargets`].
    Targets(TargetList),
    /// From [`Command::RemoveTarget`].
    Removed(Removal),
    /// From [`Command::Run`] — per-repository results and the sweep's verdict.
    ///
    /// A non-[`RunStatus::AllSucceeded`](crate::run::RunStatus::AllSucceeded)
    /// report is an ORDINARY `Ok` here: the sweep ran and some of it failed, and
    /// the failures are data the front end must show. It is [`Outcome::exit_code`]
    /// that turns that into a non-zero process exit, so a caller cannot report
    /// success without having read the status (#5555).
    Run(RunReport),
    /// From [`Command::Package`] — the file to send back, and what it omits.
    Package(ReturnPackage),
    /// From [`Command::Audit`] — what every phase of the chain produced.
    ///
    /// Like [`Outcome::Run`], a report carrying failures is an ORDINARY `Ok`:
    /// the chain finished and some of it did not, and those failures are data
    /// the front end must show. [`Outcome::exit_code`] is what stops that
    /// reading as success.
    Audit(ChainReport),
    /// From [`Command::Distribute`] — the file to send a client, and what it
    /// will run on.
    Distributed(InstallPackage),
}

/// Exit status for a run that succeeded but did not cover everything asked for.
pub const EXIT_INCOMPLETE: i32 = 2;

/// Exit status for a sweep that partly failed.
pub const EXIT_PARTIAL: i32 = 1;

impl Outcome {
    /// The process exit status this outcome should produce.
    ///
    /// Why: acquisition continues past a repository it could not clone, and a
    /// sweep continues past a repository `tga audit` failed on — both are the
    /// right behaviour for a hundred-repo run and the wrong thing to report as
    /// unqualified success. The rendered text names every exclusion, but
    /// `taudit clone $(cat repos.txt) && taudit run` reads the status, not the
    /// text — so an incomplete outcome that exits 0 chains straight into the
    /// next stage over a set the operator never actually got (#5215 review,
    /// #5555).
    /// What: [`EXIT_PARTIAL`] for a [`RunReport`] that did not fully succeed;
    /// [`EXIT_INCOMPLETE`] for a [`CloneReport`] that carries gaps, and for a
    /// [`ReturnPackage`] that does not cover every repository the sweep
    /// attempted (#5499 — the package is still worth sending, and it is still
    /// not the whole engagement); 0 otherwise. The policy lives here rather than
    /// in `main.rs` so the Tauri shell reads the same judgement rather than
    /// re-deriving it.
    /// Test: `crate::cli::cli_tests::a_partial_sweep_does_not_exit_zero`,
    /// `crate::cli::cli_tests::a_run_with_gaps_exits_non_zero`,
    /// `crate::cli::cli_tests::a_package_that_omits_a_repository_does_not_exit_zero`,
    /// `crate::chain::chain_tests::a_partly_failed_chain_packages_and_still_does_not_exit_zero`.
    pub fn exit_code(&self) -> i32 {
        match self {
            Outcome::Run(report) if report.status != run::RunStatus::AllSucceeded => EXIT_PARTIAL,
            Outcome::Cloned(report) if !report.gaps.is_empty() => EXIT_INCOMPLETE,
            Outcome::Package(package) if !package.excluded.is_empty() => EXIT_INCOMPLETE,
            // #5824: the chain reports the sweep's verdict for the same reason
            // `run` does — a repository that failed makes this an incomplete
            // engagement, and `taudit audit && send-it` must not chain onward.
            Outcome::Audit(report) if report.run.status != run::RunStatus::AllSucceeded => {
                EXIT_PARTIAL
            }
            // A target the chain never attempted (a registered board today) is
            // not a sweep failure and would otherwise be invisible to `$?`.
            Outcome::Audit(report)
                if !report.gaps.is_empty() || !report.package.excluded.is_empty() =>
            {
                EXIT_INCOMPLETE
            }
            _ => 0,
        }
    }
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
    auto_install: bool,
    /// Which `gh` invocations a repository registration runs (#5822).
    repo_probe: validate::RepoProbe,
    /// The credential the FRONT END resolved, when it resolved one (#5868).
    ///
    /// Why: this replaced the `fn(&str) -> Option<String>` environment lookup
    /// #5825 introduced. That seam could only ever read the process
    /// environment, and first-run entry adds two more sources — an existing
    /// engagement config and a `/dev/tty` prompt. Keeping the lookup AND adding
    /// a resolved value would leave two ways for a credential to reach the same
    /// code, free to disagree about precedence, so the lookup is gone:
    /// `Session` now takes the ANSWER rather than a way to find one. That is
    /// also what keeps [`Session::execute`] terminal-free, which the Tauri
    /// shell (#5477) and the TUI after it both depend on.
    ///
    /// It is still the value a test supplies rather than a global it reads, so
    /// #5825's guarantee is unchanged: `cargo test` cannot write a developer's
    /// exported `OPENROUTER_API_KEY` into a zip on disk.
    /// What: `None` means the front end resolved nothing, and every capability
    /// behaves as it did before — `distribute` falls back to its template's
    /// key, `run` and `audit` to the engagement config's.
    /// Test: `super::session_tests::a_credential_in_the_environment_is_the_one_packaged`,
    /// `super::session_tests::a_supplied_credential_beats_the_configs_own`.
    credential: Option<SecretKey>,
    /// Where live progress goes while a long capability runs (#5823).
    ///
    /// A field rather than a parameter on [`Session::execute`] because progress
    /// is a property of the FRONT END, not of the command — the CLI decides once
    /// that it renders to a terminal, and the Tauri shell decides once that it
    /// renders to a window. Defaults to [`Progress::none`], on which every
    /// update is dropped, so nothing here depends on there being a terminal.
    progress: Progress,
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
            auto_install: true,
            repo_probe: validate::RepoProbe::real(),
            credential: None,
            progress: Progress::none(),
        }
    }

    /// Point the repository check at a different pair of `gh` invocations.
    ///
    /// Why: #5822 — the repository arm's fail-closed guarantee ("a refused
    /// registration writes no file") was provable only by an `#[ignore]`d test
    /// needing network and an authenticated `gh`. `#[cfg(test)]` because it is
    /// the injection seam for that proof and nothing else: it does not exist in
    /// a shipped build, so `Session::execute` remains the only door a front end
    /// gets and no capability can hide behind it.
    /// What: replaces the [`validate::RepoProbe`] `add` runs.
    /// Test: `super::session_tests::a_refused_repository_registration_writes_nothing`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_repo_probe(mut self, probe: validate::RepoProbe) -> Self {
        self.repo_probe = probe;
        self
    }

    /// Run with the credential the front end resolved (#5868).
    ///
    /// Why: `pub`, unlike the `#[cfg(test)]` lookup it replaced, because a real
    /// front end now has something to say here. The CLI resolves
    /// `OPENROUTER_API_KEY`, then the engagement config, then a `/dev/tty`
    /// prompt (`crate::cli::credential`) and hands the ANSWER across this
    /// boundary; the Tauri shell will resolve it its own way and use the same
    /// setter. What must not cross is a way to ASK — [`Session::execute`] has
    /// no terminal and must never acquire one.
    ///
    /// This is the only way a credential reaches `Session`. Passing `None`
    /// leaves each capability on the fallback it had before: `distribute` on
    /// its template's key, `run` and `audit` on the engagement config's.
    /// What: stores the key. A blank one is not special-cased here —
    /// [`Session::engagement_config`] refuses it, so every front end gets that
    /// check rather than only the one that remembered to make it.
    /// Test: `super::session_tests::a_supplied_credential_beats_the_configs_own`,
    /// `super::session_tests::a_credential_in_the_environment_is_the_one_packaged`.
    #[must_use]
    pub fn with_credential(mut self, credential: Option<SecretKey>) -> Self {
        self.credential = credential;
        self
    }

    /// Render this session's progress through `sink` (#5823).
    ///
    /// Why: the long capabilities — installing, cloning, sweeping — report what
    /// they are doing, and without a sink that reporting goes nowhere. Which is
    /// the point: [`Session::execute`] must stay callable by a front end that
    /// has no terminal, so the display is something a caller SUPPLIES rather
    /// than something the dispatch reaches for. `crate::main` supplies
    /// [`crate::progress::terminal::TerminalProgress`]; a GUI supplies its own,
    /// and neither has to know the other exists.
    /// What: stores the sink. Absent, every update is discarded and the
    /// capabilities behave identically.
    /// Test: `crate::run::run_tests::a_childs_stage_events_reach_the_progress_sink`
    /// for a sink that receives, `a_sweep_without_a_sink_is_unchanged` for the
    /// default.
    #[must_use]
    pub fn with_progress(mut self, sink: std::sync::Arc<dyn ProgressSink>) -> Self {
        self.progress = Progress::to(sink);
        self
    }

    /// Whether a capability that needs the pinned tools may install them itself.
    ///
    /// Why: #5797. On by default, because the recipient double-clicks one thing
    /// and the tooling is this client's own prerequisite, not a decision it
    /// should hand back. The opt-out exists for the case the default gets wrong:
    /// someone who wants to know the state of a working directory without this
    /// process reaching the network. `trusty-audit tools` answers that without
    /// the flag — it never installs — and the flag covers the flows that do.
    /// What: `false` restores the previous behaviour exactly. `guided` reports
    /// [`NextStep::InstallTools`] and `run` refuses with
    /// [`AuditError::ToolsNotInstalled`], both as they did before auto-install.
    /// Test: `super::session_tests::the_opt_out_leaves_guided_asking_for_tools`.
    #[must_use]
    pub fn with_auto_install(mut self, auto_install: bool) -> Self {
        self.auto_install = auto_install;
        self
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
            Command::Guided => self.guided().await.map(Outcome::Guided),
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
            // #5215: acquisition writes only under `self.work`, which is what
            // keeps `rm -rf <root>` a complete uninstall.
            Command::CloneRepos { repos, options } => {
                clone::clone_all(&self.work, &repos, &options, &self.progress)
                    .await
                    .map(Outcome::Cloned)
            }
            // #5822: registration validates before it persists, so every one of
            // these three reads or writes only `state/audit-targets.toml`.
            Command::AddTarget { kind, spec } => {
                self.add_target(kind, &spec).await.map(Outcome::Registered)
            }
            Command::ListTargets => self.list_targets().map(Outcome::Targets),
            Command::RemoveTarget { spec } => self.remove_target(&spec).await.map(Outcome::Removed),
            Command::Run(options) => self.run(&options).await.map(Outcome::Run),
            Command::Package { destination } => self.package(destination).map(Outcome::Package),
            // #5824: the one-shot chain over the four above. `crate::chain`
            // calls each of them unchanged, so this and they cannot drift.
            Command::Audit(options) => self.audit(&options).await.map(Outcome::Audit),
            Command::Distribute(options) => self.distribute(&options).map(Outcome::Distributed),
        }
    }

    /// Register one target, additively, after proving it can be read.
    ///
    /// Why: #5822. The order is the whole behaviour — parse, then read the
    /// existing set, then validate, and only then write. A target that fails
    /// validation is never persisted, and the targets already registered are
    /// never rewritten, so a refusal costs the operator nothing.
    ///
    /// A target that is already registered returns early WITHOUT re-validating.
    /// Idempotent means no-op: re-running `add` over a set to make sure it is
    /// complete must not start failing over a network blip on an entry that
    /// already passed.
    /// What: [`registry::parse`] owns the spec, [`validate::validate`] owns the
    /// access check, and [`registry::register`] owns the write — including the
    /// lock that stops two concurrent `add` runs discarding each other's target.
    /// Test: `super::session_tests::a_rejected_target_is_not_persisted`,
    /// `super::session_tests::re_adding_a_registered_target_changes_nothing`,
    /// `crate::registry::registry_tests::concurrent_registrations_keep_every_target`.
    async fn add_target(&self, kind: TargetKind, spec: &str) -> Result<Registration, AuditError> {
        let target = registry::parse(Some(kind), spec)?;
        self.work.create()?;
        if Registry::load(&self.work)?.contains(&target) {
            return Ok(Registration {
                target,
                already_registered: true,
            });
        }
        // Absent rather than required: a repository target needs no config at
        // all, and a board target's refusal names the field to set (#5822).
        let config = EngagementConfig::load_if_present(&self.config_path)?;
        // #5822: validation runs OUTSIDE the registry's lock — it reaches the
        // network under a 30s ceiling, and holding the lock across that would
        // stall every other `add` in this working directory behind one
        // unreachable site. `register` re-reads the file, so the append is
        // decided against the snapshot current at write time.
        validate::validate(&target, config.as_ref(), self.repo_probe).await?;
        let inserted = self
            .under_registry_lock({
                let target = target.clone();
                move |work| registry::register(work, &target)
            })
            .await?;
        Ok(Registration {
            target,
            // A concurrent writer that won the race registered the same target
            // first; from this call's side that is the idempotent no-op.
            already_registered: !inserted,
        })
    }

    /// Run one registry critical section off the async runtime's thread.
    ///
    /// Why: [`trusty_common::file_lock::with_exclusive_lock`] blocks until the
    /// lock is free, and its own contract says an async caller must run it
    /// where blocking is safe (#5822).
    /// What: `spawn_blocking`. A panic escaping the section arrives as a
    /// `JoinError` and becomes [`AuditError::RegistryLock`], so a failed
    /// critical section can never read as a completed one.
    /// Test: `super::session_tests::removing_operates_on_the_same_registry`.
    async fn under_registry_lock<T, F>(&self, f: F) -> Result<T, AuditError>
    where
        T: Send + 'static,
        F: FnOnce(&WorkDir) -> Result<T, AuditError> + Send + 'static,
    {
        let work = self.work.clone();
        tokio::task::spawn_blocking(move || f(&work))
            .await
            .map_err(|source| AuditError::RegistryLock {
                path: Registry::path(&self.work),
                source: std::io::Error::other(source),
            })?
    }

    /// What is registered, plus the selection file the sweep still reads.
    ///
    /// See [`registry::legacy_selection`] for why both are reported.
    fn list_targets(&self) -> Result<TargetList, AuditError> {
        Ok(TargetList {
            targets: Registry::load(&self.work)?.targets().to_vec(),
            legacy_selection: registry::legacy_selection(&self.work)?,
        })
    }

    /// Drop one target. Removing one that is not registered writes nothing.
    ///
    /// Under the same lock `add` takes (#5822): a removal is the same
    /// load-mutate-save, so an unserialised one discards a concurrent add.
    async fn remove_target(&self, spec: &str) -> Result<Removal, AuditError> {
        let target = registry::parse(None, spec)?;
        let was_registered = self
            .under_registry_lock({
                let target = target.clone();
                move |work| registry::deregister(work, &target)
            })
            .await?;
        Ok(Removal {
            target,
            was_registered,
        })
    }

    /// Assemble the deliverable from what the last sweep left behind.
    ///
    /// Why: #5499. The config is required for the same reason every other
    /// capability requires it — it carries the engagement metadata the package
    /// states, and the credential the member scan checks against.
    ///
    /// The completion signal is `RunProgress::complete`, not the record's mere
    /// presence. Since #5494 the record is written after every repository, so a
    /// sweep that died three repositories into six leaves one behind — and
    /// packaging that would send a partial engagement as a whole one. An
    /// incomplete record is refused with the count it holds, because the
    /// remedy is to re-run and resume, not to start over.
    /// What: loads the config, then hands off to [`package::from_checkpoint`],
    /// which owns the completion check — #5824 gave it a second caller, and a
    /// precondition enforced in two places is one that drifts.
    /// Test: `super::session_tests::packaging_before_any_sweep_is_refused`,
    /// `super::session_tests::packaging_an_unfinished_sweep_is_refused`.
    fn package(&self, destination: Option<PathBuf>) -> Result<ReturnPackage, AuditError> {
        // #5868: through `engagement_config` so the outbound credential scan
        // looks for the key the sweep actually used. `package::from_checkpoint`
        // refuses a deliverable containing `config.openrouter_key`; with an
        // environment key in play, loading the config directly would scan for
        // the wrong bytes and let the real one through.
        let config = self.engagement_config()?;
        let destination = destination.unwrap_or_else(|| package::default_destination(&self.work));
        // No unattempted targets: the standalone verb packages whatever the last
        // sweep recorded and knows nothing about a registry (#5824).
        package::from_checkpoint(&self.work, &config, &[], &destination)
    }

    /// Drive the whole engagement in one call (#5824).
    ///
    /// The config is loaded here for the same reason [`Session::run`] loads it,
    /// and `auto_install` is forwarded so `--no-install` means the same thing on
    /// the chained path as on the four separate ones.
    async fn audit(&self, options: &ChainOptions) -> Result<ChainReport, AuditError> {
        let config = self.engagement_config()?;
        chain::audit(
            &self.work,
            &config,
            options,
            self.auto_install,
            &self.progress,
        )
        .await
    }

    /// Assemble the install package that goes TO a client (#5825).
    ///
    /// Why: the only capability run on the AUDITOR's machine, so it takes
    /// nothing from the working directory — a client's engagement has not
    /// started yet. It reads the engagement config as a TEMPLATE rather than as
    /// this session's own config: the file on the auditor's disk supplies the
    /// instructions, the pins and the labels, and the credential that reaches
    /// the generated copy is preferably one that was never written down.
    ///
    /// The credential is resolved by the FRONT END rather than inside
    /// [`distribute::assemble`]. That keeps the assembly function pure — its
    /// tests never mutate a global every other test in the binary shares — and
    /// it is the same division `main.rs` already uses for
    /// `TRUSTY_AUDIT_WORKDIR`. No CLI flag carries the key: argv is
    /// world-readable through `ps` and lands in shell history.
    /// What: hands [`Session::credential`] straight through. Everything else —
    /// validation, the fail-open guards, the zip, and the fallback to the
    /// template's own key when nothing was supplied — is
    /// [`distribute::assemble`]'s and is unchanged.
    /// Test: `super::session_tests::distributing_without_a_template_is_refused`,
    /// `super::session_tests::a_credential_in_the_environment_is_the_one_packaged`,
    /// `super::session_tests::no_credential_in_the_environment_packages_the_templates`,
    /// `crate::distribute::distribute_tests`.
    fn distribute(&self, options: &DistributeOptions) -> Result<InstallPackage, AuditError> {
        distribute::assemble(
            &self.config_path,
            options,
            self.credential.as_ref(),
            &self.progress,
        )
    }

    /// The engagement config, with the front end's credential applied.
    ///
    /// Why: two things have to be true before a sweep starts, and both are
    /// decided once here so `run`, `audit` and `package` cannot disagree.
    /// `OPENROUTER_API_KEY` must BEAT the config's own key (#5868) — before
    /// this, `run` passed the config's key to the `tga audit` child
    /// unconditionally, so an exported variable was silently ignored. And the
    /// key that ends up in play must not be blank: a blank one is not a
    /// refusal anywhere downstream, it is
    /// [`crate::inference::inference_env`] returning NO variables, the child
    /// running without its `TRUSTY_REVIEW_*` selection, and `trusty-review`
    /// falling back to a provider nobody chose — discovered hours later at the
    /// report stage, or not at all.
    ///
    /// The check lives on `Session` rather than in the CLI prompt because the
    /// Tauri shell reaches this same code and must not be able to skip it.
    /// What: loads the config, overwrites `openrouter_key` when this session
    /// carries a resolved credential, then refuses a blank result.
    /// Test: `super::session_tests::a_supplied_credential_beats_the_configs_own`,
    /// `super::session_tests::a_blank_configured_key_refuses_before_the_sweep`.
    ///
    /// # Errors
    ///
    /// Whatever [`EngagementConfig::load`] failed with, and
    /// [`AuditError::BlankCredential`] when no usable key is in play.
    fn engagement_config(&self) -> Result<EngagementConfig, AuditError> {
        let mut config = EngagementConfig::load(&self.config_path)?;
        if let Some(credential) = &self.credential {
            config.openrouter_key = credential.clone();
        }
        if config.openrouter_key.is_empty() {
            return Err(AuditError::BlankCredential {
                config: self.config_path.clone(),
                env: run::ENV_INFERENCE_CREDENTIAL,
            });
        }
        Ok(config)
    }

    /// Read the engagement's key and pins, then sweep the selected repositories.
    ///
    /// The config is loaded for the same reason [`Session::install_tools`] loads
    /// it: it carries the OpenRouter key `tga audit`'s report render needs, and
    /// an absent config is a refusal rather than a run that will fail an hour in
    /// (#5555).
    async fn run(&self, options: &RunOptions) -> Result<RunReport, AuditError> {
        let config = self.engagement_config()?;
        // #5797: the sweep's own preflight refuses over a tool that is missing,
        // unverified, or off the pin. Auto-install closes exactly that set
        // first, using the same three conditions, so the operator does not run
        // `install` by hand between two commands that both already know the
        // pins. An install that cannot resolve every pin fails here and the
        // sweep never starts — the #5454 guarantee, reached earlier.
        if self.auto_install {
            tools::ensure(&self.work, &config.tools, &self.progress).await?;
        }
        run::sweep(&self.work, &config, options, &self.progress).await
    }

    /// Read the engagement's pins, then install exactly those.
    ///
    /// The config is loaded rather than defaulted: an absent or unreadable
    /// engagement config is a refusal, because the alternative — installing
    /// "latest" — is the version-skew defect #5454 closed (#5495).
    async fn install_tools(&self) -> Result<Vec<InstalledTool>, AuditError> {
        let config = EngagementConfig::load(&self.config_path)?;
        tools::install(&self.work, &config.tools, &self.progress).await
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

    async fn guided(&self) -> Result<GuidedStatus, AuditError> {
        self.work.create()?;
        let manifest = AuditManifest::load_if_present(&self.manifest_path)?;

        // #5502: the epic's pre-sweep order is repo selection, then tooling —
        // so a missing repository set outranks a missing binary.
        let repos_known = manifest
            .as_ref()
            .is_some_and(|m| !m.repositories.is_empty());

        // #5797: install at the point the flow would otherwise have printed
        // "now go run `install`", and not one step earlier. Repository selection
        // comes first, so a working directory with nothing chosen yet reports
        // its state without this process reaching the network — the operator
        // has not committed to an engagement here.
        let installed = if self.auto_install && repos_known {
            self.auto_install_tools().await?
        } else {
            None
        };

        let tools = tools::status(&self.work)?;
        let missing: Vec<RequiredTool> = tools
            .iter()
            .filter(|s| !s.installed)
            .map(|s| s.tool)
            .collect();

        // #5499: a finished sweep that audited something is the last state the
        // guided flow can advance from — without this the flow's final word is
        // "run the sweep", and the recipient is left holding a working directory
        // with no instruction to send anything back.
        //
        // #5494: FINISHED, not merely recorded. A checkpoint left by a sweep
        // that died names audited repositories too, and pointing at the return
        // package there would send a partial engagement instead of resuming.
        let audited = run::read_progress(&self.work)?.is_some_and(|progress| {
            progress.complete && progress.repos.iter().any(|r| r.result.succeeded())
        });

        let next = if audited {
            NextStep::ReturnPackage
        } else if !repos_known {
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
            installed,
            next,
        })
    }

    /// Install the pinned set for the guided flow, when there is a set to pin to.
    ///
    /// Why: #5797. The guided flow runs against a working directory that may
    /// carry no engagement config — it is the flow you enter before anything is
    /// set up — and that case has to keep reporting rather than fail. There are
    /// no pins without a config, and installing without pins means installing
    /// whatever is current, which is the #5454 defect. So an absent config
    /// declines to install and the flow names the step, exactly as before.
    ///
    /// A config that is PRESENT and unreadable or malformed is not that case and
    /// propagates: `load_if_present` tolerates only absence.
    /// What: `Ok(None)` when there is no config or the set was already
    /// satisfied; `Ok(Some(installed))` naming what this call placed.
    /// Test: `super::session_tests::guided_without_a_config_still_names_the_step`,
    /// `super::session_tests::guided_propagates_a_malformed_config`.
    async fn auto_install_tools(&self) -> Result<Option<Vec<InstalledTool>>, AuditError> {
        let Some(config) = EngagementConfig::load_if_present(&self.config_path)? else {
            return Ok(None);
        };
        tools::ensure(&self.work, &config.tools, &self.progress).await
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

    /// Record a sweep that reached the end of its selection (#5494).
    fn write_finished_progress(work: &WorkDir, report: &RunReport) {
        run::checkpoint::write_progress(work, &run::RunProgress::finished(report))
            .expect("write progress");
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
trusty-search = "0.0.0-never-published"
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

    /// The completion precondition: `run::sweep` writes the progress record
    /// last, so no record means no sweep finished — and packaging must say that
    /// rather than produce a zip holding two generated files (#5499).
    #[tokio::test]
    async fn packaging_before_any_sweep_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_with_config(tmp.path(), UNPUBLISHABLE_CONFIG);
        session
            .execute(Command::WorkDir)
            .await
            .expect("create tree");

        let err = session
            .execute(Command::Package { destination: None })
            .await
            .expect_err("there is nothing to package");
        let AuditError::NothingToPackage { reason } = &err else {
            panic!("expected NothingToPackage, got {err:?}");
        };
        assert!(reason.contains("trusty-audit run"), "{reason}");
        assert!(
            !crate::package::default_destination(session.work_dir()).exists(),
            "a refused package must leave no file"
        );
    }

    /// Read one member out of a written install package.
    fn package_member(zip_path: &Path, entry: &str) -> String {
        use std::io::Read as _;
        let file = std::fs::File::open(zip_path).expect("open package");
        let mut archive = zip::ZipArchive::new(file).expect("read package");
        let mut member = archive.by_name(entry).expect("member present");
        let mut text = String::new();
        member.read_to_string(&mut text).expect("member is text");
        text
    }

    /// Assemble an install package through `execute`, with `credential`
    /// standing in for what the front end resolved.
    ///
    /// Nothing here reads the real environment. That is the point, and #5868
    /// did not weaken it: a developer with `OPENROUTER_API_KEY` exported must
    /// not have their own credential written into a zip by `cargo test`. What
    /// changed is only the shape of the seam — the `fn(&str) -> Option<String>`
    /// lookup became the resolved value it would have produced, because the
    /// front end now resolves it from three sources rather than one.
    async fn distribute_through_execute(
        tmp: &Path,
        credential: Option<SecretKey>,
    ) -> InstallPackage {
        let session = session_with_config(tmp, UNPUBLISHABLE_CONFIG).with_credential(credential);
        let binary = tmp.join("taudit-fixture");
        std::fs::write(&binary, b"bytes to copy").expect("write binary");

        let outcome = session
            .execute(Command::Distribute(DistributeOptions {
                output_dir: Some(tmp.join("packages")),
                binary: Some(binary),
            }))
            .await
            .expect("assembles");
        let Outcome::Distributed(package) = outcome else {
            panic!("expected Distributed");
        };
        package
    }

    /// #5825: the inbound package reaches the recipient through `execute`, the
    /// same door as everything else — and it is the AUDITOR-side capability, so
    /// it needs no sweep, no manifest, and nothing in the working directory.
    #[tokio::test]
    async fn distributing_builds_a_package_from_the_template_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let package = distribute_through_execute(tmp.path(), None).await;

        assert_eq!(
            package.path,
            tmp.path()
                .join("packages")
                .join(crate::distribute::PACKAGE_FILE_NAME),
            "the package lands where the operator asked"
        );
        assert!(package.path.is_file());
        assert_eq!(package.files.len(), 4);
        assert!(!package.platform.is_empty());
    }

    /// A config carrying pins and instructions but no key — the shape a client
    /// receives when the auditor conveys the key out of band (#5825).
    const BLANK_KEY_CONFIG: &str = r#"
openrouter_key = ""
instructions = "Assess the last 52 weeks."

[tools]
tga = "0.0.0-never-published"
trusty-search = "0.0.0-never-published"
trusty-analyze = "0.0.0-never-published"
trusty-review = "0.0.0-never-published"
"#;

    /// #5868: which capabilities a front end must resolve a credential for is a
    /// property of the capability. The exhaustive match makes a new variant a
    /// compile error; this pins the answers it gives today, so a variant moved
    /// between tiers is a deliberate edit rather than a silent one.
    #[test]
    fn only_the_inference_capabilities_need_a_credential() {
        assert_eq!(
            Command::Run(RunOptions::default()).credential_need(),
            CredentialNeed::Required
        );
        assert_eq!(
            Command::Audit(ChainOptions::default()).credential_need(),
            CredentialNeed::Required
        );
        // These handle a key without ever needing to obtain one, so they read
        // the environment and never prompt.
        assert_eq!(
            Command::Package { destination: None }.credential_need(),
            CredentialNeed::Environment
        );
        assert_eq!(
            Command::Distribute(DistributeOptions::default()).credential_need(),
            CredentialNeed::Environment
        );
        // A capability that sends nothing for inference must never make a
        // client type a key to see their own working directory.
        for command in [
            Command::Guided,
            Command::WorkDir,
            Command::Manifest,
            Command::Tools,
            Command::InstallTools,
            Command::Repos,
            Command::DiscoverRepos,
            Command::ListTargets,
        ] {
            assert_eq!(
                command.credential_need(),
                CredentialNeed::None,
                "{command:?} should need no credential"
            );
        }
    }

    /// #5868: `OPENROUTER_API_KEY` beats the config's own key. Before this,
    /// `run` handed the config's key to the `tga audit` child unconditionally,
    /// so an exported variable was silently ignored.
    #[test]
    fn a_supplied_credential_beats_the_configs_own() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_with_config(tmp.path(), UNPUBLISHABLE_CONFIG);

        // Without one, the config's key stands.
        let unchanged = session.engagement_config().expect("loads");
        assert_eq!(unchanged.openrouter_key.expose(), "sk-or-v1-not-a-real-key");

        let overridden = session
            .with_credential(Some(SecretKey::new("sk-or-v1-resolved")))
            .engagement_config()
            .expect("loads");
        assert_eq!(overridden.openrouter_key.expose(), "sk-or-v1-resolved");
        // One field, not a whole config: the pins have to survive.
        assert_eq!(overridden.tools.tga.version(), "0.0.0-never-published");
    }

    /// #5868: a present-but-blank key must be refused before the sweep starts.
    ///
    /// It was not a refusal anywhere downstream. `inference::inference_env`
    /// reads a blank key as "select nothing" and returns NO variables, so the
    /// `tga audit` child runs without its `TRUSTY_REVIEW_*` selection and
    /// `trusty-review` falls back to its Bedrock default — found hours later at
    /// the report stage, or not at all if the fallback happens to work.
    #[tokio::test]
    async fn a_blank_configured_key_refuses_before_the_sweep() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_with_config(tmp.path(), BLANK_KEY_CONFIG);

        let err = session
            .execute(Command::Run(RunOptions::default()))
            .await
            .expect_err("a blank key cannot run an audit");
        assert!(matches!(err, AuditError::BlankCredential { .. }), "{err:?}");

        // Actionable: it names both ways to put a key in play.
        let text = err.to_string();
        assert!(text.contains(run::ENV_INFERENCE_CREDENTIAL), "{text}");
        assert!(text.contains("openrouter_key"), "{text}");

        // And a resolved credential is exactly what unblocks it — the refusal
        // is about the key in play, not about the file's own field.
        let config = session
            .with_credential(Some(SecretKey::new("sk-or-v1-resolved")))
            .engagement_config()
            .expect("a supplied key makes the same config usable");
        assert_eq!(config.openrouter_key.expose(), "sk-or-v1-resolved");
    }

    /// The check lives on `Session`, not in the CLI prompt, so a front end that
    /// never prompts still cannot start a sweep on a blank key.
    #[test]
    fn a_blank_supplied_credential_is_refused_too() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = session_with_config(tmp.path(), UNPUBLISHABLE_CONFIG)
            .with_credential(Some(SecretKey::new("   ")))
            .engagement_config()
            .expect_err("whitespace is not a credential");
        assert!(matches!(err, AuditError::BlankCredential { .. }), "{err:?}");
    }

    /// #5825: an exported credential is the one that reaches the recipient.
    ///
    /// The whole `execute` → `Session::distribute` → `distribute::assemble`
    /// chain is under test, in both directions, which is what the environment
    /// read being a global made unprovable: pointing `distribute` at the wrong
    /// variable name, or dropping `supplied` on the way to `assemble`, leaves
    /// this test packaging the template's key and failing.
    #[tokio::test]
    async fn a_credential_in_the_environment_is_the_one_packaged() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let package = distribute_through_execute(
            tmp.path(),
            Some(SecretKey::new("sk-or-v1-from-the-environment")),
        )
        .await;

        assert!(package.key_from_environment);
        let config = package_member(&package.path, "trusty-audit/engagement.toml");
        assert!(config.contains("sk-or-v1-from-the-environment"), "{config}");
        assert!(!config.contains("sk-or-v1-not-a-real-key"), "{config}");
    }

    /// #5825: the other direction — nothing exported, so the template's key
    /// ships and `key_from_environment` says so.
    #[tokio::test]
    async fn no_credential_in_the_environment_packages_the_templates() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let package = distribute_through_execute(tmp.path(), None).await;

        assert!(!package.key_from_environment);
        let config = package_member(&package.path, "trusty-audit/engagement.toml");
        assert!(config.contains("sk-or-v1-not-a-real-key"), "{config}");
    }

    /// #5825: a missing template is a refusal, not an empty config the
    /// recipient discovers on their own machine.
    #[tokio::test]
    async fn distributing_without_a_template_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let out = tmp.path().join("packages");
        // `session_in` points the config path at a file nothing wrote.
        let session = session_in(tmp.path());

        let err = session
            .execute(Command::Distribute(DistributeOptions {
                output_dir: Some(out.clone()),
                binary: None,
            }))
            .await
            .expect_err("no template, no package");
        assert!(
            matches!(
                err,
                AuditError::MissingPackageInput {
                    what: "engagement config template",
                    ..
                }
            ),
            "{err:?}"
        );
        assert!(!out.exists(), "a refused package must leave no directory");
    }

    /// #5494: the checkpoint is written after every repository now, so a record
    /// EXISTING no longer means a sweep finished. Packaging one that did not
    /// would send a partial engagement as a whole one — the same fail-open
    /// shape as packaging before any sweep, reached by a different route. The
    /// refusal names the resume, because that is the remedy.
    #[tokio::test]
    async fn packaging_an_unfinished_sweep_is_refused() {
        use crate::run::{RepoResult, RepoRun, SelectedRepo};

        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_with_config(tmp.path(), UNPUBLISHABLE_CONFIG);
        session
            .execute(Command::WorkDir)
            .await
            .expect("create tree");
        let done = vec![RepoRun {
            repo: SelectedRepo {
                name: "acme-api".to_owned(),
                path: PathBuf::from("repos/acme-api"),
            },
            output: session.work_dir().path(Area::Output).join("00-acme-api"),
            log: session.work_dir().path(Area::Logs).join("00-acme-api.log"),
            gaps: Vec::new(),
            resumed: false,
            result: RepoResult::Succeeded,
        }];
        run::checkpoint::write_progress(session.work_dir(), &run::RunProgress::checkpoint(&done))
            .expect("write checkpoint");

        let err = session
            .execute(Command::Package { destination: None })
            .await
            .expect_err("an interrupted sweep is not a deliverable");
        let AuditError::NothingToPackage { reason } = &err else {
            panic!("expected NothingToPackage, got {err:?}");
        };
        assert!(reason.contains("did not finish"), "{reason}");
        assert!(reason.contains("1 repository"), "{reason}");
        assert!(reason.contains("resume"), "{reason}");
        assert!(
            !crate::package::default_destination(session.work_dir()).exists(),
            "a refused package must leave no file"
        );
    }

    /// The guided flow reads the same completion signal: a checkpoint from a
    /// sweep that died must send the operator back to `run` to resume, not on
    /// to the return package (#5494).
    #[tokio::test]
    async fn an_interrupted_sweep_is_not_a_return_package() {
        use crate::run::{RepoResult, RepoRun, SelectedRepo};

        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());
        session
            .execute(Command::WorkDir)
            .await
            .expect("create tree");
        write_manifest(&session, MANIFEST);
        run::checkpoint::write_progress(
            session.work_dir(),
            &run::RunProgress::checkpoint(&[RepoRun {
                repo: SelectedRepo {
                    name: "acme-api".to_owned(),
                    path: PathBuf::from("repos/acme-api"),
                },
                output: session.work_dir().path(Area::Output).join("00-acme-api"),
                log: session.work_dir().path(Area::Logs).join("00-acme-api.log"),
                gaps: Vec::new(),
                resumed: false,
                result: RepoResult::Succeeded,
            }]),
        )
        .expect("write checkpoint");

        let Outcome::Guided(status) = session.execute(Command::Guided).await.expect("runs") else {
            panic!("Guided command must yield a Guided outcome");
        };
        assert_ne!(
            status.next,
            NextStep::ReturnPackage,
            "an interrupted sweep must be resumed, not packaged"
        );
    }

    /// No engagement config means no metadata and no key to scan against, so
    /// packaging refuses for the same reason installing and running do.
    #[tokio::test]
    async fn packaging_without_an_engagement_config_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());

        let err = session
            .execute(Command::Package { destination: None })
            .await
            .expect_err("no config, no package");
        assert!(matches!(err, AuditError::Read { .. }), "{err:?}");
    }

    /// The guided flow's last state: once a sweep has audited something, the
    /// step it names is sending the deliverable back (#5499).
    #[tokio::test]
    async fn guided_points_at_the_return_package_once_a_sweep_has_audited_something() {
        use crate::run::{RepoResult, RepoRun, RunReport, SelectedRepo};

        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());
        session
            .execute(Command::WorkDir)
            .await
            .expect("create tree");
        write_manifest(&session, MANIFEST);
        let report = RunReport::of(vec![RepoRun {
            repo: SelectedRepo {
                name: "acme-api".to_owned(),
                path: PathBuf::from("repos/acme-api"),
            },
            output: session.work_dir().path(Area::Output).join("00-acme-api"),
            log: session.work_dir().path(Area::Logs).join("00-acme-api.log"),
            gaps: Vec::new(),
            resumed: false,
            result: RepoResult::Succeeded,
        }]);
        write_finished_progress(session.work_dir(), &report);

        let Outcome::Guided(status) = session.execute(Command::Guided).await.expect("runs") else {
            panic!("Guided command must yield a Guided outcome");
        };
        assert_eq!(status.next, NextStep::ReturnPackage);
    }

    /// A working directory whose `tools/` is a symlink, so `tools::install`
    /// refuses at its own guard BEFORE building an HTTP client. That is what
    /// makes "auto-install fired here" assertable offline: reaching the guard is
    /// only possible by having decided to install (#5797).
    #[cfg(unix)]
    fn session_that_cannot_install(tmp: &Path) -> Session {
        let session = session_with_config(tmp, UNPUBLISHABLE_CONFIG);
        let work = session.work_dir();
        std::fs::create_dir_all(work.root()).expect("mkdir root");
        std::fs::create_dir_all(tmp.join("elsewhere")).expect("mkdir elsewhere");
        std::os::unix::fs::symlink(tmp.join("elsewhere"), work.path(Area::Tools)).expect("symlink");
        session
    }

    /// The behaviour the owner asked for: once repositories are chosen, the
    /// guided flow installs the pinned set instead of printing an instruction to
    /// go and do it (#5797). Before this change the same call returned `Ok`.
    #[cfg(unix)]
    #[tokio::test]
    async fn guided_installs_the_pinned_set_once_repositories_are_known() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_that_cannot_install(tmp.path());
        write_manifest(&session, MANIFEST);

        let err = session
            .execute(Command::Guided)
            .await
            .expect_err("the guided flow must have tried to install");
        assert!(
            matches!(err, AuditError::UnsafeToolsDir { .. }),
            "expected the install guard, got {err:?}"
        );
    }

    /// The opt-out restores the previous behaviour exactly: no network, and the
    /// flow names the step it would have performed (#5797).
    #[cfg(unix)]
    #[tokio::test]
    async fn the_opt_out_leaves_guided_asking_for_tools() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_that_cannot_install(tmp.path()).with_auto_install(false);
        write_manifest(&session, MANIFEST);

        let Outcome::Guided(status) = session
            .execute(Command::Guided)
            .await
            .expect("--no-install must not reach the installer")
        else {
            panic!("Guided command must yield a Guided outcome");
        };
        assert_eq!(
            status.next,
            NextStep::InstallTools(RequiredTool::ALL.to_vec())
        );
        assert!(status.installed.is_none(), "nothing was installed");
    }

    /// Repository selection comes first, so a working directory with nothing
    /// chosen yet reports its state without reaching the network (#5502 order).
    #[cfg(unix)]
    #[tokio::test]
    async fn guided_does_not_install_before_repositories_are_chosen() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_that_cannot_install(tmp.path());

        let Outcome::Guided(status) = session
            .execute(Command::Guided)
            .await
            .expect("selection comes before any download")
        else {
            panic!("Guided command must yield a Guided outcome");
        };
        assert_eq!(status.next, NextStep::SelectRepositories);
        assert!(status.installed.is_none());
    }

    /// No config means no pins, and no pins must never mean "fetch latest" — so
    /// the flow declines to install and names the step, as it always did.
    #[cfg(unix)]
    #[tokio::test]
    async fn guided_without_a_config_still_names_the_step() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());
        session
            .execute(Command::WorkDir)
            .await
            .expect("create tree");
        write_manifest(&session, MANIFEST);

        let Outcome::Guided(status) = session
            .execute(Command::Guided)
            .await
            .expect("an absent config is a state to report")
        else {
            panic!("Guided command must yield a Guided outcome");
        };
        assert_eq!(
            status.next,
            NextStep::InstallTools(RequiredTool::ALL.to_vec())
        );
    }

    /// A config that is present and wrong is not the same as one that is
    /// absent: `load_if_present` tolerates only absence.
    #[tokio::test]
    async fn guided_propagates_a_malformed_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_with_config(tmp.path(), "this is not toml = = =");
        write_manifest(&session, MANIFEST);

        let err = session
            .execute(Command::Guided)
            .await
            .expect_err("a malformed config must not be swallowed");
        assert!(matches!(err, AuditError::Parse { .. }), "{err:?}");
    }

    /// The sweep's tooling is a prerequisite it can satisfy itself. Before this
    /// change the same call refused with `ToolsNotInstalled` (#5797).
    #[cfg(unix)]
    #[tokio::test]
    async fn run_installs_the_pinned_set_before_sweeping() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_that_cannot_install(tmp.path());

        let err = session
            .execute(Command::Run(RunOptions::default()))
            .await
            .expect_err("the sweep must have tried to install");
        assert!(
            matches!(err, AuditError::UnsafeToolsDir { .. }),
            "expected the install guard, got {err:?}"
        );
    }

    /// With the opt-out, `run` refuses exactly as it did before auto-install —
    /// the fail-closed preflight, untouched.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_opt_out_leaves_run_refusing_over_missing_tools() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_that_cannot_install(tmp.path()).with_auto_install(false);

        let err = session
            .execute(Command::Run(RunOptions::default()))
            .await
            .expect_err("no tools, no sweep");
        assert!(
            matches!(err, AuditError::ToolsNotInstalled { .. }),
            "expected the sweep's own preflight, got {err:?}"
        );
    }

    /// An engagement config with no board credentials, so a board registration
    /// refuses at the credential check without reaching the network.
    const CONFIG_WITHOUT_BOARDS: &str = r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks."

[tools]
tga = "2.9.4"
trusty-search = "0.47.0"
trusty-analyze = "0.9.2"
trusty-review = "0.15.1"
"#;

    fn registry_of(session: &Session) -> Vec<String> {
        Registry::load(session.work_dir())
            .expect("the registry reads")
            .targets()
            .iter()
            .map(crate::registry::Target::id)
            .collect()
    }

    /// #5822's central guarantee: validation runs BEFORE the write, so a target
    /// that cannot be checked leaves no file behind at all.
    #[tokio::test]
    async fn a_rejected_target_is_not_persisted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_with_config(tmp.path(), CONFIG_WITHOUT_BOARDS);

        let err = session
            .execute(Command::AddTarget {
                kind: TargetKind::Board,
                spec: "jira:ACME".to_owned(),
            })
            .await
            .expect_err("no jira credential, no registration");
        assert!(
            matches!(err, AuditError::BoardCredentialMissing { .. }),
            "{err:?}"
        );
        assert!(
            !Registry::path(session.work_dir()).exists(),
            "a refused registration wrote a registry file"
        );
    }

    /// The REPOSITORY arm of the same guarantee, deterministically (#5822).
    /// `a_rejected_target_is_not_persisted` covers the board arm; the repo arm's
    /// only refusal coverage was `validate`'s `#[ignore]`d live test, so a
    /// default `cargo test -p trusty-audit` never proved that a refused
    /// repository leaves no file behind.
    #[tokio::test]
    async fn a_refused_repository_registration_writes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path()).with_repo_probe(validate::RepoProbe::unusable());

        let err = session
            .execute(Command::AddTarget {
                kind: TargetKind::Repo,
                spec: "acme/api".to_owned(),
            })
            .await
            .expect_err("a gh that cannot answer must not register a repository");
        assert!(matches!(err, AuditError::RepoUnreachable { .. }), "{err:?}");
        assert!(
            !Registry::path(session.work_dir()).exists(),
            "a refused repository registration wrote a registry file"
        );
    }

    /// The message names the field to set — the recipient is not the author of
    /// this config and cannot infer it.
    #[tokio::test]
    async fn a_board_without_a_credential_names_the_config_field() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_with_config(tmp.path(), CONFIG_WITHOUT_BOARDS);

        let err = session
            .execute(Command::AddTarget {
                kind: TargetKind::Board,
                spec: "linear:ENG".to_owned(),
            })
            .await
            .expect_err("no linear credential");
        let rendered = err.to_string();
        assert!(rendered.contains("boards.linear"), "{rendered}");
        assert!(rendered.contains("nothing was registered"), "{rendered}");
    }

    /// A malformed spec is refused before anything is read or written.
    #[tokio::test]
    async fn a_spec_that_is_not_a_target_is_refused_before_any_write() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());

        let err = session
            .execute(Command::AddTarget {
                kind: TargetKind::Repo,
                spec: "../etc/passwd".to_owned(),
            })
            .await
            .expect_err("a traversing name must not register");
        assert!(matches!(err, AuditError::InvalidRepoName { .. }), "{err:?}");
        assert!(!Registry::path(session.work_dir()).exists());
    }

    /// Registration is additive and idempotent, and a repeat does not
    /// re-validate — which is why this passes with no credential configured.
    #[tokio::test]
    async fn re_adding_a_registered_target_changes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_with_config(tmp.path(), CONFIG_WITHOUT_BOARDS);
        session
            .execute(Command::WorkDir)
            .await
            .expect("create tree");

        // Seed directly: the add path would need the network to get this far.
        let mut registry = Registry::default();
        registry
            .insert(crate::registry::parse(Some(TargetKind::Board), "jira:ACME").expect("parses"));
        registry.save(session.work_dir()).expect("writes");

        let Outcome::Registered(again) = session
            .execute(Command::AddTarget {
                kind: TargetKind::Board,
                spec: "JIRA:acme".to_owned(),
            })
            .await
            .expect("a repeat must not re-validate")
        else {
            panic!("AddTarget must yield a Registered outcome");
        };
        assert!(again.already_registered);
        assert_eq!(registry_of(&session), vec!["jira:ACME"]);
    }

    /// Adding one target never disturbs the ones already registered.
    #[tokio::test]
    async fn registration_is_additive() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_with_config(tmp.path(), CONFIG_WITHOUT_BOARDS);
        session
            .execute(Command::WorkDir)
            .await
            .expect("create tree");

        let mut registry = Registry::default();
        registry.insert(crate::registry::parse(None, "acme/api").expect("parses"));
        registry.insert(crate::registry::parse(None, "jira:ACME").expect("parses"));
        registry.save(session.work_dir()).expect("writes");

        // A refused add leaves both entries exactly as they were.
        session
            .execute(Command::AddTarget {
                kind: TargetKind::Board,
                spec: "linear:ENG".to_owned(),
            })
            .await
            .expect_err("no linear credential");
        assert_eq!(registry_of(&session), vec!["acme/api", "jira:ACME"]);
    }

    /// The credential lives in the engagement config and nowhere else: the
    /// registry's schema has no field one could be written into.
    #[tokio::test]
    async fn no_credential_reaches_the_registry_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = format!(
            "{CONFIG_WITHOUT_BOARDS}\n[boards.jira]\nurl = \"https://acme.atlassian.net\"\n\
             email = \"auditor@acme.example\"\ntoken = \"jira-token-secret\"\n\
             \n[boards.linear]\napi_key = \"lin_api_secret\"\n"
        );
        let session = session_with_config(tmp.path(), &config);
        session
            .execute(Command::WorkDir)
            .await
            .expect("create tree");

        let mut registry = Registry::default();
        registry.insert(crate::registry::parse(None, "jira:ACME").expect("parses"));
        registry.insert(crate::registry::parse(None, "linear:ENG").expect("parses"));
        registry.save(session.work_dir()).expect("writes");

        let text = std::fs::read_to_string(Registry::path(session.work_dir())).expect("read");
        assert!(!text.contains("jira-token-secret"), "{text}");
        assert!(!text.contains("lin_api_secret"), "{text}");
        assert!(!text.contains("auditor@acme.example"), "{text}");
        assert!(
            text.contains("jira:ACME") || text.contains("ACME"),
            "{text}"
        );
    }

    /// Removing a registered target writes; removing an unregistered one does
    /// not, and neither is a failure.
    #[tokio::test]
    async fn removing_operates_on_the_same_registry() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());
        session
            .execute(Command::WorkDir)
            .await
            .expect("create tree");
        let mut registry = Registry::default();
        registry.insert(crate::registry::parse(None, "acme/api").expect("parses"));
        registry.insert(crate::registry::parse(None, "acme/web").expect("parses"));
        registry.save(session.work_dir()).expect("writes");

        let Outcome::Removed(gone) = session
            .execute(Command::RemoveTarget {
                spec: "acme/api".to_owned(),
            })
            .await
            .expect("removes")
        else {
            panic!("RemoveTarget must yield a Removed outcome");
        };
        assert!(gone.was_registered);
        assert_eq!(registry_of(&session), vec!["acme/web"]);

        let Outcome::Removed(absent) = session
            .execute(Command::RemoveTarget {
                spec: "jira:ACME".to_owned(),
            })
            .await
            .expect("removing something unregistered is not a failure")
        else {
            panic!("RemoveTarget must yield a Removed outcome");
        };
        assert!(!absent.was_registered);
        assert_eq!(registry_of(&session), vec!["acme/web"]);
    }

    /// `targets` reads the same registry `add` and `remove` write, and names
    /// the selection file the sweep still reads (#5822).
    #[tokio::test]
    async fn listing_reports_the_registry_and_the_sweeps_own_selection() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());
        session
            .execute(Command::WorkDir)
            .await
            .expect("create tree");

        let Outcome::Targets(empty) = session
            .execute(Command::ListTargets)
            .await
            .expect("an empty registry is not a failure")
        else {
            panic!("ListTargets must yield a Targets outcome");
        };
        assert!(empty.targets.is_empty());
        assert!(empty.legacy_selection.is_none());

        let mut registry = Registry::default();
        registry.insert(crate::registry::parse(None, "acme/api").expect("parses"));
        registry.save(session.work_dir()).expect("writes");
        run::save_selection(
            session.work_dir(),
            &[run::SelectedRepo {
                name: "acme/api".to_owned(),
                path: PathBuf::from("repos/acme/api"),
            }],
        )
        .expect("writes");

        let Outcome::Targets(list) = session.execute(Command::ListTargets).await.expect("reads")
        else {
            panic!("ListTargets must yield a Targets outcome");
        };
        assert_eq!(list.targets.len(), 1);
        let (path, count) = list.legacy_selection.expect("the selection is named");
        assert_eq!(path, run::selection_path(session.work_dir()));
        assert_eq!(count, 1);
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
