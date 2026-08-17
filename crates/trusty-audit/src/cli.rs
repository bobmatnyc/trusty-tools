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

use clap::{Parser, Subcommand};

use crate::chain::ChainOptions;
use crate::clone::CloneOptions;
use crate::distribute::DistributeOptions;
use crate::registry::TargetKind;
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
    /// Working-directory root (default: ./trusty-audit-work, or $TRUSTY_AUDIT_WORKDIR).
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

/// One subcommand per capability.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Verb {
    /// Walk the pre-run steps: repository selection, then tooling.
    Guided,
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
        /// Fetch full history instead of only the tip commit.
        #[arg(long)]
        full: bool,
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
    /// A GitHub repository, checked with your `gh` credential.
    ///
    /// Application, database-schema and migration, infrastructure, shared-library
    /// and config repositories all belong here.
    Repo {
        /// The repository, as owner/name.
        #[arg(value_name = "OWNER/NAME")]
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
            Some(Verb::Clone {
                repos,
                full,
                budget_gb,
            }) => Command::CloneRepos {
                repos: repos.clone(),
                options: CloneOptions {
                    shallow: !full,
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
            Some(Verb::Distribute { out, binary }) => Command::Distribute(DistributeOptions {
                output_dir: out.clone(),
                binary: binary.clone(),
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
mod render;

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
        }
    }

    /// Every capability, as values rather than a `const` — #5215's
    /// `CloneRepos` carries data, so the list cannot be a const array. The
    /// exhaustive match in `argv_for` is what still fails to compile when a
    /// capability arrives without a CLI path.
    fn all_commands() -> Vec<Command> {
        vec![
            Command::Guided,
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
    }

    #[test]
    fn rendering_an_empty_repo_list_says_what_to_do() {
        let text = render(&Outcome::Repos(Vec::new()));
        assert!(text.contains("guided flow"), "{text}");
    }

    #[tokio::test]
    async fn rendering_a_guided_status_states_the_next_step() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = Session::new(WorkDir::new(tmp.path().join("work")));
        let outcome = session.execute(Command::Guided).await.expect("runs");
        let text = render(&outcome);
        assert!(text.contains("Next: pick the repositories"), "{text}");
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

    #[test]
    fn the_clone_verb_defaults_to_a_shallow_bounded_clone() {
        let cli = Cli::try_parse_from(["taudit", "clone", "acme/api", "acme/web"])
            .expect("the clone verb parses");
        let Command::CloneRepos { repos, options } = cli.to_command() else {
            panic!("clone must route to CloneRepos");
        };
        assert_eq!(repos, vec!["acme/api", "acme/web"]);
        assert!(options.shallow);
        assert_eq!(
            options.budget_bytes,
            Some(crate::clone::DEFAULT_BUDGET_BYTES)
        );
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

        let full =
            Cli::try_parse_from(["taudit", "clone", "acme/api", "--full", "--budget-gb", "2"])
                .expect("parses")
                .to_command();
        let Command::CloneRepos { options, .. } = full else {
            panic!("clone must route to CloneRepos");
        };
        assert!(!options.shallow);
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
            },
            output: PathBuf::from("/work/out/00-acme-api"),
            log: PathBuf::from("/work/logs/00-acme-api.log"),
            gaps: vec!["Collection stage `jira sync` did not complete.".to_owned()],
            resumed: false,
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
            },
            output: PathBuf::from(format!("/work/out/0{index}-{name}")),
            log: PathBuf::from(format!("/work/logs/0{index}-{name}.log")),
            gaps: Vec::new(),
            resumed,
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
                },
                output: PathBuf::from("/work/out/00-acme-api"),
                log: PathBuf::from("/work/logs/00-acme-api.log"),
                gaps: Vec::new(),
                resumed: false,
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
