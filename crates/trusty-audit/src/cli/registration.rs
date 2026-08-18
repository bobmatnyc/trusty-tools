//! Registering audit targets at the terminal, inside the launch itself (#5885).
//!
//! Why: launching 0.1.0 printed a status card ending "Next: register the
//! repositories and boards to audit (`trusty-audit add`)" and exited. The
//! operator had run the thing and was told to go type a different command. The
//! owner's requirement is one step, so the launch TAKES them into registration
//! instead of naming it.
//!
//! It lives HERE, beside [`crate::cli::credential`], for the same reason that
//! module gives: [`Session::execute`] is the one door every front end uses, the
//! Tauri shell (#5477) already calls it, and none of those front ends has a
//! terminal to prompt on. What crosses that boundary is a registered TARGET,
//! never a way to ask for one.
//!
//! What: [`guided_at_the_terminal`] is the whole launch path — the loop, then
//! the guided flow's own next step, then the sweep when the operator says so.
//! [`register_targets`] is the loop on its own. Both take a
//! [`Tty`](crate::cli::credential::Tty), so every branch is drivable from a test
//! binary that has no controlling terminal.
//!
//! Three properties this module exists to hold:
//!
//! - **A refused target does not end the session.** Validation reaches the
//!   network, and an operator who typed one name wrong, or reached for a board
//!   whose credential is not configured, keeps the prompt. Only the terminal
//!   itself failing stops the loop.
//! - **Every entry is persisted as it lands.** Each one goes through
//!   [`Command::AddTarget`], which validates and then writes under the
//!   registry's lock, so a crash mid-loop loses at most the entry being typed.
//! - **No terminal changes nothing.** `main` reaches this module only when
//!   `/dev/tty` opened; otherwise the launch prints the same status card it
//!   always did.
//!
//! Test: `super::registration_tests`.

use crate::cli::credential::Tty;
use crate::cli::render;
use crate::error::AuditError;
use crate::registry::{self, COVERAGE_COACHING, Target};
use crate::run::RunOptions;
use crate::session::{Command, NextStep, Outcome, Session};

/// The one prompt, naming both shapes it takes and how to stop.
///
/// It names no command. An operator inside this loop has no shell to type one
/// at, and the two verbs that would be reached for here — `add` to register and
/// `targets` to list — are what the loop is already doing for them.
const PROMPT: &str =
    "Add a repository (owner/repo), a board (jira:KEY or linear:TEAM), or press Enter when done: ";

/// Shown once, above the prompt, when the loop opens.
const OPENING: &str = "\nWhat should this audit cover?";

/// Shown when the operator finished without registering anything.
///
/// It points at `targets`, never at `repos`: `repos` reads the companion
/// manifest a completed sweep writes, so an operator sent there straight after
/// registering reads an empty list and concludes their registration failed.
const NOTHING_REGISTERED: &str = "Nothing registered — this audit covers nothing yet. Run `trusty-audit` again to add \
     targets, or `trusty-audit targets` to see what is registered.";

/// The one question that starts an hours-long unattended sweep.
const RUN_PROMPT: &str = "Everything is in place. Start the audit sweep now? It runs unattended and can take \
     hours. [Y/n]: ";

/// What a declined sweep leaves the operator holding.
const SWEEP_DECLINED: &str = "Not started. Run `trusty-audit run` when you are ready.";

/// Whether this invocation is eligible for the interactive launch.
///
/// Why: a named subcommand is what a script, a CI job or the E2E harness runs,
/// and none of them may be stopped for a prompt. Stating the rule as a function
/// makes "no subcommand ever prompts" a test rather than a reading of the
/// twenty-line binary shim.
/// What: only a bare launch — [`Command::Guided`]. Having a terminal is the
/// second and independent condition, which `main` checks with
/// [`DevTty::open`](crate::cli::credential::DevTty::open); both must hold.
/// Test: `super::registration_tests::only_a_bare_launch_is_interactive`.
pub fn is_interactive(command: &Command) -> bool {
    matches!(command, Command::Guided)
}

