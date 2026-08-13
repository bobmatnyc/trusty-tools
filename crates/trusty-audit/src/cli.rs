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

use crate::run::{RepoResult, RunStatus};
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
    /// Run the audit sweep over the selected repositories.
    Run,
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
        match self.verb {
            None | Some(Verb::Guided) => Command::Guided,
            Some(Verb::Workdir) => Command::WorkDir,
            Some(Verb::Manifest) => Command::Manifest,
            Some(Verb::Tools) => Command::Tools,
            Some(Verb::Install) => Command::InstallTools,
            Some(Verb::Repos) => Command::Repos,
            Some(Verb::Run) => Command::Run,
        }
    }
}

/// The process exit status an outcome deserves.
///
/// Why: #5555's fail-open guard. `Session::execute` returns `Ok` for a sweep
/// that ran and partly failed, because the per-repo failures are data a front
/// end must render — but a shell, a CI job, or the operator reading `$?` must
/// not see that as success. Putting the mapping here rather than in `main.rs`
/// keeps it in the library, where it is unit-testable and where the Tauri shell
/// can consult the same rule.
/// What: 0 for every outcome except a [`Outcome::Run`] whose status is not
/// [`RunStatus::AllSucceeded`]; 1 for that.
/// Test: `super::cli_tests::a_partial_sweep_does_not_exit_zero`.
pub fn exit_code(outcome: &Outcome) -> i32 {
    match outcome {
        Outcome::Run(report) if report.status != RunStatus::AllSucceeded => 1,
        _ => 0,
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
        // #5555: a partial sweep must not read like a clean one. Each failure
        // is printed with its reason and its log path, and the verdict line
        // says which of the three states this run ended in.
        Outcome::Run(report) => {
            let mut out = String::new();
            for run in &report.repos {
                match &run.result {
                    RepoResult::Succeeded => out.push_str(&format!(
                        "ok      {:<24} {}\n",
                        run.repo.name,
                        run.output.display()
                    )),
                    RepoResult::Failed { reason } => {
                        out.push_str(&format!("FAILED  {:<24} {reason}\n", run.repo.name))
                    }
                }
                // #5555: a stated gap is a dimension the sweep could not assess
                // (DOC-67 §9). It does not fail the repository, and it must not
                // be invisible either — an audited repo with four gaps is not
                // the same result as one with none.
                for gap in &run.gaps {
                    out.push_str(&format!("  gap   {gap}\n"));
                }
            }
            let audited = report.repos.len() - report.failures().count();
            out.push_str(&match report.status {
                RunStatus::AllSucceeded => format!(
                    "\nAudited {}.\n",
                    count_of(audited, "repository", "repositories")
                ),
                RunStatus::Partial => format!(
                    "\nPARTIAL: {} audited, {} failed. The report covers only what succeeded.\n",
                    audited,
                    report.failures().count()
                ),
                RunStatus::AllFailed => format!(
                    "\nFAILED: no repository was audited ({} attempted).\n",
                    report.repos.len()
                ),
            });
            out
        }
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
        NextStep::ReadyForRun => "run the audit sweep (`trusty-audit run`)".to_string(),
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    use crate::manifest::AuditManifest;
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
            Command::Run => vec!["taudit", "run"],
        }
    }

    const ALL_COMMANDS: [Command; 7] = [
        Command::Guided,
        Command::WorkDir,
        Command::Manifest,
        Command::Tools,
        Command::InstallTools,
        Command::Repos,
        Command::Run,
    ];

    #[test]
    fn every_command_variant_has_a_cli_invocation() {
        for command in ALL_COMMANDS {
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
