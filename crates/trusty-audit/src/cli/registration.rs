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

use crate::chain::ChainOptions;
use crate::cli::credential::Tty;
use crate::cli::{Cli, render};
use crate::error::AuditError;
use crate::registry::{self, COVERAGE_COACHING, Target};
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
/// and none of them may be stopped for a prompt. It takes the PARSED CLI rather
/// than the [`Command`] it maps to because that mapping is not injective:
/// `Cli::to_command` collapses `None | Some(Verb::Guided)` onto
/// [`Command::Guided`], so by the time a `Command` exists, `trusty-audit` and
/// `trusty-audit guided` are the same value. Asking the `Command` therefore made
/// the named verb — a documented spelling, and the one #5502's reachability
/// table mandates — block on a prompt where it used to print the card and exit.
/// Under any pty (expect, `ssh -t`, tmux, a tty-allocating CI runner) that is an
/// unbounded hang.
/// What: no subcommand at all, and a command that may prompt. Having a terminal
/// is the third and independent condition, which `main` checks with
/// [`DevTty::open`](crate::cli::credential::DevTty::open); all must hold.
/// Test: `super::registration_tests::the_named_guided_verb_is_not_interactive`,
/// `super::registration_tests::only_a_bare_launch_is_interactive`.
pub fn is_interactive(cli: &Cli) -> bool {
    cli.verb.is_none() && may_prompt(&cli.to_command())
}

/// Which capability may ever be reached interactively.
///
/// The secondary guard: [`is_interactive`]'s first condition already excludes
/// every named verb, and this keeps the answer assertable over the CAPABILITY
/// set too — so a future `Command` that a bare launch could route to has to be
/// named here before it can prompt.
/// Test: `super::registration_tests::only_a_bare_launch_is_interactive`.
fn may_prompt(command: &Command) -> bool {
    matches!(command, Command::Guided)
}

/// What the launch decided at the terminal.
///
/// Why: the sweep needs an inference credential and the status card does not,
/// so `main` cannot resolve one before knowing which of the two happened. This
/// type is that answer, which is what lets a launch that only wants to see its
/// own state never be stopped for a key (#5896 review).
/// What: either the outcome to print, or the operator's agreement to the sweep —
/// which `main` answers by resolving a credential and running
/// [`Command::Audit`].
/// Test: `super::registration_tests::a_declined_sweep_returns_the_guided_card`,
/// `super::registration_tests::a_confirmed_sweep_audits_the_registered_target`.
///
/// Deliberately NOT `#[non_exhaustive]`, unlike [`Command`] and [`Outcome`]: it
/// crosses only into `main.rs`, and #5502's enforcement is exactly that a new
/// variant fails the shim's match to compile until someone decides what the
/// launch does with it.
#[derive(Debug)]
pub enum Launch {
    /// The flow stopped short of the sweep. This is what to print.
    ///
    /// Boxed because [`Outcome`] is large enough for
    /// `clippy::large_enum_variant` to care, and this enum is returned from
    /// every launch — including the one that carries nothing.
    Reported(Box<Outcome>),
    /// The operator saw the card and agreed to the sweep.
    SweepConfirmed,
}

/// Drive the launch up to the operator's decision: register, advance, ask.
///
/// Why: "one step" means one invocation. The operator names what the audit
/// covers and the flow carries on into tool installation and the sweep without
/// a second command. It stops at the DECISION rather than running the sweep
/// itself so `main` can resolve the inference credential only once the sweep is
/// actually going to happen (#5896 review) — see [`Launch`].
/// What: the loop, then [`Command::Guided`] — which is where auto-install
/// happens, so the tools land in this same invocation — and then the question,
/// when the flow reached [`NextStep::ReadyForRun`]. Everything shown before the
/// answer goes to the terminal; what `main` prints to stdout is whatever it does
/// with the returned [`Launch`].
/// Test: `super::registration_tests::a_scripted_session_registers_and_advances_the_flow`,
/// `super::registration_tests::a_declined_sweep_returns_the_guided_card`,
/// `super::registration_tests::a_confirmed_sweep_audits_the_registered_target`.
///
/// # Errors
///
/// [`AuditError::RegistrationPromptFailed`] when the terminal fails, plus
/// whatever the guided flow fails with. A refused TARGET is not an error here —
/// it is reported and the loop asks again.
pub async fn guided_at_the_terminal(
    session: &Session,
    tty: &mut dyn Tty,
) -> Result<Launch, AuditError> {
    register_targets(session, tty).await?;

    let guided = session.execute(Command::Guided).await?;
    if !ready_for_run(&guided) {
        return Ok(Launch::Reported(Box::new(guided)));
    }

    // The card first: the operator decides whether to commit hours from what is
    // actually installed and registered, not from a bare question.
    say(tty, render(&guided).trim_end())?;
    if !confirmed(tty)? {
        say(tty, SWEEP_DECLINED)?;
        return Ok(Launch::Reported(Box::new(guided)));
    }
    Ok(Launch::SweepConfirmed)
}