/// Drive the whole launch: register, advance the guided flow, then sweep.
///
/// Why: "one step" means one invocation. The operator supplies their key (which
/// `main` has already resolved by the time this is called), names what the audit
/// covers, and the flow carries on into tool installation and the sweep without
/// a second command.
/// What: the loop, then [`Command::Guided`] — which is where auto-install
/// happens, so the tools land in this same invocation — and then
/// [`Command::Run`] when the flow reached [`NextStep::ReadyForRun`] and the
/// operator agreed. The returned [`Outcome`] is whichever of the two ran last,
/// and `main` prints it to stdout; everything before it is shown on the terminal.
/// Test: `super::registration_tests::a_scripted_session_registers_and_advances_the_flow`,
/// `super::registration_tests::a_declined_sweep_returns_the_guided_card`.
///
/// # Errors
///
/// [`AuditError::RegistrationPromptFailed`] when the terminal fails, plus
/// whatever the guided flow or the sweep fail with. A refused TARGET is not an
/// error here — it is reported and the loop asks again.
pub async fn guided_at_the_terminal(
    session: &Session,
    tty: &mut dyn Tty,
) -> Result<Outcome, AuditError> {
    register_targets(session, tty).await?;

    let guided = session.execute(Command::Guided).await?;
    if !ready_for_run(&guided) {
        return Ok(guided);
    }

    // The card first: the operator decides whether to commit hours from what is
    // actually installed and registered, not from a bare question.
    say(tty, render(&guided).trim_end())?;
    if !confirmed(tty)? {
        say(tty, SWEEP_DECLINED)?;
        return Ok(guided);
    }
    session.execute(Command::Run(RunOptions::default())).await
}

/// Ask for targets until the operator presses Enter on an empty line.
///
/// Why: the loop is separate from [`guided_at_the_terminal`] so what it does —
/// coach once, ask repeatedly, never abort over a refusal — is assertable
/// without also running the guided flow behind it.
/// What: [`COVERAGE_COACHING`] once at the top, then one prompt per entry. An
/// entry that neither parses nor validates is reported and the prompt returns;
/// only a terminal failure propagates. End of input ends the loop the same way
/// an empty line does, so a `/dev/tty` that closes cannot spin.
/// Test: `super::registration_tests::the_coaching_prints_once_however_many_entries_land`,
/// `super::registration_tests::a_refused_entry_is_reported_and_the_loop_continues`.
///
/// # Errors
///
/// [`AuditError::RegistrationPromptFailed`] when the terminal cannot be read or
/// written.
pub async fn register_targets(
    session: &Session,
    tty: &mut dyn Tty,
) -> Result<Vec<Target>, AuditError> {
    say(tty, OPENING)?;
    // Once, at the top. Repeating it per prompt would bury the running list of
    // what is already registered under a paragraph the operator has read.
    say(tty, COVERAGE_COACHING)?;

    let mut registered = Vec::new();
    while let Some(entry) = tty
        .read_line(PROMPT)
        .map_err(prompt_failed)?
        .map(|line| line.trim().to_owned())
        .filter(|entry| !entry.is_empty())
    {
        match register_one(session, &entry).await {
            Ok(target) => {
                registered.push(target);
                // The existing `targets` rendering, so the running list reads
                // exactly as `trusty-audit targets` does.
                let listed = session.execute(Command::ListTargets).await?;
                say(tty, render(&listed).trim_end())?;
            }
            // #5885: a refusal is the ordinary case — a typo, or a board whose
            // credential is not configured yet. The message names which, and the
            // operator gets the prompt back rather than a dead session.
            Err(refusal) => say(tty, &format!("not registered: {refusal}"))?,
        }
    }

    if registered.is_empty() {
        say(tty, NOTHING_REGISTERED)?;
    }
    Ok(registered)
}

