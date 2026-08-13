//! The command-line front end — argument parsing and rendering.
//!
//! Why: this module lives in the LIBRARY, not in `main.rs`, for two reasons.
//! Every CLI invocation is then unit-testable without spawning a process, which
//! is what lets `cli_tests::every_command_variant_has_a_cli_invocation` stand as
//! a real gate rather than a comment. And `main.rs` stays a twenty-line shim, so
//! there is nowhere for logic to accumulate outside the API the Tauri shell will
//! call (#5502's permanent constraint).
//!
//! What: [`Cli`], the clap parser; [`Cli::command`], which maps a parse onto
//! exactly one [`Command`]; and [`render`], which turns an [`Outcome`] into the
//! text the CLI prints. Both mappings are exhaustive matches over the
//! capability enums, so adding a capability without a CLI path — or without a
//! way to display its result — is a compile error.
//! Test: `super::cli_tests`.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::clone::{CloneOptions, CloneState};
use crate::session::{Command, NextStep, Outcome};

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
        }
    }
}

/// Render an outcome as the text the CLI prints.
///
/// Why: rendering is the front end's job, so it lives here rather than on
/// [`Outcome`] — the Tauri shell will render the same values as a window. It
/// returns a `String` instead of printing so it is assertable in a test.
/// What: one arm per [`Outcome`] variant.
/// Test: `super::cli_tests::rendering_names_the_root_and_every_area`.
pub fn render(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Guided(status) => {
            let mut out = format!("Working directory: {}\n", status.root.display());
            match &status.manifest {
                Some(m) => out.push_str(&format!(
                    "Engagement: {} ({} configured)\n",
                    m.report.title,
                    count_of(m.repositories.len(), "repository", "repositories")
                )),
                None => out.push_str("Engagement: no manifest yet — nothing has run here\n"),
            }
            let installed = status.tools.iter().filter(|s| s.installed).count();
            out.push_str(&format!(
                "Tools: {installed}/{} installed\n",
                status.tools.len()
            ));
            out.push_str(&format!("Next: {}\n", describe_next(&status.next)));
            out
        }
        Outcome::WorkDir(report) => {
            let mut out = format!(
                "{}\n  (delete this directory to remove everything this client wrote)\n",
                report.root.display()
            );
            for (area, path) in &report.areas {
                out.push_str(&format!(
                    "  {:<8} {}\n           {}\n",
                    area.dir_name(),
                    path.display(),
                    area.description()
                ));
            }
            out
        }
        Outcome::Manifest(manifest) => {
            let mut out = format!("Title:  {}\n", manifest.report.title);
            if let Some(client) = &manifest.report.client {
                out.push_str(&format!("Client: {client}\n"));
            }
            if let Some(analyst) = &manifest.report.analyst {
                out.push_str(&format!("Analyst: {analyst}\n"));
            }
            out.push_str(&format!(
                "Repositories: {}\n",
                count_of(manifest.repositories.len(), "repository", "repositories")
            ));
            for gap in &manifest.report.gaps {
                out.push_str(&format!("Gap: {gap}\n"));
            }
            out
        }
        Outcome::Tools(statuses) => {
            let mut out = String::new();
            for status in statuses {
                // #5495: "installed" and "installed at a known version" are
                // different states, and the recipient has to be able to tell
                // them apart — a binary this client did not place is one it
                // cannot vouch for, so it is never shown carrying a version.
                let mark = match (status.installed, status.version.is_some()) {
                    (false, _) => "MISSING",
                    (true, true) => "ok",
                    (true, false) => "UNVERIFIED",
                };
                let version = status.version.as_deref().unwrap_or("-");
                out.push_str(&format!(
                    "{mark:<11} {:<16} {version:<12} {}\n",
                    status.tool.binary_name(),
                    status.path.display()
                ));
            }
            out
        }
        Outcome::Installed(installed) => {
            let mut out = format!(
                "Installed and verified {}:\n",
                count_of(installed.len(), "tool", "tools")
            );
            for tool in installed {
                out.push_str(&format!(
                    "  {:<16} {:<12} {}\n",
                    tool.crate_name,
                    tool.version,
                    tool.binary.display()
                ));
            }
            out
        }
        Outcome::Repos(repos) => {
            if repos.is_empty() {
                return "No repositories configured yet — run the guided flow to pick them.\n"
                    .to_string();
            }
            repos
                .iter()
                .map(|r| format!("{:<24} {}\n", r.name, r.path.display()))
                .collect()
        }
        Outcome::Discovered(repos) => {
            // #5487: an empty result here means the credential really can see
            // nothing — every failure is an error, never a short list — so the
            // wording says so rather than hedging.
            if repos.is_empty() {
                return "Your GitHub credential can reach no repositories.\n".to_string();
            }
            let mut out = format!(
                "{} your credential can reach:\n",
                count_of(repos.len(), "repository", "repositories")
            );
            for repo in repos {
                let mut marks = Vec::new();
                if repo.is_private {
                    marks.push("private");
                }
                if repo.is_archived {
                    marks.push("archived");
                }
                out.push_str(&format!(
                    "  {:<40} {}\n",
                    repo.name_with_owner,
                    marks.join(", ")
                ));
            }
            out
        }
        Outcome::Cloned(report) => {
            let mut out = String::new();
            for repo in &report.repos {
                // #5215: a repository that is NOT in the audit has to read as
                // excluded, never as a blank line the recipient scrolls past.
                let state = match &repo.state {
                    CloneState::Cloned => "cloned".to_string(),
                    CloneState::Reused => "already present".to_string(),
                    CloneState::Failed(why) => format!("FAILED — {why}"),
                    CloneState::Empty(why) => format!("NOTHING CLONED — {why}"),
                    CloneState::Skipped(why) => format!("SKIPPED — {why}"),
                };
                out.push_str(&format!("  {:<40} {state}\n", repo.name_with_owner));
            }
            out.push_str(&format!(
                "{} on disk, using {}{}.\n",
                count_of(
                    report.repos.iter().filter(|r| r.state.is_usable()).count(),
                    "repository",
                    "repositories"
                ),
                // #5215 review: a walk that hit something unreadable produces a
                // floor, and saying "using X" of a floor is a confident number
                // nothing measured.
                if report.total_bytes_complete {
                    ""
                } else {
                    "at least "
                },
                human_bytes(report.total_bytes)
            ));
            for gap in &report.gaps {
                out.push_str(&format!("Gap: {gap}\n"));
            }
            out
        }
    }
}

/// Bytes as something a recipient reads, not a raw count.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `"1 repository"` / `"3 repositories"` — this text goes to the recipient.
fn count_of(n: usize, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n == 1 { singular } else { plural })
}

fn describe_next(next: &NextStep) -> String {
    match next {
        NextStep::SelectRepositories => {
            "pick the repositories to audit (`trusty-audit repos`)".to_string()
        }
        NextStep::InstallTools(missing) => {
            let names: Vec<&str> = missing.iter().map(|t| t.binary_name()).collect();
            format!(
                "install the pinned tools (`trusty-audit install`): {}",
                names.join(", ")
            )
        }
        NextStep::ReadyForRun => {
            "everything checked here is in place; the audit run itself is later work".to_string()
        }
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    use crate::clone::{CloneReport, ClonedRepo};
    use crate::discover::DiscoveredRepo;
    use crate::manifest::AuditManifest;
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
        ]
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