/// The capability a confirmed launch runs.
///
/// Why: #5896 shipped [`Command::Run`] here. `Run` reads
/// `state/selected-repos.toml`, which only [`crate::clone`] writes, and nothing
/// in the guided launch clones — [`register_targets`] writes the registry and
/// [`Command::Guided`] installs tools. So on a fresh recipient the one-step
/// launch died at `NoRepositoriesSelected` immediately after saying "Everything
/// is in place", and on a recipient carrying a selection from an earlier
/// `taudit clone` it AUDITED THOSE OLD REPOSITORIES, reported them as audited,
/// and exited 0.
/// What: [`Command::Audit`], whose materialize phase reads the registry and
/// clones the registered targets before the sweep — the only path that bridges
/// registry to selection (`crate::chain`, "Where the registry finally reaches
/// the sweep").
/// Test: `super::registration_tests::a_confirmed_sweep_audits_the_registered_target`.
pub fn confirmed_sweep() -> Command {
    Command::Audit(ChainOptions::default())
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
///
/// The input queue is discarded FIRST (#5896 review). The registration loop
/// ends when the operator presses Enter on an empty line, and a terminal in
/// canonical mode holds whatever arrived after that — so an operator
/// double-tapping Enter to finish adding targets had the second newline
/// answered this question, starting hours of unattended work that spends the
/// client's inference credential. The prompt itself is unchanged: Enter still
/// starts the sweep, it just has to be an Enter pressed after reading the
/// question.
fn confirmed(tty: &mut dyn Tty) -> Result<bool, AuditError> {
    tty.discard_typeahead().map_err(prompt_failed)?;
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
    use std::path::{Path, PathBuf};

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
        /// How many prompts had been issued when typeahead was last discarded.
        ///
        /// The ORDER is what matters: discarding after the question has been
        /// asked closes nothing, so a count alone would not prove the fix.
        discarded_after: Vec<usize>,
    }

    impl FakeTty {
        fn new<I: IntoIterator<Item = &'static str>>(answers: I) -> Self {
            Self {
                answers: answers.into_iter().map(|a| Ok(a.to_owned())).collect(),
                prompts: Vec::new(),
                shown: Vec::new(),
                discarded_after: Vec::new(),
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

        fn discard_typeahead(&mut self) -> io::Result<()> {
            self.discarded_after.push(self.prompts.len());
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

    /// The outcome a launch that stopped short of the sweep hands back.
    fn reported(launch: Launch) -> Outcome {
        match launch {
            Launch::Reported(outcome) => *outcome,
            Launch::SweepConfirmed => panic!("this launch must not have started a sweep"),
        }
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

        let outcome = reported(
            guided_at_the_terminal(&session, &mut tty)
                .await
                .expect("the loop runs"),
        );

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

        let outcome = reported(
            guided_at_the_terminal(&session, &mut tty)
                .await
                .expect("declining is not a failure"),
        );

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

    /// The verb #5896 broke. `Cli::to_command` maps `taudit guided` and a bare
    /// `taudit` onto the same `Command::Guided`, so a rule stated over `Command`
    /// cannot tell them apart — and #5502's own reachability table mandates the
    /// named spelling, which under any pty then blocked on a prompt forever.
    ///
    /// Against `501b6dae5` this fails on the second assertion.
    #[test]
    fn the_named_guided_verb_is_not_interactive() {
        use clap::Parser as _;

        assert!(
            is_interactive(&Cli::parse_from(["taudit"])),
            "a bare launch is the one invocation that may prompt"
        );
        assert!(
            !is_interactive(&Cli::parse_from(["taudit", "guided"])),
            "`taudit guided` is what a script and the E2E harness run — it must \
             print the card and exit, never block on a prompt"
        );
        // The same holds however the global options are spelled around it.
        assert!(
            !is_interactive(&Cli::parse_from(["taudit", "guided", "--no-install"])),
            "the verb decides, not the flags beside it"
        );
        assert!(is_interactive(&Cli::parse_from(["taudit", "--no-install"])));
    }

    /// Every named subcommand is something a script or the E2E harness runs, so
    /// none of them may be stopped for a prompt however the terminal looks.
    ///
    /// Stated over the CAPABILITY set, which is the secondary guard — the
    /// argv-level rule is `the_named_guided_verb_is_not_interactive`.
    #[test]
    fn only_a_bare_launch_is_interactive() {
        use crate::clone::CloneOptions;
        use crate::distribute::DistributeOptions;
        use crate::registry::TargetKind;
        use crate::run::RunOptions;

        assert!(may_prompt(&Command::Guided));
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
                !may_prompt(&command),
                "{command:?} must never prompt — a script runs it"
            );
        }
    }

    /// The typeahead hazard (#5896 review). The registration loop ENDS on an
    /// empty line, and the very next question commits to hours of unattended
    /// work that spends the client's inference credential — so an operator
    /// double-tapping Enter had the second newline answer a question they never
    /// read. The queue is discarded BEFORE the question is asked; discarding it
    /// afterwards would close nothing.
    ///
    /// The prompt semantics are unchanged and deliberately so: Enter still
    /// starts the sweep.
    #[test]
    fn the_sweep_prompt_discards_typeahead_first() {
        let mut tty = FakeTty::new([""]);
        assert!(confirmed(&mut tty).expect("Enter still starts the sweep"));
        assert_eq!(
            tty.discarded_after,
            vec![0],
            "typeahead must be discarded before the prompt is issued, not after"
        );
        assert_eq!(tty.prompts, vec![RUN_PROMPT.to_owned()]);
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

    /// An engagement pinning the versions `install_stubs` records.
    const CONFIG: &str = r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks."

[tools]
tga = "2.9.4"
trusty-search = "0.47.0"
trusty-analyze = "0.9.2"
trusty-review = "0.15.1"
"#;

    /// Stub binaries plus the version record, which together are what the
    /// sweep's `pinned_binaries` preflight accepts — so nothing reaches the
    /// network. `tga` writes the manifest a real one would for whatever
    /// `--output` it is handed, so the sweep succeeds over ANY repository and
    /// the only thing a test can be reading is WHICH one ran.
    #[cfg(unix)]
    fn install_stubs(work: &crate::workdir::WorkDir) {
        use crate::tools::RequiredTool;
        use std::os::unix::fs::PermissionsExt as _;

        const TGA: &str = "#!/bin/sh\nout=\"\"\nwhile [ $# -gt 0 ]; do\n  \
             case \"$1\" in --output) out=\"$2\"; shift;; esac\n  shift\ndone\n\
             mkdir -p \"$out\"\n\
             printf '[report]\\ntitle = \"Acme\"\\n\\n[[repositories]]\\n\
             name = \"acme\"\\npath = \"/r\"\\n' > \"$out/manifest.toml\"\nexit 0\n";

        for tool in RequiredTool::ALL {
            let path = tool.path_in(work);
            let body = if tool == RequiredTool::Tga {
                TGA
            } else {
                "#!/bin/sh\nexit 0\n"
            };
            std::fs::write(&path, body).expect("stub binary");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let record = format!(
            "[[tools]]\ncrate_name = \"tga\"\nversion = \"2.9.4\"\nbinary = \"{tga}\"\n\
             [[tools]]\ncrate_name = \"trusty-search\"\nversion = \"0.47.0\"\nbinary = \"{s}\"\n\
             [[tools]]\ncrate_name = \"trusty-analyze\"\nversion = \"0.9.2\"\nbinary = \"{a}\"\n\
             [[tools]]\ncrate_name = \"trusty-review\"\nversion = \"0.15.1\"\nbinary = \"{r}\"\n",
            tga = RequiredTool::Tga.path_in(work).display(),
            s = RequiredTool::TrustySearch.path_in(work).display(),
            a = RequiredTool::TrustyAnalyze.path_in(work).display(),
            r = RequiredTool::TrustyReview.path_in(work).display(),
        );
        std::fs::write(crate::tools::record_path(work), record).expect("write record");
    }

    /// The launch as `main` drives it: the terminal decides, and a confirmed
    /// sweep runs [`confirmed_sweep`] through the one dispatch door.
    ///
    /// It mirrors `main.rs` deliberately — the defect this covers was a single
    /// wrong `Command` at that seam, so a test that named the command itself
    /// would assert the mistake rather than the behaviour.
    async fn launch(session: &Session, tty: &mut dyn Tty) -> Result<Outcome, AuditError> {
        match guided_at_the_terminal(session, tty).await? {
            Launch::Reported(outcome) => Ok(*outcome),
            Launch::SweepConfirmed => session.execute(confirmed_sweep()).await,
        }
    }

    /// Whichever repositories the confirmed sweep actually audited, whichever
    /// capability it reached them through.
    fn audited(outcome: &Outcome) -> Vec<String> {
        let repos = match outcome {
            Outcome::Audit(report) => &report.run.repos,
            Outcome::Run(report) => &report.repos,
            other => panic!("a confirmed sweep produced no sweep at all: {other:?}"),
        };
        repos.iter().map(|r| r.repo.name.clone()).collect()
    }

    /// 🔴 The defect #5896 shipped, and the reason it is dangerous rather than
    /// merely broken.
    ///
    /// The confirmed launch ran `Command::Run`, which audits
    /// `state/selected-repos.toml` — a file only `crate::clone` writes, and
    /// nothing in the guided launch clones. A recipient carrying a selection
    /// from an earlier `taudit clone` therefore registered `acme/newrepo`, was
    /// told "Everything is in place", and received an audit of `acme-oldrepo`
    /// reported as success. A wrong answer that exits 0, at a client site.
    ///
    /// The fix is `Command::Audit`, whose materialize phase reads the REGISTRY
    /// and clones what is registered — the only path bridging the two records.
    /// Against `501b6dae5` this fails on the audited-set assertion with
    /// `["acme-oldrepo"]`.
    ///
    /// Offline by construction: the registered target already has a checkout on
    /// disk, so `clone_all` reuses it (`CloneState::Reused`) and never reaches
    /// GitHub.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_confirmed_sweep_audits_the_registered_target() {
        use crate::clone::destination;
        use crate::registry::{Registry, TargetKind};
        use crate::run::{SelectedRepo, save_selection};
        use crate::workdir::{Area, WorkDir};

        let tmp = tempfile::tempdir().expect("tempdir");
        let work = WorkDir::new(tmp.path().join("work"));
        work.create().expect("create the tree");
        install_stubs(&work);

        // The stale selection an earlier `taudit clone` left behind.
        std::fs::create_dir_all(work.path(Area::Repos).join("acme-oldrepo")).expect("old checkout");
        save_selection(
            &work,
            &[SelectedRepo {
                name: "acme-oldrepo".to_owned(),
                path: PathBuf::from("repos/acme-oldrepo"),
            }],
        )
        .expect("write the stale selection");

        // What this engagement is actually registered to audit. Its checkout
        // exists, so materialize reuses it rather than cloning.
        let mut registry = Registry::default();
        registry.insert(registry::parse(Some(TargetKind::Repo), "acme/newrepo").expect("parses"));
        registry.save(&work).expect("write the registry");
        std::fs::create_dir_all(destination(&work, "acme/newrepo").expect("a plain name"))
            .expect("registered checkout");

        let config_path = tmp.path().join("engagement.toml");
        std::fs::write(&config_path, CONFIG).expect("write the engagement config");
        let session = Session::new(work)
            .with_config_path(&config_path)
            .with_auto_install(false);

        // Enter to finish adding targets, then Enter to start the sweep.
        let mut tty = FakeTty::new(["", ""]);
        let outcome = launch(&session, &mut tty)
            .await
            .expect("the confirmed launch runs");

        assert_eq!(
            audited(&outcome),
            vec!["acme/newrepo".to_owned()],
            "the confirmed sweep must audit what this engagement REGISTERED, \
             never whatever an earlier clone left selected"
        );
        assert_eq!(
            outcome.exit_code(),
            0,
            "a clean sweep over the registered target must exit zero: {outcome:?}"
        );
    }

    /// The fresh-recipient half of the same defect: nothing was ever cloned, so
    /// `Command::Run` died at `NoRepositoriesSelected` one line after saying
    /// "Everything is in place". `Command::Audit` clones what is registered.
    ///
    /// Asserted through the capability rather than by running it, because
    /// reaching the sweep for a target with no checkout means reaching GitHub.
    #[test]
    fn the_confirmed_sweep_is_the_capability_that_reads_the_registry() {
        assert_eq!(
            confirmed_sweep(),
            Command::Audit(ChainOptions::default()),
            "only the chain's materialize phase turns a registered target into \
             something the sweep can read"
        );
    }
}