/// Register one typed entry through the same path `trusty-audit add` takes.
///
/// [`registry::parse`] decides which kind the spec is — the CLI knows because
/// the operator used a verb, and here they did not — and then the registration
/// goes through [`Session::execute`] so validation, the idempotent re-add and
/// the registry lock are the ones `add` already has.
async fn register_one(session: &Session, spec: &str) -> Result<Target, AuditError> {
    let target = registry::parse(None, spec)?;
    session
        .execute(Command::AddTarget {
            kind: target.kind(),
            spec: spec.to_owned(),
        })
        .await?;
    Ok(target)
}

/// Whether the guided flow ended one step short of the sweep.
fn ready_for_run(outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::Guided(status) if status.next == NextStep::ReadyForRun)
}

/// Ask once before committing the operator to an unattended sweep.
///
/// Enter means yes: the sweep is what they launched this for, and everything it
/// needs is already in place by the time this is asked. Anything starting with
/// `n` declines; end of input declines too, because a terminal that closed
/// cannot have agreed to anything.
fn confirmed(tty: &mut dyn Tty) -> Result<bool, AuditError> {
    let answer = tty.read_line(RUN_PROMPT).map_err(prompt_failed)?;
    Ok(answer.is_some_and(|line| !line.trim().to_ascii_lowercase().starts_with('n')))
}

/// Show one line, turning a terminal failure into this module's error.
fn say(tty: &mut dyn Tty, line: &str) -> Result<(), AuditError> {
    tty.say(line).map_err(prompt_failed)
}

/// Wrap a terminal I/O failure. Never carries what was typed.
fn prompt_failed(source: std::io::Error) -> AuditError {
    AuditError::RegistrationPromptFailed { source }
}

#[cfg(test)]
mod registration_tests {
    use super::*;
    use crate::registry::Registry;
    use crate::validate::RepoProbe;
    use crate::workdir::WorkDir;
    use std::collections::VecDeque;
    use std::io;
    use std::path::Path;

    /// A scripted terminal: answers come from a queue, and everything shown is
    /// recorded so a test can assert on it.
    ///
    /// The same shape as `crate::cli::credential::credential_tests::FakeTty`,
    /// with the queue feeding the VISIBLE read instead of the hidden one — a
    /// test binary has no controlling terminal, and every decision this module
    /// makes is above the terminal rather than inside it.
    struct FakeTty {
        answers: VecDeque<io::Result<String>>,
        prompts: Vec<String>,
        shown: Vec<String>,
    }

    impl FakeTty {
        fn new<I: IntoIterator<Item = &'static str>>(answers: I) -> Self {
            Self {
                answers: answers.into_iter().map(|a| Ok(a.to_owned())).collect(),
                prompts: Vec::new(),
                shown: Vec::new(),
            }
        }

