//! The command-line front end — argument parsing and rendering.
//!
//! Why: this module lives in the LIBRARY, not in `main.rs`, for two reasons.
//! Every CLI invocation is then unit-testable without spawning a process, which
//! is what lets `cli_tests::every_command_variant_has_a_cli_invocation` stand as
//! a real gate rather than a comment. And `main.rs` stays a twenty-line shim, so
//! there is nowhere for logic to accumulate outside the API the Tauri shell will
//! call (#5502's permanent constraint).
//!
//! What: [`Cli`], the clap parser; `Cli::command`, which maps a parse onto
//! exactly one [`Command`]; and [`render`], which turns an [`Outcome`] into the
//! text the CLI prints. Both mappings are exhaustive matches over the
//! capability enums, so adding a capability without a CLI path — or without a
//! way to display its result — is a compile error.
//! Test: `super::cli_tests`.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use crate::chain::ChainOptions;
use crate::clone::CloneOptions;
use crate::distribute::{DistributeOptions, ReportPreset};
use crate::registry::TargetKind;
use crate::rerender::RerenderOptions;
use crate::run::RunOptions;
use crate::session::{Command, Outcome};

/// The auditor client's command line.
///
/// Why: a bare invocation is the documented entry point — the recipient
/// double-clicks or types one word, and the guided flow starts at repository
/// selection. Explicit subcommands exist for every capability anyway, so each
/// one is individually drivable in a test or a script.
/// What: two global options plus an optional subcommand; absent means
/// [`Command::Guided`].
/// Test: `super::cli_tests::a_bare_invocation_enters_the_guided_flow`.
#[derive(Debug, Parser)]
#[command(
    name = "trusty-audit",
    version,
    about = "Auditor client — installs its pinned audit tooling and runs an audit engagement",
    long_about = None,
)]
pub struct Cli {
    /// Working-directory root (default: ~/.trusty-tools/trusty-audit/work, or $TRUSTY_AUDIT_WORKDIR).
    #[arg(long, global = true, value_name = "DIR")]
    pub work_dir: Option<PathBuf>,

    /// Companion manifest.toml (default: <work-dir>/out/manifest.toml).
    #[arg(long, global = true, value_name = "FILE")]
    pub manifest: Option<PathBuf>,

    /// Engagement config from the handoff package (default: ./engagement.toml).
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Never download the pinned tools; report what is missing instead.
    ///
    /// The guided flow and `run` install the tools this engagement pins when
    /// they are absent, unverified, or off the pin. Pass this to keep those
    /// commands off the network and get the state as it stands. `tools` never
    /// installs, with or without this flag.
    #[arg(long, global = true)]
    pub no_install: bool,

    /// Capability to run. Omit to enter the guided flow.
    #[command(subcommand)]
    pub verb: Option<Verb>,
}

/// The report preset `taudit distribute --template` accepts (#5483).
///
/// Why: a clap `ValueEnum` rather than a free-form string, so `--template cst`
/// is a usage error naming the two accepted values instead of an engagement
/// that renders a template nothing bundles. The set is deliberately narrow —
/// this flag selects a PRESET, and an engagement wanting some other bundled
/// template still writes `[report] template = "…"` in its own config.
/// What: one variant per preset, converted into
/// [`ReportPreset`](crate::distribute::ReportPreset) at the call site.
/// Test: `super::cli_tests::the_cast_template_flag_selects_the_cast_preset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReportTemplateArg {
    /// CAST-style technical due diligence, scoped to the code.
    Cast,
    /// Leave the template's own `[report]` table in charge.
    Default,
}

impl From<ReportTemplateArg> for ReportPreset {
    fn from(arg: ReportTemplateArg) -> Self {
        match arg {
            ReportTemplateArg::Cast => ReportPreset::Cast,
            ReportTemplateArg::Default => ReportPreset::Inherit,
        }
    }
}

