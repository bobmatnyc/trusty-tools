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
    /// Report which pinned tools are installed.
    Tools,
    /// List the repositories this engagement is configured to audit.
    Repos,
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
            Some(Verb::Repos) => Command::Repos,
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
                let mark = if status.installed { "ok " } else { "MISSING" };
                out.push_str(&format!(
                    "{mark:<8} {:<16} {}\n",
                    status.tool.binary_name(),
                    status.path.display()
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
            format!("install the pinned tools: {}", names.join(", "))
        }
        NextStep::ReadyForRun => {
            "everything checked here is in place; the audit run itself is later work".to_string()
        }
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    use crate::manifest::AuditManifest;
    use crate::session::{Session, WorkDirReport};
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
            Command::Repos => vec!["taudit", "repos"],
        }
    }

    const ALL_COMMANDS: [Command; 5] = [
        Command::Guided,
        Command::WorkDir,
        Command::Manifest,
        Command::Tools,
        Command::Repos,
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

    #[test]
    fn rendering_a_guided_status_states_the_next_step() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = Session::new(WorkDir::new(tmp.path().join("work")));
        let outcome = session.execute(Command::Guided).expect("runs");
        let text = render(&outcome);
        assert!(text.contains("Next: pick the repositories"), "{text}");
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