        /// Everything displayed, prompts and guidance together.
        fn everything_displayed(&self) -> String {
            format!("{}\n{}", self.prompts.join("\n"), self.shown.join("\n"))
        }
    }

    impl Tty for FakeTty {
        fn read_hidden(&mut self, prompt: &str) -> io::Result<String> {
            self.prompts.push(prompt.to_owned());
            self.answers
                .pop_front()
                .unwrap_or_else(|| Err(io::Error::other("the script ran out of answers")))
        }

        fn read_line(&mut self, prompt: &str) -> io::Result<Option<String>> {
            self.prompts.push(prompt.to_owned());
            // An exhausted script is end of input, which is how a real terminal
            // that closed behaves — never an error, so a test that forgets a
            // trailing blank line still terminates.
            self.answers.pop_front().transpose()
        }

        fn say(&mut self, line: &str) -> io::Result<()> {
            self.shown.push(line.to_owned());
            Ok(())
        }
    }

    /// A session whose repository registrations succeed without `gh`, rooted in
    /// a throwaway working directory.
    fn accepting_session(dir: &Path) -> Session {
        Session::new(WorkDir::new(dir.join("work")))
            .with_repo_probe(RepoProbe::accepting())
            .with_auto_install(false)
    }

    fn registered_ids(session: &Session) -> Vec<String> {
        Registry::load(session.work_dir())
            .expect("the registry reads")
            .targets()
            .iter()
            .map(Target::id)
            .collect()
    }

    /// The owner's requirement, end to end: one repository, one entry that
    /// cannot be registered, one more repository, then Enter. Every valid target
    /// is on disk and the flow has left the registration state behind.
    #[tokio::test]
    async fn a_scripted_session_registers_and_advances_the_flow() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());
        let mut tty = FakeTty::new(["acme/api", "not a target", "acme/schema", ""]);

        let outcome = guided_at_the_terminal(&session, &mut tty)
            .await
            .expect("the loop runs");

        assert_eq!(registered_ids(&session), vec!["acme/api", "acme/schema"]);

        let Outcome::Guided(status) = outcome else {
            panic!("a launch that registers must end in the guided flow");
        };
        assert_ne!(
            status.next,
            NextStep::SelectRepositories,
            "the flow must not still be asking for what was just registered"
        );
    }

    /// A refusal keeps the session: the operator is told what went wrong and
    /// gets the prompt back, and the entries around it still land.
    #[tokio::test]
    async fn a_refused_entry_is_reported_and_the_loop_continues() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());
        // A board with no credential configured is the realistic refusal: it is
        // a well-formed spec that validation cannot reach.
        let mut tty = FakeTty::new(["acme/api", "jira:ACME", "acme/web", ""]);

        let registered = register_targets(&session, &mut tty)
            .await
            .expect("a refused target is not a failed session");

        let ids: Vec<String> = registered.iter().map(Target::id).collect();
        assert_eq!(ids, vec!["acme/api", "acme/web"]);
        assert_eq!(
            registered_ids(&session),
            ids,
            "a refusal must persist nothing"
        );

        let shown = tty.shown.join("\n");
        assert!(
            shown.contains("not registered:") && shown.contains("boards.jira"),
            "the refusal must name what to fix: {shown}"
        );
        assert_eq!(
            tty.prompts.len(),
            4,
            "the prompt must come back after a refusal: {:?}",
            tty.prompts
        );
    }

    /// The coaching is a paragraph. Printing it per entry would bury the running
    /// list of what is already registered underneath it.
    #[tokio::test]
    async fn the_coaching_prints_once_however_many_entries_land() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());
        let mut tty = FakeTty::new(["acme/api", "acme/web", "nope", "acme/iac", ""]);

        register_targets(&session, &mut tty)
            .await
            .expect("the loop runs");

        assert_eq!(
            tty.shown.iter().filter(|l| *l == COVERAGE_COACHING).count(),
            1,
            "the coaching was repeated: {:?}",
            tty.shown
        );
    }

    /// Each entry is written as it lands, so a session that dies mid-loop leaves
    /// everything before it on disk — the resumability discipline stated as a
    /// property rather than argued from where the write happens.
    #[tokio::test]
    async fn a_terminal_that_dies_keeps_what_was_already_registered() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());
        let mut tty = FakeTty::new(["acme/api", "acme/web"]);
        tty.answers
            .push_back(Err(io::Error::other("the terminal went away")));

        let err = register_targets(&session, &mut tty)
            .await
            .expect_err("a terminal that fails is an error");
        assert!(
            matches!(err, AuditError::RegistrationPromptFailed { .. }),
            "{err:?}"
        );

        assert_eq!(
            registered_ids(&session),
            vec!["acme/api", "acme/web"],
            "entries registered before the failure must survive it"
        );
    }

    /// The running list is the existing `targets` rendering, so what the loop
    /// shows and what `trusty-audit targets` prints cannot drift.
    #[tokio::test]
    async fn the_running_list_grows_as_entries_land() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());
        let mut tty = FakeTty::new(["acme/api", "acme/web", ""]);

        register_targets(&session, &mut tty)
            .await
            .expect("the loop runs");

        let lists: Vec<&String> = tty
            .shown
            .iter()
            .filter(|l| l.contains("registered:"))
            .collect();
        assert_eq!(lists.len(), 2, "one list per entry: {:?}", tty.shown);
        assert!(lists[0].contains("1 target registered:"), "{}", lists[0]);
        assert!(lists[1].contains("2 targets registered:"), "{}", lists[1]);
        assert!(lists[1].contains("acme/api") && lists[1].contains("acme/web"));
    }

    /// An operator who registers nothing must not be pointed at `repos`, which
    /// lists cloned checkouts — reading "No repositories configured yet" after a
    /// registration is what makes a successful `add` look like a failure.
    #[tokio::test]
    async fn finishing_with_nothing_registered_points_at_targets_not_repos() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());
        let mut tty = FakeTty::new([""]);

        let registered = register_targets(&session, &mut tty)
            .await
            .expect("an empty session is not a failure");

        assert!(registered.is_empty());
        let displayed = tty.everything_displayed();
        assert!(displayed.contains("trusty-audit targets"), "{displayed}");
        assert!(
            !displayed.contains("trusty-audit repos"),
            "the loop must never send an operator to `repos`: {displayed}"
        );
    }

    /// Declining the sweep hands back the guided card rather than starting hours
    /// of unattended work, and says what to run instead.
    #[tokio::test]
    async fn a_declined_sweep_returns_the_guided_card() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());
        // Every pinned tool present, so the flow reaches `ReadyForRun`.
        session
            .execute(Command::WorkDir)
            .await
            .expect("create the tree");
        for tool in crate::tools::RequiredTool::ALL {
            std::fs::write(tool.path_in(session.work_dir()), b"stub").expect("stub binary");
        }
        let mut tty = FakeTty::new(["acme/api", "", "n"]);

        let outcome = guided_at_the_terminal(&session, &mut tty)
            .await
            .expect("declining is not a failure");

        let Outcome::Guided(status) = outcome else {
            panic!("a declined sweep must hand back the guided card");
        };
        assert_eq!(status.next, NextStep::ReadyForRun);
        assert!(
            tty.shown.iter().any(|l| l == SWEEP_DECLINED),
            "{:?}",
            tty.shown
        );
    }

    /// End of input declines: a terminal that closed cannot have agreed to an
    /// hours-long unattended run.
    #[test]
    fn end_of_input_declines_the_sweep() {
        let mut tty = FakeTty::new([]);
        assert!(!confirmed(&mut tty).expect("end of input is not an error"));
    }

    /// Every named subcommand is something a script or the E2E harness runs, so
    /// none of them may be stopped for a prompt however the terminal looks.
    #[test]
    fn only_a_bare_launch_is_interactive() {
        use crate::chain::ChainOptions;
        use crate::clone::CloneOptions;
        use crate::distribute::DistributeOptions;
        use crate::registry::TargetKind;

        assert!(is_interactive(&Command::Guided));
        for command in [
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
            Command::ListTargets,
            Command::RemoveTarget {
                spec: "acme/api".to_owned(),
            },
            Command::Run(RunOptions::default()),
            Command::Package { destination: None },
            Command::Audit(ChainOptions::default()),
            Command::Distribute(DistributeOptions::default()),
        ] {
            assert!(
                !is_interactive(&command),
                "{command:?} must never prompt — a script runs it"
            );
        }
    }

    /// Enter means yes — the sweep is what the launch exists to reach, and by
    /// this point everything it needs is in place.
    #[test]
    fn an_empty_answer_starts_the_sweep() {
        for answer in ["", "  ", "y", "Yes"] {
            let mut tty = FakeTty::new([answer]);
            assert!(confirmed(&mut tty).expect("answers"), "{answer:?} declined");
        }
        for answer in ["n", "N", "no"] {
            let mut tty = FakeTty::new([answer]);
            assert!(!confirmed(&mut tty).expect("answers"), "{answer:?} agreed");
        }
    }
}