/// One subcommand per capability.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Verb {
    /// Walk the pre-run steps: repository selection, then tooling.
    Guided,
    /// Create `engagement.toml` without a terminal, for a scripted run.
    ///
    /// The README's sequence — workdir, add repo, targets, audit — begins at a
    /// step that needs a config, and until now only a bare `trusty-audit` on a
    /// real terminal ever wrote one. A CI or scripted caller had no way to get
    /// past `add repo` except by faking a pty. This is that way: it reads
    /// `OPENROUTER_API_KEY` from the environment, never prompts, and writes the
    /// same file the interactive cold start writes. A directory that is already
    /// an engagement is left exactly as it is.
    Init,
    /// Create the working directory and print what lands where.
    Workdir,
    /// Print the engagement metadata from the companion manifest.
    Manifest,
    /// Report which pinned tools are installed, and at which verified versions.
    Tools,
    /// Download and verify the pinned tools named in the engagement config.
    Install,
    /// List the repositories this engagement is configured to audit.
    Repos,
    /// List the repositories your GitHub credential can reach.
    Discover,
    /// Clone the named repositories into the working directory.
    Clone {
        /// Repositories to clone, as owner/name.
        #[arg(value_name = "OWNER/NAME", required = true)]
        repos: Vec<String>,
        /// Stop STARTING clones once this many gigabytes are on disk (0: never).
        ///
        /// Not a cap on one repository: a clone already running is never
        /// interrupted, so a single large repository can exceed this.
        #[arg(long, value_name = "GB")]
        budget_gb: Option<u64>,
    },
    /// Register an audit target, after checking it can be read.
    ///
    /// Register everything in scope, not only the obvious application
    /// repositories: the repository holding your database schema or migrations,
    /// infrastructure and IaC, shared libraries and config repositories, and
    /// every ticketing board in use. The assessment judges how mature, how
    /// stable and how supportable the technology is, on what is registered and
    /// nothing else.
    Add {
        /// What to register.
        #[command(subcommand)]
        target: AddTarget,
    },
    /// List the registered audit targets.
    ///
    /// Read it as the audit's coverage: a repository or board absent here —
    /// a schema repository, an IaC repository, a second ticketing board — is
    /// absent from the report too.
    Targets,
    /// Remove a registered audit target.
    Remove {
        /// The target, as owner/name or provider:key.
        #[arg(value_name = "TARGET")]
        target: String,
    },
    /// Run the audit sweep over the selected repositories.
    ///
    /// A repository an earlier run already audited is skipped, so an
    /// interrupted sweep is resumed rather than repeated. A repository the
    /// earlier run recorded as FAILED is retried.
    Run {
        /// Audit every selected repository again, ignoring recorded progress.
        ///
        /// Reach for this when the recorded outputs are stale rather than
        /// missing — re-cloned repositories, or a config change that should
        /// reach work already done. Hours-long: it re-collects everything.
        #[arg(long)]
        fresh: bool,
    },
    /// Run the whole engagement: install, clone, audit, package.
    ///
    /// One invocation over what `trusty-audit add` registered, instead of
    /// `install`, `clone`, `run` and `package` in order. Interrupt it and run it
    /// again: installed tools, complete checkouts and audited repositories are
    /// all carried over rather than redone.
    ///
    /// It continues past a repository that fails and names it in the package it
    /// assembles. It exits non-zero whenever anything registered was not
    /// audited, and it assembles nothing at all when no repository was.
    Audit {
        /// Audit every selected repository again, ignoring recorded progress.
        ///
        /// Hours-long: it re-collects everything. Same flag, same meaning, as
        /// `trusty-audit run --fresh`.
        #[arg(long)]
        fresh: bool,
        /// Where to write the return package (default: <work-dir>/audit-return-package.zip).
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
    /// Assemble the unencrypted deliverable zip to send back.
    Package {
        /// Where to write the zip (default: <work-dir>/audit-return-package.zip).
        ///
        /// The CLI's answer to a save dialog: name a path here and the file
        /// lands where you will attach it from. This is the one path on which
        /// this client writes outside the working directory.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
    /// Build the install package to send a client (auditor-side).
    ///
    /// Writes one zip holding this binary, a launcher, an engagement.toml
    /// carrying the OpenRouter key, and a README. The key comes from
    /// OPENROUTER_API_KEY when it is set, and from --config otherwise — there is
    /// deliberately no flag for it, because argv is visible to every process on
    /// this machine and lands in your shell history.
    ///
    /// Refuses rather than overwriting a package that is already there.
    Distribute {
        /// Directory to write the package into (default: ~/duetto/audit).
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,

        /// The taudit binary to ship (default: the one running).
        ///
        /// The default is right whenever you are packaging for a machine like
        /// this one. Name a path to ship a binary built for the client's
        /// platform instead.
        #[arg(long, value_name = "FILE")]
        binary: Option<PathBuf>,

        /// Ship no OpenRouter key; the recipient is asked for one on first run.
        ///
        /// Use this whenever the key reaches the recipient out of band, which
        /// is the normal handover — the package then carries a blank
        /// `openrouter_key`, and the first `audit` asks for it on the terminal
        /// and saves it. It beats `OPENROUTER_API_KEY` and the template's own
        /// key, so a variable left over from an earlier command cannot bake one
        /// in by accident.
        #[arg(long)]
        prompt_for_key: bool,

        /// Which report the packaged engagement produces (default: whatever the
        /// template's own `[report]` table says).
        ///
        /// `cast` writes `template = "cast"` and `code_only = true` into the
        /// generated `engagement.toml` — the CAST-style technical due-diligence
        /// report, scoped to what a repository checkout can prove.
        #[arg(long = "template", value_name = "NAME", value_enum)]
        report_template: Option<ReportTemplateArg>,

        /// Pre-populate the package's repository list from a file.
        ///
        /// Takes a `repos.txt` — one `owner/name`, absolute checkout path, or
        /// GitHub URL per line, `#` starting a comment — or an `engagement.toml`
        /// from a previous engagement, whose `[[targets]]` are reused. The
        /// generated config declares them, so `taudit audit` on the recipient's
        /// machine audits exactly that list and asks them to pick nothing.
        #[arg(long, value_name = "FILE")]
        repos: Option<PathBuf>,
    },
    /// Regenerate the reports from a finished audit package.
    ///
    /// Unzip the audit, change into the directory that came out of it, and run
    /// `trusty-audit render` with nothing after it. It reads the
    /// `engagement.toml` beside you for the OpenRouter key, runs the report step
    /// again over every manifest under `reports/`, and writes the fresh copies
    /// to `rerendered/` in that same directory. It clones nothing, collects
    /// nothing, and never writes into the package it read.
    ///
    /// Every flag below overrides one of those defaults; none is required.
    ///
    /// It needs an OpenRouter key: the report's executive summary is written by
    /// a model, so a re-render calls one. OPENROUTER_API_KEY wins, and the
    /// `openrouter_key` in the engagement config beside you is used when it is
    /// not set. The dimensions that need the repositories themselves — the code
    /// scan and the analysis pass — are named as gaps rather than silently left
    /// out, because an audit package carries no checkouts.
    Render {
        /// The unzipped audit package (default: this directory, else the work dir).
        #[arg(long, value_name = "DIR")]
        from: Option<PathBuf>,
        /// Where to write the regenerated reports (default: <from>/rerendered).
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// The trusty-review to run (default: the installed one, else PATH).
        #[arg(long, value_name = "FILE")]
        review_bin: Option<PathBuf>,
    },
}

/// What `taudit add` was asked to register.
///
/// Why: two verbs rather than one that guesses from the spelling. The operator
/// has already said which kind they mean, and carrying that through is what
/// makes `add repo jira:ACME` a refusal instead of a silently-registered board
/// (#5822).
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum AddTarget {
    /// A repository — on GitHub, or already checked out on this machine.
    ///
    /// Application, database-schema and migration, infrastructure, shared-library
    /// and config repositories all belong here.
    ///
    /// #6001: an ABSOLUTE path names a checkout on disk, checked by reading it
    /// rather than with your `gh` credential; anything else is a GitHub
    /// owner/name. The audit clones from the path and never modifies it.
    Repo {
        /// The repository, as owner/name or an absolute path to a checkout.
        #[arg(value_name = "OWNER/NAME|PATH")]
        name: String,
    },
    /// A JIRA project or Linear team, checked with the configured credential.
    Board {
        /// The board, as jira:PROJECT-KEY or linear:TEAM-KEY.
        #[arg(value_name = "PROVIDER:KEY")]
        id: String,
    },
}

impl Cli {
    /// The capability this invocation asks for.
    ///
    /// Why: the only bridge between argv and the API. Keeping it a total
    /// function — no `Option`, no fallible path — is what makes "a bare
    /// invocation is the entry point" a property of the type rather than a
    /// convention `main.rs` happens to follow.
    /// What: maps each [`Verb`] onto its [`Command`]; `None` maps to
    /// [`Command::Guided`].
    /// Test: `super::cli_tests::every_command_variant_has_a_cli_invocation`.
    pub fn to_command(&self) -> Command {
        match &self.verb {
            None | Some(Verb::Guided) => Command::Guided,
            // #6159: the non-interactive half of the cold start. It takes no
            // flags — every value it writes comes from the environment or the
            // pinned release list, which is what makes it scriptable.
            Some(Verb::Init) => Command::Init,
            Some(Verb::Workdir) => Command::WorkDir,
            Some(Verb::Manifest) => Command::Manifest,
            Some(Verb::Tools) => Command::Tools,
            Some(Verb::Install) => Command::InstallTools,
            Some(Verb::Repos) => Command::Repos,
            Some(Verb::Discover) => Command::DiscoverRepos,
            // #5215: `--budget-gb 0` is the explicit way to ask for no ceiling;
            // omitting the flag keeps `CloneOptions`' bounded default, because
            // the recipient who never thought about disk is the one the budget
            // exists for.
            Some(Verb::Clone { repos, budget_gb }) => Command::CloneRepos {
                repos: repos.clone(),
                options: CloneOptions {
                    budget_bytes: match budget_gb {
                        Some(0) => None,
                        // #5215 review: `--budget-gb 17179869184` overflowed u64 —
                        // a panic in debug, and in release a wrap to 0 that
                        // skipped every repo and ended in AllClonesFailed.
                        Some(gb) => Some(gb.saturating_mul(1024 * 1024 * 1024)),
                        None => CloneOptions::default().budget_bytes,
                    },
                },
            },
            // #5822: the verb decides the kind; `registry::parse` decides
            // whether the spec is usable, because `to_command` cannot fail.
            Some(Verb::Add {
                target: AddTarget::Repo { name },
            }) => Command::AddTarget {
                kind: TargetKind::Repo,
                spec: name.clone(),
            },
            Some(Verb::Add {
                target: AddTarget::Board { id },
            }) => Command::AddTarget {
                kind: TargetKind::Board,
                spec: id.clone(),
            },
            Some(Verb::Targets) => Command::ListTargets,
            Some(Verb::Remove { target }) => Command::RemoveTarget {
                spec: target.clone(),
            },
            // #5494: resume is the default and re-collection is the opt-in,
            // because the expensive direction is the one to ask for by name.
            Some(Verb::Run { fresh }) => Command::Run(RunOptions { fresh: *fresh }),
            Some(Verb::Package { out }) => Command::Package {
                destination: out.clone(),
            },
            // #5824: the same two knobs the phases it chains already take, and
            // no third one — anything else the chain needs it reads from the
            // registry, which is where the operator put it.
            Some(Verb::Audit { fresh, out }) => Command::Audit(ChainOptions {
                fresh: *fresh,
                destination: out.clone(),
            }),
            // #5825: the inbound package. A separate variant from `Package`
            // because the two travel opposite ways — see `crate::distribute`.
            Some(Verb::Distribute {
                out,
                binary,
                prompt_for_key,
                report_template,
                repos,
            }) => Command::Distribute(DistributeOptions {
                output_dir: out.clone(),
                binary: binary.clone(),
                prompt_for_key: *prompt_for_key,
                report_preset: report_template.map_or(ReportPreset::Inherit, Into::into),
                // #5483: the PATH travels, not the parsed list — `to_command`
                // cannot fail, so `distribute::assemble` does the read.
                repos: repos.clone(),
            }),
            // #6080: every knob defaults — the source to the directory this was
            // run in — so `taudit render` is the whole invocation the recipient
            // is given.
            Some(Verb::Render {
                from,
                out,
                review_bin,
            }) => Command::Rerender(RerenderOptions {
                from: from.clone(),
                out: out.clone(),
                review: review_bin.clone(),
            }),
        }
    }
}

/// The process exit status an outcome deserves.
///
/// Why: #5555's fail-open guard, extended by #5215's — `Session::execute`
/// returns `Ok` for a sweep or a clone that ran and only partly succeeded,
/// because the per-repo failures are data a front end must render, but a
/// shell, a CI job, or the operator reading `$?` must not see that as
/// success. Kept as a thin forwarder to [`Outcome::exit_code`] (the policy's
/// actual home, shared with the Tauri shell) so this crate's existing
/// call sites and tests do not have to move.
/// What: forwards to [`Outcome::exit_code`].
/// Test: `super::cli_tests::a_partial_sweep_does_not_exit_zero`,
/// `super::cli_tests::a_run_with_gaps_exits_non_zero`.
pub fn exit_code(outcome: &Outcome) -> i32 {
    outcome.exit_code()
}

// #5824: rendering moved out when the one-shot chain's own arm pushed this file
// past the 500-SLOC production cap. Re-exported, so `crate::cli::render` stays
// the name every caller and test already uses.
pub mod credential;
// #5970: a launch in a directory with no `engagement.toml` creates one instead
// of registering targets against an engagement that does not exist. Beside
// `credential` for the same reason: it prompts, and a prompt is a front-end
// concern the library must never acquire.
pub mod bootstrap;
// #5885: the launch walks the operator into registration rather than naming a
// command for them to run next. Beside `credential` because both are prompts,
// and a prompt is a front-end concern the library must never acquire.
pub mod registration;
mod render;
// #5978: `repos.txt` / `boards.txt` are the target list when they are present,
// so the per-target prompt loop is skipped rather than seeded. Parsing only —
// registering stays in `registration`, which owns the one path a target takes.
pub mod targets_file;
// #5978: one confirmation surface, reached two ways — the operator who supplied
// a targets file and the operator who typed targets both end at the same menu.
pub mod review;

pub use render::render;

#[cfg(test)]
mod cli_tests {
    use super::render::count_of;
    use super::*;
    use crate::clone::{CloneReport, CloneState, ClonedRepo};
    use crate::discover::DiscoveredRepo;
    use crate::distribute::InstallPackage;
    use crate::manifest::AuditManifest;
    use crate::package::{PackagedFile, ReturnPackage};
    use crate::registry::Target;
    use crate::run::RepoResult;
    use crate::session::EXIT_INCOMPLETE;
    use crate::session::{Session, WorkDirReport};
    use crate::tools::{InstalledTool, RequiredTool, ToolStatus};
    use crate::workdir::WorkDir;

    /// The CLI invocation that produces each capability.
    ///
    /// This match is the enforcement mechanism behind #5502's permanent
    /// constraint: adding a `Command` variant without giving it a CLI path
    /// fails to compile here, before any reviewer has to notice.
    fn argv_for(command: &Command) -> Vec<&'static str> {
        match command {
            Command::Guided => vec!["taudit", "guided"],
            Command::Init => vec!["taudit", "init"],
            Command::WorkDir => vec!["taudit", "workdir"],
            Command::Manifest => vec!["taudit", "manifest"],
            Command::Tools => vec!["taudit", "tools"],
            Command::InstallTools => vec!["taudit", "install"],
            Command::Repos => vec!["taudit", "repos"],
            Command::DiscoverRepos => vec!["taudit", "discover"],
            Command::CloneRepos { .. } => vec!["taudit", "clone", "acme/api"],
            Command::AddTarget {
                kind: TargetKind::Repo,
                ..
            } => vec!["taudit", "add", "repo", "acme/api"],
            Command::AddTarget {
                kind: TargetKind::Board,
                ..
            } => vec!["taudit", "add", "board", "jira:ACME"],
            Command::ListTargets => vec!["taudit", "targets"],
            Command::RemoveTarget { .. } => vec!["taudit", "remove", "acme/api"],
            Command::Run(_) => vec!["taudit", "run"],
            Command::Package { .. } => vec!["taudit", "package"],
            Command::Audit(_) => vec!["taudit", "audit"],
            Command::Distribute(_) => vec!["taudit", "distribute"],
            Command::Rerender(_) => vec!["taudit", "render"],
        }
    }

    /// Every capability, as values rather than a `const` — #5215's
    /// `CloneRepos` carries data, so the list cannot be a const array. The
    /// exhaustive match in `argv_for` is what still fails to compile when a
    /// capability arrives without a CLI path.
    fn all_commands() -> Vec<Command> {
        vec![
            Command::Guided,
            Command::Init,
            Command::WorkDir,
            Command::Manifest,
            Command::Tools,
            Command::InstallTools,
            Command::Repos,
            Command::DiscoverRepos,
            Command::CloneRepos {
                repos: vec!["acme/api".to_owned()],
                options: CloneOptions::default(),
            },
            Command::AddTarget {
                kind: TargetKind::Repo,
                spec: "acme/api".to_owned(),
            },
            Command::AddTarget {
                kind: TargetKind::Board,
                spec: "jira:ACME".to_owned(),
            },
            Command::ListTargets,
            Command::RemoveTarget {
                spec: "acme/api".to_owned(),
            },
            Command::Run(RunOptions::default()),
            Command::Package { destination: None },
            Command::Audit(ChainOptions::default()),
            Command::Distribute(DistributeOptions::default()),
            Command::Rerender(RerenderOptions::default()),
        ]
    }

    /// #5824: the chain must not be a way to reach the expensive direction by
    /// accident, and its two knobs must mean what the same flags mean on the
    /// verbs it chains.
    #[test]
    fn the_one_shot_audit_resumes_by_default() {
        let plain = Cli::try_parse_from(["taudit", "audit"]).expect("audit parses");
        assert_eq!(plain.to_command(), Command::Audit(ChainOptions::default()));

        let asked = Cli::try_parse_from(["taudit", "audit", "--fresh", "--out", "/tmp/p.zip"])
            .expect("both flags parse");
        assert_eq!(
            asked.to_command(),
            Command::Audit(ChainOptions {
                fresh: true,
                destination: Some(PathBuf::from("/tmp/p.zip")),
            })
        );
    }

    /// #6080: the recipient reads two things off this — the files to open, and
    /// how this copy differs from the one they were sent. Both are asserted,
    /// because a re-render that looks like a clean reproduction of a report it
    /// cannot fully reproduce is the fail-open shape the verb must not have.
    #[test]
    fn a_re_render_names_the_files_and_what_did_not_reproduce() {
        use crate::rerender::{RenderResult, RenderedReport, RerenderReport};

        let outcome = Outcome::Rerendered(RerenderReport {
            source: PathBuf::from("/pkg"),
            output: PathBuf::from("/pkg/rerendered"),
            review: PathBuf::from("/usr/local/bin/trusty-review"),
            review_source: crate::rerender::ReviewSource::Engagement,
            review_version: Some("0.20.0".to_owned()),
            reports: vec![
                RenderedReport {
                    manifest: PathBuf::from("/pkg/reports/01-acme-api/manifest.toml"),
                    name: "01-acme-api".to_owned(),
                    output: PathBuf::from("/pkg/rerendered/01-acme-api"),
                    log: PathBuf::from("/pkg/rerendered/01-acme-api.log"),
                    artifacts: vec![PathBuf::from("/pkg/rerendered/01-acme-api/report.md")],
                    gaps: vec!["no checkout for acme/api at /work/repos/acme-api".to_owned()],
                    duration_ms: None,
                    result: RenderResult::Succeeded,
                },
                RenderedReport {
                    manifest: PathBuf::from("/pkg/reports/02-acme-web/manifest.toml"),
                    name: "02-acme-web".to_owned(),
                    output: PathBuf::from("/pkg/rerendered/02-acme-web"),
                    log: PathBuf::from("/pkg/rerendered/02-acme-web.log"),
                    artifacts: Vec::new(),
                    gaps: Vec::new(),
                    duration_ms: None,
                    result: RenderResult::Failed {
                        reason: "exited with code 3".to_owned(),
                    },
                },
            ],
        });

        let rendered = render(&outcome);
        assert!(
            rendered.contains("/pkg/rerendered/01-acme-api/report.md"),
            "the file to open must be named: {rendered}"
        );
        assert!(
            rendered.contains("no checkout for acme/api"),
            "a dimension that did not reproduce must be stated: {rendered}"
        );
        assert!(rendered.contains("PARTIAL"), "{rendered}");
        assert!(
            rendered.contains("written by a model"),
            "the narrative's non-reproducibility must be stated: {rendered}"
        );
        assert_eq!(
            exit_code(&outcome),
            crate::session::EXIT_PARTIAL,
            "a re-render that could not regenerate every report must not exit 0"
        );
    }

    /// One re-render report driving `render`, with the renderer's provenance
    /// named. Everything else is fixed — only the two #6080 fields vary.
    fn re_render_resolved_from(
        source: crate::rerender::ReviewSource,
        version: Option<&str>,
    ) -> Outcome {
        use crate::rerender::{RenderResult, RenderedReport, RerenderReport};

        Outcome::Rerendered(RerenderReport {
            source: PathBuf::from("/pkg"),
            output: PathBuf::from("/pkg/rerendered"),
            review: PathBuf::from("/usr/local/bin/trusty-review"),
            review_source: source,
            review_version: version.map(str::to_owned),
            reports: vec![RenderedReport {
                manifest: PathBuf::from("/pkg/reports/01-acme-api/manifest.toml"),
                name: "01-acme-api".to_owned(),
                output: PathBuf::from("/pkg/rerendered/01-acme-api"),
                log: PathBuf::from("/pkg/rerendered/01-acme-api.log"),
                artifacts: vec![PathBuf::from("/pkg/rerendered/01-acme-api/report.md")],
                gaps: Vec::new(),
                duration_ms: None,
                result: RenderResult::Succeeded,
            }],
        })
    }

    /// 🔴 #6080: a renderer nobody chose is disclosed at the top of the run,
    /// with the path and the version it answered. The live failure was silent —
    /// a `trusty-review` two minor versions behind the engagement's pin rendered
    /// the whole audit and exited 0, and the only record was the versions table
    /// in an `index.md` the operator opens afterwards.
    #[test]
    fn a_path_resolved_renderer_is_disclosed_with_its_version() {
        let text = render(&re_render_resolved_from(
            crate::rerender::ReviewSource::Path,
            Some("0.18.0"),
        ));

        assert!(text.contains("/usr/local/bin/trusty-review"), "{text}");
        assert!(text.contains("0.18.0"), "the version must be named: {text}");
        assert!(text.contains("PATH"), "{text}");
        assert!(
            text.contains("--review-bin"),
            "the remedy must be named: {text}"
        );

        // A binary that will not answer says so rather than rendering a blank.
        let silent = render(&re_render_resolved_from(
            crate::rerender::ReviewSource::Path,
            None,
        ));
        assert!(silent.contains("did not answer"), "{silent}");
    }

    /// The line means something only if its absence does: a renderer somebody
    /// chose — a flag, or the engagement's own `tools/` copy — is not disclosed.
    #[test]
    fn a_chosen_renderer_is_not_disclosed() {
        for source in [
            crate::rerender::ReviewSource::Explicit,
            crate::rerender::ReviewSource::Engagement,
        ] {
            let text = render(&re_render_resolved_from(source, Some("0.20.0")));
            assert!(!text.contains("NOTE:"), "{source:?} was disclosed: {text}");
            assert!(!text.contains("resolved on PATH"), "{source:?}: {text}");
        }
    }

    #[test]
    fn every_command_variant_has_a_cli_invocation() {
        for command in all_commands() {
            let argv = argv_for(&command);
            let cli =
                Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("{argv:?} must parse: {e}"));
            assert_eq!(
                cli.to_command(),
                command,
                "{argv:?} did not route to {command:?}"
            );
        }
    }

    /// #5494: resume is what a bare `run` does, and re-collection is the thing
    /// asked for by name — an operator who types `run` after a crash must get
    /// the cheap direction, and the expensive one must be impossible to reach
    /// by accident.
    #[test]
    fn run_resumes_by_default_and_re_collects_only_when_asked() {
        let plain = Cli::try_parse_from(["taudit", "run"]).expect("run parses");
        assert_eq!(
            plain.to_command(),
            Command::Run(RunOptions { fresh: false }),
            "a bare `run` must resume"
        );

        let fresh = Cli::try_parse_from(["taudit", "run", "--fresh"]).expect("--fresh parses");
        assert_eq!(fresh.to_command(), Command::Run(RunOptions { fresh: true }));
    }

    #[test]
    fn a_bare_invocation_enters_the_guided_flow() {
        let cli = Cli::try_parse_from(["taudit"]).expect("bare invocation parses");
        assert_eq!(cli.to_command(), Command::Guided);
    }

    /// #5483: the three new knobs reach `DistributeOptions`, and a bare
    /// `distribute` still means what it meant — no key mode, no preset, no
    /// declared repositories.
    #[test]
    fn the_cast_template_flag_selects_the_cast_preset() {
        let bare = Cli::try_parse_from(["taudit", "distribute"]).expect("parses");
        assert_eq!(
            bare.to_command(),
            Command::Distribute(DistributeOptions::default())
        );

        let cli = Cli::try_parse_from([
            "taudit",
            "distribute",
            "--prompt-for-key",
            "--template",
            "cast",
            "--repos",
            "/tmp/repos.txt",
        ])
        .expect("parses");
        let Command::Distribute(options) = cli.to_command() else {
            panic!("expected Distribute");
        };
        assert!(options.prompt_for_key);
        assert_eq!(options.report_preset, ReportPreset::Cast);
        assert_eq!(options.repos, Some(PathBuf::from("/tmp/repos.txt")));

        // `--template default` is the explicit spelling of the default, not a
        // second preset.
        let cli =
            Cli::try_parse_from(["taudit", "distribute", "--template", "default"]).expect("parses");
        let Command::Distribute(options) = cli.to_command() else {
            panic!("expected Distribute");
        };
        assert_eq!(options.report_preset, ReportPreset::Inherit);

        assert!(
            Cli::try_parse_from(["taudit", "distribute", "--template", "cst"]).is_err(),
            "an unknown template must be a usage error, not a silent default"
        );
    }

    #[test]
    fn global_options_are_accepted_after_a_subcommand() {
        let cli = Cli::try_parse_from(["taudit", "tools", "--work-dir", "/tmp/w"])
            .expect("global options parse after the verb");
        assert_eq!(cli.work_dir, Some(PathBuf::from("/tmp/w")));
        assert_eq!(cli.to_command(), Command::Tools);
    }

    #[test]
    fn the_parser_definition_is_valid() {
        // clap's own debug assertions catch conflicting arg definitions.
        <Cli as clap::CommandFactory>::command().debug_assert();
    }

    #[test]
    fn rendering_names_the_root_and_every_area() {
        let work = WorkDir::new("/engagement/work");
        let outcome = Outcome::WorkDir(WorkDirReport {
            root: work.root().to_path_buf(),
            areas: work.layout(),
        });
        let text = render(&outcome);
        assert!(text.contains("/engagement/work"));
        for (area, _) in work.layout() {
            assert!(
                text.contains(area.dir_name()),
                "{area:?} missing from the rendered layout"
            );
        }
        assert!(text.contains("delete this directory"));
        // #5915: the approval is recorded outside the root, so the layout must
        // name what removes it rather than implying deletion covers everything.
        assert!(text.contains("trusty-search index remove"), "{text}");
    }

    /// #5885: `repos` reads the manifest a completed sweep writes, so it is
    /// empty however many targets are registered. It must point at `targets`,
    /// not send the operator back to register what they already registered.
    #[test]
    fn rendering_an_empty_repo_list_says_what_to_do() {
        let text = render(&Outcome::Repos(Vec::new()));
        assert!(text.contains("trusty-audit targets"), "{text}");
        assert!(
            !text.contains("configured yet"),
            "a successful registration must not read as a failure: {text}"
        );
    }

    #[tokio::test]
    async fn rendering_a_guided_status_states_the_next_step() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = Session::new(WorkDir::new(tmp.path().join("work")));
        let outcome = session.execute(Command::Guided).await.expect("runs");
        let text = render(&outcome);
        // #5884: `repos` LISTS what is already registered; nothing is
        // registered yet in this state, so the next step names `add`.
        assert!(
            text.contains(
                "Next: register the repositories and boards to audit (`trusty-audit add`)"
            ),
            "{text}"
        );
    }

    /// The guided flow coaches breadth at the registration step, naming schema
    /// repositories rather than asking for "relevant" ones.
    #[tokio::test]
    async fn the_guided_flow_coaches_registration_breadth() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = Session::new(WorkDir::new(tmp.path().join("work")));
        let outcome = session.execute(Command::Guided).await.expect("runs");
        let text = render(&outcome);
        assert!(text.contains("Coverage: "), "{text}");
        assert!(text.contains("database schema or migrations"), "{text}");
        assert!(text.contains("ticketing board"), "{text}");
        // #5885: the paragraph ends a sentence. A trailing colon reads as
        // "…and here it comes", with nothing after it.
        assert!(
            !text.trim_end().ends_with(':'),
            "the card ends with a stray colon: {text}"
        );
        // #5884: the "Next:" line must name the registering path, not the
        // listing one — `repos` lists what `add` already registered.
        assert!(text.contains("Next: register"), "{text}");
        assert!(text.contains("`trusty-audit add`"), "{text}");
    }

    /// The `add` help is where an operator reading `--help` decides what counts
    /// as a target, so the breadth nudge has to survive there too.
    #[test]
    fn the_add_help_names_schema_repositories() {
        let mut command = <Cli as clap::CommandFactory>::command();
        let add = command
            .find_subcommand_mut("add")
            .expect("`add` is a subcommand")
            .render_long_help()
            .to_string();
        assert!(add.contains("database schema or migrations"), "{add}");
        assert!(add.contains("ticketing board"), "{add}");
    }

    /// A binary this client did not place must never be shown carrying a
    /// version — the recipient has to be able to see the difference.
    #[test]
    fn an_unverified_binary_renders_differently_from_a_verified_one() {
        let statuses = vec![
            ToolStatus {
                tool: RequiredTool::Tga,
                path: PathBuf::from("/work/tools/tga"),
                installed: true,
                version: Some("2.9.4".to_owned()),
            },
            ToolStatus {
                tool: RequiredTool::TrustyReview,
                path: PathBuf::from("/work/tools/trusty-review"),
                installed: true,
                version: None,
            },
        ];
        let text = render(&Outcome::Tools(statuses));
        assert!(text.contains("2.9.4"), "{text}");
        assert!(text.contains("UNVERIFIED"), "{text}");
    }

    #[test]
    fn rendering_an_install_names_every_version() {
        let installed = vec![InstalledTool {
            crate_name: "tga".to_owned(),
            version: "2.9.4".to_owned(),
            binary: PathBuf::from("/work/tools/tga"),
        }];
        let text = render(&Outcome::Installed(installed));
        assert!(text.contains("1 tool"), "{text}");
        assert!(text.contains("2.9.4"), "{text}");
    }

    #[test]
    fn the_config_path_is_overridable_from_the_command_line() {
        let cli = Cli::try_parse_from(["taudit", "install", "--config", "/pkg/engagement.toml"])
            .expect("the config flag parses");
        assert_eq!(cli.config, Some(PathBuf::from("/pkg/engagement.toml")));
        assert_eq!(cli.to_command(), Command::InstallTools);
    }

    /// Discovery reaching nothing must not read like the manifest-backed
    /// `repos` list, which says "run the guided flow" — here an empty list is
    /// a fact about the credential, not a state to advance out of (#5487).
    #[test]
    fn rendering_an_empty_discovery_does_not_suggest_a_next_step() {
        let text = render(&Outcome::Discovered(Vec::new()));
        assert!(text.contains("can reach no repositories"), "{text}");
        assert!(!text.contains("guided flow"), "{text}");
    }

    #[test]
    fn rendering_a_discovery_marks_private_and_archived() {
        let repos = vec![
            DiscoveredRepo {
                name_with_owner: "acme/api".to_owned(),
                name: "api".to_owned(),
                is_private: true,
                is_archived: false,
                url: String::new(),
            },
            DiscoveredRepo {
                name_with_owner: "acme/old".to_owned(),
                name: "old".to_owned(),
                is_private: false,
                is_archived: true,
                url: String::new(),
            },
        ];
        let text = render(&Outcome::Discovered(repos));
        assert!(text.contains("2 repositories"), "{text}");
        assert!(text.contains("acme/api"), "{text}");
        assert!(text.contains("private"), "{text}");
        assert!(text.contains("archived"), "{text}");
    }

    /// #5916: the verb has no depth knob to get wrong. `--full` used to exist
    /// and default to off, so the ordinary invocation was the truncating one.
    #[test]
    fn the_clone_verb_defaults_to_a_bounded_clone_and_takes_no_depth_flag() {
        let cli = Cli::try_parse_from(["taudit", "clone", "acme/api", "acme/web"])
            .expect("the clone verb parses");
        let Command::CloneRepos { repos, options } = cli.to_command() else {
            panic!("clone must route to CloneRepos");
        };
        assert_eq!(repos, vec!["acme/api", "acme/web"]);
        assert_eq!(
            options.budget_bytes,
            Some(crate::clone::DEFAULT_BUDGET_BYTES)
        );
        for gone in ["--full", "--depth", "--shallow"] {
            assert!(
                Cli::try_parse_from(["taudit", "clone", "acme/api", gone]).is_err(),
                "{gone} must not be accepted — a truncated clone empties the audit"
            );
        }
    }

    /// The ceiling comes off only when the recipient asks for it in words.
    #[test]
    fn only_an_explicit_zero_budget_removes_the_ceiling() {
        let unbounded = Cli::try_parse_from(["taudit", "clone", "acme/api", "--budget-gb", "0"])
            .expect("parses")
            .to_command();
        let Command::CloneRepos { options, .. } = unbounded else {
            panic!("clone must route to CloneRepos");
        };
        assert_eq!(options.budget_bytes, None);

        let bounded = Cli::try_parse_from(["taudit", "clone", "acme/api", "--budget-gb", "2"])
            .expect("parses")
            .to_command();
        let Command::CloneRepos { options, .. } = bounded else {
            panic!("clone must route to CloneRepos");
        };
        assert_eq!(options.budget_bytes, Some(2 * 1024 * 1024 * 1024));
    }

    #[test]
    fn cloning_nothing_is_a_parse_error_rather_than_a_no_op_run() {
        assert!(Cli::try_parse_from(["taudit", "clone"]).is_err());
    }

    /// A repository that is not in the audit must read as excluded.
    #[test]
    fn rendering_a_clone_report_names_every_exclusion() {
        let report = CloneReport {
            repos: vec![
                ClonedRepo {
                    name_with_owner: "acme/api".to_owned(),
                    path: PathBuf::from("/w/repos/acme/api"),
                    state: CloneState::Cloned,
                    bytes: 2048,
                    bytes_complete: true,
                },
                ClonedRepo {
                    name_with_owner: "acme/web".to_owned(),
                    path: PathBuf::from("/w/repos/acme/web"),
                    state: CloneState::Failed("no such repository".to_owned()),
                    bytes: 0,
                    bytes_complete: true,
                },
            ],
            total_bytes: 2048,
            total_bytes_complete: true,
            gaps: vec!["acme/web was not audited — the clone failed".to_owned()],
        };
        let text = render(&Outcome::Cloned(report));
        assert!(text.contains("1 repository on disk"), "{text}");
        assert!(text.contains("2.0 KiB"), "{text}");
        assert!(text.contains("FAILED"), "{text}");
        assert!(text.contains("Gap: acme/web"), "{text}");
    }

    /// A budget so large it overflows must clamp, never panic or wrap to zero.
    #[test]
    fn an_enormous_budget_saturates_rather_than_wrapping() {
        let cli =
            Cli::try_parse_from(["taudit", "clone", "acme/api", "--budget-gb", "17179869184"])
                .expect("parses")
                .to_command();
        let Command::CloneRepos { options, .. } = cli else {
            panic!("clone must route to CloneRepos");
        };
        assert_eq!(options.budget_bytes, Some(u64::MAX));
    }

    /// A repository that produced no checkout must read as excluded, and the
    /// run must not claim a byte figure a failed walk produced.
    #[test]
    fn rendering_reports_an_empty_clone_and_an_incomplete_measurement() {
        let report = CloneReport {
            repos: vec![
                ClonedRepo {
                    name_with_owner: "acme/api".to_owned(),
                    path: PathBuf::from("/w/repos/acme/api"),
                    state: CloneState::Cloned,
                    bytes: 1024,
                    bytes_complete: false,
                },
                ClonedRepo {
                    name_with_owner: "acme/blank".to_owned(),
                    path: PathBuf::from("/w/repos/acme/blank"),
                    state: CloneState::Empty("the repository has no commits".to_owned()),
                    bytes: 0,
                    bytes_complete: true,
                },
            ],
            total_bytes: 1024,
            total_bytes_complete: false,
            gaps: vec!["acme/blank was not audited — nothing was cloned".to_owned()],
        };
        let text = render(&Outcome::Cloned(report));
        assert!(text.contains("NOTHING CLONED"), "{text}");
        assert!(text.contains("at least 1.0 KiB"), "{text}");
        assert!(text.contains("Gap: acme/blank"), "{text}");
    }

    /// A partial acquisition must not exit 0 — `taudit clone … && taudit run`
    /// would otherwise proceed against an incomplete set.
    #[test]
    fn a_run_with_gaps_exits_non_zero() {
        let clean = CloneReport {
            repos: Vec::new(),
            total_bytes: 0,
            total_bytes_complete: true,
            gaps: Vec::new(),
        };
        assert_eq!(Outcome::Cloned(clean).exit_code(), 0);

        let gapped = CloneReport {
            repos: Vec::new(),
            total_bytes: 0,
            total_bytes_complete: true,
            gaps: vec!["acme/web was not audited".to_owned()],
        };
        assert_eq!(Outcome::Cloned(gapped).exit_code(), EXIT_INCOMPLETE);
        assert_eq!(Outcome::Repos(Vec::new()).exit_code(), 0);
    }

    #[test]
    fn counts_are_singular_when_there_is_one() {
        assert_eq!(count_of(1, "repository", "repositories"), "1 repository");
        assert_eq!(count_of(0, "repository", "repositories"), "0 repositories");
        assert_eq!(count_of(3, "repository", "repositories"), "3 repositories");
    }

    /// The fail-open guard: a sweep that partly failed is an `Ok` outcome, and
    /// it must still not leave the process reporting success.
    #[test]
    fn a_partial_sweep_does_not_exit_zero() {
        use crate::run::{RepoRun, RunReport, SelectedRepo};

        let run = |result| RepoRun {
            repo: SelectedRepo {
                name: "acme-api".to_owned(),
                path: PathBuf::from("repos/acme-api"),
                github_slug: None,
                github_absent: None,
            },
            output: PathBuf::from("/work/out/00-acme-api"),
            log: PathBuf::from("/work/logs/00-acme-api.log"),
            gaps: vec!["Collection stage `jira sync` did not complete.".to_owned()],
            resumed: false,
            duration_ms: None,
            result,
        };
        let ok = run(RepoResult::Succeeded);
        let bad = run(RepoResult::Failed {
            reason: "`tga audit` exited with code 3; see /work/logs/acme-api.log".to_owned(),
        });

        let clean = Outcome::Run(RunReport::of(vec![ok.clone()]));
        assert_eq!(exit_code(&clean), 0);
        assert!(
            render(&clean).contains("Audited 1 repository"),
            "{}",
            render(&clean)
        );

        let partial = Outcome::Run(RunReport::of(vec![ok, bad.clone()]));
        assert_eq!(exit_code(&partial), 1);
        let text = render(&partial);
        assert!(text.contains("PARTIAL"), "{text}");
        assert!(text.contains("exited with code 3"), "{text}");
        // A stated gap is not a failure, and not invisible either.
        assert!(text.contains("gap   Collection stage"), "{text}");

        let total = Outcome::Run(RunReport::of(vec![bad]));
        assert_eq!(exit_code(&total), 1);
        assert!(
            render(&total).contains("no repository was audited"),
            "{}",
            render(&total)
        );
    }

    /// #5982: every repository succeeded and a registered board was skipped, so
    /// this is `AllSucceeded` and still not a whole engagement. Before the
    /// `board_gaps` wiring, `taudit run` over a legacy `linear:<team-id>` exited
    /// 0 and rendered nothing at all about the board — the silent-empty shape
    /// `boards::resolve`'s gap lines exist to replace.
    ///
    /// Against `9ee9cc386` the `RunReport::stating` constructor does not exist.
    #[test]
    fn a_sweep_that_skipped_a_board_does_not_exit_zero() {
        use crate::run::{RepoRun, RunReport, SelectedRepo};

        let audited = RepoRun {
            repo: SelectedRepo {
                name: "acme-api".to_owned(),
                path: PathBuf::from("repos/acme-api"),
                github_slug: None,
                github_absent: None,
            },
            output: PathBuf::from("/work/out/00-acme-api"),
            log: PathBuf::from("/work/logs/00-acme-api.log"),
            gaps: Vec::new(),
            resumed: false,
            duration_ms: None,
            result: RepoResult::Succeeded,
        };
        let gap = "linear:a1b2c3d4 was not audited — re-register it (#5982)";
        let outcome = Outcome::Run(RunReport::of(vec![audited]).stating(vec![gap.to_owned()]));

        let text = render(&outcome);
        assert!(text.contains(&format!("Not audited: {gap}")), "{text}");
        // The board is not a repository that failed: the verdict is unchanged.
        assert!(text.contains("Audited 1 repository"), "{text}");
        assert_eq!(exit_code(&outcome), EXIT_INCOMPLETE);
    }

    /// #5494: silent skipping is the defect. A resumed sweep finishes in
    /// seconds, and without a word about it that reads as a run that did
    /// nothing — so each carried-over repository is marked in its own line and
    /// the summary separates what was carried over from what ran now.
    #[test]
    fn a_resumed_sweep_says_what_it_carried_over_and_what_it_audited() {
        use crate::run::{RepoRun, RunReport, SelectedRepo};

        let repo = |name: &str, index: usize, resumed| RepoRun {
            repo: SelectedRepo {
                name: name.to_owned(),
                path: PathBuf::from(format!("repos/{name}")),
                github_slug: None,
                github_absent: None,
            },
            output: PathBuf::from(format!("/work/out/0{index}-{name}")),
            log: PathBuf::from(format!("/work/logs/0{index}-{name}.log")),
            gaps: Vec::new(),
            resumed,
            duration_ms: None,
            result: RepoResult::Succeeded,
        };
        let outcome = Outcome::Run(RunReport::of(vec![
            repo("acme-api", 0, true),
            repo("acme-web", 1, false),
        ]));

        let text = render(&outcome);
        assert!(text.contains("resumed acme-api"), "{text}");
        assert!(text.contains("ok      acme-web"), "{text}");
        assert!(
            text.contains("Resumed 1 repository from an earlier run; 1 audited now."),
            "{text}"
        );
        assert_eq!(exit_code(&outcome), 0);

        // A sweep with nothing to carry over says nothing about resuming.
        let plain = Outcome::Run(RunReport::of(vec![repo("acme-web", 1, false)]));
        assert!(!render(&plain).contains("Resumed"), "{}", render(&plain));
    }

    /// #5499 closure condition 3: the path has to be findable in the output,
    /// and the recipient has to be told the file is theirs to inspect.
    #[test]
    fn rendering_a_package_names_the_file_to_send_and_its_members() {
        let package = ReturnPackage {
            path: PathBuf::from("/work/audit-return-package.zip"),
            files: vec![
                PackagedFile {
                    entry: "README.md".to_owned(),
                    source: None,
                    bytes: 900,
                },
                PackagedFile {
                    entry: "extract/00-acme-api.db".to_owned(),
                    source: Some(PathBuf::from("/work/extract/00-acme-api.db")),
                    bytes: 3 * 1024 * 1024,
                },
            ],
            total_bytes: 900 + 3 * 1024 * 1024,
            packaged_bytes: 1024 * 1024,
            excluded: Vec::new(),
        };
        let text = render(&Outcome::Package(package));
        assert!(text.contains("/work/audit-return-package.zip"), "{text}");
        assert!(text.contains("Send this file back"), "{text}");
        assert!(text.contains("unencrypted"), "{text}");
        assert!(text.contains("extract/00-acme-api.db"), "{text}");
        assert!(text.contains("3.0 MiB"), "{text}");
    }

    /// #5825: the auditor cannot tell which key a built package carries without
    /// reading a credential out of the zip, so the rendered text has to name the
    /// SOURCE. Both sources render, and neither renders the value.
    #[test]
    fn rendering_an_install_package_names_the_key_source() {
        let package = |key_from_environment: bool| InstallPackage {
            path: PathBuf::from("/home/auditor/duetto/audit/trusty-audit-install.zip"),
            files: vec![PackagedFile {
                entry: "taudit".to_owned(),
                source: None,
                bytes: 12 * 1024 * 1024,
            }],
            total_bytes: 12 * 1024 * 1024,
            packaged_bytes: 5 * 1024 * 1024,
            platform: "macos-aarch64".to_owned(),
            key_from_environment,
            prompts_for_key: false,
            declared_repos: 0,
            dropped_board_credentials: vec!["boards.jira".to_owned()],
        };

        let from_env = render(&Outcome::Distributed(package(true)));
        assert!(from_env.contains("Install package:"), "{from_env}");
        assert!(from_env.contains("macos-aarch64"), "{from_env}");
        assert!(from_env.contains("taudit"), "{from_env}");
        assert!(
            from_env.contains("credential: from OPENROUTER_API_KEY"),
            "{from_env}"
        );

        let from_template = render(&Outcome::Distributed(package(false)));
        assert!(
            from_template.contains("credential: from the template config"),
            "{from_template}"
        );
        // #5861: a board credential the template held and the package did not
        // ship is stated, so the auditor does not expect that board to collect.
        assert!(
            from_template.contains("not shipped: boards.jira"),
            "{from_template}"
        );
    }

    /// A package covering four of five repositories is still worth sending and
    /// is still not the whole engagement — `taudit package && mail …` must not
    /// read the first half of that sentence only.
    #[test]
    fn a_package_that_omits_a_repository_does_not_exit_zero() {
        let package = |excluded: Vec<String>| ReturnPackage {
            path: PathBuf::from("/work/audit-return-package.zip"),
            files: Vec::new(),
            total_bytes: 0,
            packaged_bytes: 0,
            excluded,
        };
        assert_eq!(Outcome::Package(package(Vec::new())).exit_code(), 0);

        let partial = Outcome::Package(package(vec![
            "acme-web is not in this package — `tga audit` exited with code 3".to_owned(),
        ]));
        assert_eq!(exit_code(&partial), EXIT_INCOMPLETE);
        assert!(
            render(&partial).contains("Not included: acme-web"),
            "{}",
            render(&partial)
        );
    }

    /// #5824: a chained run that printed only its last phase would hide the two
    /// places an engagement most often comes up short — a repository that could
    /// not be cloned, and a registered target the chain cannot audit at all.
    /// Both must survive into the text even when the package assembled fine.
    #[test]
    fn a_chained_run_names_every_phase() {
        use crate::chain::ChainReport;
        use crate::run::{RepoRun, RunReport, SelectedRepo};

        let report = ChainReport {
            installed: Some(vec![InstalledTool {
                crate_name: "tga".to_owned(),
                version: "2.9.4".to_owned(),
                binary: PathBuf::from("/work/tools/tga"),
            }]),
            acquired: Some(CloneReport {
                repos: vec![ClonedRepo {
                    name_with_owner: "acme/web".to_owned(),
                    path: PathBuf::from("/work/repos/acme/web"),
                    state: CloneState::Failed("no such repository".to_owned()),
                    bytes: 0,
                    bytes_complete: true,
                }],
                total_bytes: 0,
                total_bytes_complete: true,
                gaps: vec!["acme/web was not audited — the clone failed".to_owned()],
            }),
            run: RunReport::of(vec![RepoRun {
                repo: SelectedRepo {
                    name: "acme-api".to_owned(),
                    path: PathBuf::from("repos/acme-api"),
                    github_slug: None,
                    github_absent: None,
                },
                output: PathBuf::from("/work/out/00-acme-api"),
                log: PathBuf::from("/work/logs/00-acme-api.log"),
                gaps: Vec::new(),
                resumed: false,
                duration_ms: None,
                result: RepoResult::Succeeded,
            }]),
            package: ReturnPackage {
                path: PathBuf::from("/work/audit-return-package.zip"),
                files: Vec::new(),
                total_bytes: 0,
                packaged_bytes: 0,
                excluded: Vec::new(),
            },
            gaps: vec!["jira:ACME was not audited".to_owned()],
        };

        let text = render(&Outcome::Audit(report));
        assert!(text.contains("Installed 1 tool: tga 2.9.4"), "{text}");
        assert!(
            text.contains("acme/web"),
            "the clone phase is missing: {text}"
        );
        assert!(text.contains("ok      acme-api"), "{text}");
        assert!(text.contains("Not audited: jira:ACME"), "{text}");
        assert!(text.contains("Send this file back"), "{text}");
    }

    /// The save-dialog equivalent: the recipient names where the file lands.
    #[test]
    fn the_package_destination_is_choosable_from_the_command_line() {
        let cli =
            Cli::try_parse_from(["taudit", "package", "--out", "/Users/x/Desktop/return.zip"])
                .expect("the out flag parses");
        assert_eq!(
            cli.to_command(),
            Command::Package {
                destination: Some(PathBuf::from("/Users/x/Desktop/return.zip"))
            }
        );
    }

    fn repo_target(name: &str) -> Target {
        Target::Repo {
            name_with_owner: name.to_owned(),
        }
    }

    fn jira_target(key: &str) -> Target {
        Target::Board {
            provider: crate::registry::BoardProvider::Jira,
            key: key.to_owned(),
        }
    }

    /// The verb is what decides the kind, so a board spec typed after `repo`
    /// reaches the library as a repo request and is refused there (#5822).
    #[test]
    fn the_add_verb_carries_the_kind_the_operator_typed() {
        assert_eq!(
            Cli::try_parse_from(["taudit", "add", "board", "linear:ENG"])
                .expect("parses")
                .to_command(),
            Command::AddTarget {
                kind: TargetKind::Board,
                spec: "linear:ENG".to_owned(),
            }
        );
        assert_eq!(
            Cli::try_parse_from(["taudit", "add", "repo", "jira:ACME"])
                .expect("parses")
                .to_command(),
            Command::AddTarget {
                kind: TargetKind::Repo,
                spec: "jira:ACME".to_owned(),
            }
        );
    }

    #[test]
    fn adding_nothing_is_a_parse_error_rather_than_a_no_op_run() {
        assert!(Cli::try_parse_from(["taudit", "add"]).is_err());
        assert!(Cli::try_parse_from(["taudit", "add", "repo"]).is_err());
        assert!(Cli::try_parse_from(["taudit", "remove"]).is_err());
    }

    /// An idempotent re-add must not read like a fresh registration.
    #[test]
    fn rendering_distinguishes_a_new_registration_from_a_repeat() {
        let fresh = render(&Outcome::Registered(crate::registry::Registration {
            target: repo_target("acme/api"),
            already_registered: false,
        }));
        assert!(fresh.starts_with("registered:"), "{fresh}");
        assert!(fresh.contains("acme/api"), "{fresh}");

        let repeat = render(&Outcome::Registered(crate::registry::Registration {
            target: jira_target("ACME"),
            already_registered: true,
        }));
        assert!(repeat.starts_with("already registered:"), "{repeat}");
        assert!(repeat.contains("jira:ACME"), "{repeat}");
    }

    /// The registry and the sweep's selection file are two records, and the
    /// listing has to say so rather than leave one of them looking lost.
    #[test]
    fn rendering_a_target_list_names_both_records() {
        let empty = render(&Outcome::Targets(crate::registry::TargetList {
            targets: Vec::new(),
            legacy_selection: None,
        }));
        assert!(empty.contains("No targets registered yet"), "{empty}");

        let text = render(&Outcome::Targets(crate::registry::TargetList {
            targets: vec![repo_target("acme/api"), jira_target("ACME")],
            legacy_selection: Some((PathBuf::from("/w/state/selected-repos.toml"), 3)),
        }));
        assert!(text.contains("2 targets registered"), "{text}");
        assert!(text.contains("repo     acme/api"), "{text}");
        assert!(text.contains("board    jira:ACME"), "{text}");
        assert!(text.contains("3 repositories on disk"), "{text}");
        assert!(text.contains("selected-repos.toml"), "{text}");
    }

    #[test]
    fn removing_something_unregistered_says_nothing_changed() {
        let text = render(&Outcome::Removed(crate::registry::Removal {
            target: repo_target("acme/web"),
            was_registered: false,
        }));
        assert!(text.contains("was not registered"), "{text}");
        assert_eq!(
            render(&Outcome::Removed(crate::registry::Removal {
                target: repo_target("acme/web"),
                was_registered: true,
            })),
            "removed: acme/web\n"
        );
    }

    #[test]
    fn rendering_a_manifest_reports_its_metadata() {
        let manifest = AuditManifest::from_toml(
            "[report]\ntitle = \"Acme\"\nclient = \"Acme Inc\"\n",
            std::path::Path::new("manifest.toml"),
        )
        .expect("parses");
        let text = render(&Outcome::Manifest(manifest));
        assert!(text.contains("Acme Inc"), "{text}");
    }
}
