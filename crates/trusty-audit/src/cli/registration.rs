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
//! What: [`guided_at_the_terminal`] is the whole launch path — the targets, then
//! the review menu, then the guided flow's own next step, then the sweep when
//! the operator says so. [`register_targets`] is the prompt loop on its own.
//! Both take a [`Tty`](crate::cli::credential::Tty), so every branch is drivable
//! from a test binary that has no controlling terminal.
//!
//! Where the targets come from is decided by [`targets_file::detect`] (#5978): a
//! `repos.txt` or `boards.txt` beside the config REPLACES the prompt loop rather
//! than seeding it, and one unparseable line in either file registers nothing at
//! all. [`adopt_without_a_terminal`] is the same adoption for a launch with no
//! terminal, which gets the counts and the list instead of the menu.
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
use crate::cli::bootstrap::{self, ColdStart};
use crate::cli::credential::Tty;
use crate::cli::{Cli, render, review, targets_file};
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
/// What: the cold start (#5970) when this directory holds no engagement config,
/// then the loop, then [`Command::Guided`] — which is where auto-install
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
    cold: &ColdStart,
    tty: &mut dyn Tty,
) -> Result<Launch, AuditError> {
    // #5970: the engagement FIRST — key, config, tools — and registration after
    // it. The other order is what this issue reported: an operator naming
    // repositories for an engagement that did not exist, then being told to run
    // `trusty-audit install`. A directory that already has a config skips this
    // entirely and the flow is what it always was.
    bootstrap::cold_start(session, cold, tty).await?;

    // #5978: a `repos.txt` or `boards.txt` beside the config IS the target
    // list, so the per-target prompt loop is skipped rather than seeded. A file
    // with an unparseable line never reaches here — `detect` refuses the whole
    // read, and nothing has been registered by then.
    match targets_file::detect(session.config_path())? {
        Some(detected) => {
            for line in adopt(session, &detected).await? {
                say(tty, &line)?;
            }
        }
        None => {
            register_targets(session, tty).await?;
        }
    }

    // Both paths end here — one confirmation surface, reached two ways.
    review::review(session, tty).await?;

    let guided = session.execute(Command::Guided).await?;
    if !ready_for_run(&guided) {
        return Ok(Launch::Reported(Box::new(guided)));
    }

    // The card first: the operator decides whether to commit hours from what is
    // actually installed and registered, not from a bare question.
    say(tty, render(&guided).trim_end())?;
    if !confirmed(tty, RUN_PROMPT)? {
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

/// Register everything a targets file named, reporting what happened.
///
/// Why: the registration is separate from [`targets_file::detect`] so the parse
/// can be all-or-nothing while the registration is not. A line that parses but
/// cannot be VALIDATED — an unreachable repository, a board whose credential is
/// not configured — is the ordinary refusal the prompt loop already tolerates,
/// and the counts on the review menu are where the operator sees the shortfall.
/// What: one [`register_one`] per spec, then the lines to show: every refusal,
/// then where the list came from. It states no COUNT — the caller does, from
/// the registry rather than from the lines read. A file naming one repository
/// twice, in two spellings, reads as two lines and registers one target, and a
/// count taken from the lines would tell the operator two repositories will be
/// cloned. Returning the lines rather than printing them is what lets the
/// no-terminal launch print the same text to stderr (`main.rs`).
/// Test: `super::registration_tests::a_targets_file_registers_without_prompting_for_each_target`,
/// `super::registration_tests::a_refused_line_does_not_stop_the_rest_of_the_file`.
///
/// # Errors
///
/// Whatever [`Session::execute`] fails with for a reason other than a refused
/// target.
pub async fn adopt(
    session: &Session,
    detected: &targets_file::Detected,
) -> Result<Vec<String>, AuditError> {
    let mut lines = Vec::new();
    for spec in &detected.specs {
        if let Err(refusal) = register_one(session, spec).await {
            lines.push(format!("not registered: {refusal}"));
        }
    }
    lines.push(format!("Read the targets from {}.", detected.named()));
    Ok(lines)
}

/// Adopt the targets files for a launch that has no terminal to review at.
///
/// Why: #5978's stop-on-parse-error rule holds with and without a terminal —
/// a scripted run that quietly audits four of a file's twenty repositories
/// reports success over the sixteen it never saw. What a terminal buys is the
/// MENU; without one the counts and the full list are printed and the run
/// proceeds.
/// What: `None` when neither file is there, which is every invocation that
/// predates this feature. The caller prints the lines — this crate is a library
/// and writes to no stream of its own.
/// Test: `super::registration_tests::without_a_terminal_the_files_are_still_adopted`,
/// `super::registration_tests::without_a_terminal_a_bad_line_still_refuses_everything`.
///
/// # Errors
///
/// [`AuditError::TargetsFileRefused`] naming every unparseable line, plus
/// whatever registering one of them fails with.
pub async fn adopt_without_a_terminal(
    session: &Session,
) -> Result<Option<Vec<String>>, AuditError> {
    let Some(detected) = targets_file::detect(session.config_path())? else {
        return Ok(None);
    };
    let mut lines = adopt(session, &detected).await?;
    // The counts and the full list, because nobody can ask for either here.
    // Both come from the registry, which is what the sweep will read.
    let registered = review::registered(session).await?;
    lines.push(review::summary(&registered));
    if !registered.is_empty() {
        lines.push(review::listing(&registered));
    }
    Ok(Some(lines))
}

/// Register one typed entry through the same path `trusty-audit add` takes.
///
/// [`registry::parse`] decides which kind the spec is — the CLI knows because
/// the operator used a verb, and here they did not — and then the registration
/// goes through [`Session::execute`] so validation, the idempotent re-add and
/// the registry lock are the ones `add` already has.
pub(super) async fn register_one(session: &Session, spec: &str) -> Result<Target, AuditError> {
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

/// Ask `prompt` once, after discarding anything typed ahead of it.
///
/// Why: a terminal in canonical mode holds whatever arrived before the program
/// asked for it. The registration loop ends when the operator presses Enter on
/// an empty line, so an operator double-tapping Enter to finish adding targets
/// had the second newline answer whatever came next — which was, and is, a
/// question that commits hours of unattended work spending the client's
/// inference credential (#5896 review). #5978 makes it worse rather than
/// better: a stray line at the review menu can select `delete`.
/// What: `discard_typeahead` FIRST, then the read. Discarding afterwards would
/// close nothing. `Ok(None)` at end of input.
/// Test: `super::registration_tests::the_sweep_prompt_discards_typeahead_first`,
/// `super::super::review::review_tests::the_menu_discards_typeahead_before_every_choice`.
///
/// # Errors
///
/// [`AuditError::RegistrationPromptFailed`] when the terminal cannot be read,
/// written, or flushed.
pub(super) fn read_deliberately(
    tty: &mut dyn Tty,
    prompt: &str,
) -> Result<Option<String>, AuditError> {
    tty.discard_typeahead().map_err(prompt_failed)?;
    tty.read_line(prompt).map_err(prompt_failed)
}

/// Ask `prompt` as a yes/no, defaulting to yes.
///
/// Enter means yes: by the time either caller asks, what the answer commits to
/// is what the operator launched this for. Anything starting with `n` declines;
/// end of input declines too, because a terminal that closed cannot have agreed
/// to anything. The prompt is a parameter since #5978, because the review menu
/// and the sweep are two questions taking the same guard.
fn confirmed(tty: &mut dyn Tty, prompt: &str) -> Result<bool, AuditError> {
    let answer = read_deliberately(tty, prompt)?;
    Ok(answer.is_some_and(|line| !line.trim().to_ascii_lowercase().starts_with('n')))
}

/// Show one line, turning a terminal failure into this module's error.
pub(super) fn say(tty: &mut dyn Tty, line: &str) -> Result<(), AuditError> {
    tty.say(line).map_err(prompt_failed)
}

/// Wrap a terminal I/O failure. Never carries what was typed.
pub(super) fn prompt_failed(source: std::io::Error) -> AuditError {
    AuditError::RegistrationPromptFailed { source }
}

#[cfg(test)]
pub(crate) mod registration_tests {
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
    pub(crate) struct FakeTty {
        answers: VecDeque<io::Result<String>>,
        pub(crate) prompts: Vec<String>,
        pub(crate) shown: Vec<String>,
        /// How many prompts had been issued when typeahead was last discarded.
        ///
        /// The ORDER is what matters: discarding after the question has been
        /// asked closes nothing, so a count alone would not prove the fix.
        pub(crate) discarded_after: Vec<usize>,
    }

    impl FakeTty {
        pub(crate) fn new<I: IntoIterator<Item = &'static str>>(answers: I) -> Self {
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
    /// a throwaway working directory, ALREADY carrying an engagement config.
    ///
    /// #5970: the config is seeded so these cases stay about registration. A
    /// launch that finds one skips the cold start entirely, which is exactly the
    /// recipient-handed-a-config path every one of them was written for.
    /// `the_key_is_asked_for_before_the_first_target` is the case that starts
    /// from nothing.
    pub(crate) fn accepting_session(dir: &Path) -> Session {
        let session = Session::new(WorkDir::new(dir.join("work")))
            .with_config_path(dir.join("engagement.toml"))
            .with_repo_probe(RepoProbe::accepting())
            .with_auto_install(false);
        bootstrap::create_engagement(
            session.config_path(),
            &crate::config::SecretKey::new("sk-or-v1-from-the-auditor"),
            &bootstrap::fixed_pins("1.2.3"),
        )
        .expect("seed an engagement config");
        session
    }

    /// Pins no test here ever downloads.
    fn pins() -> ColdStart {
        ColdStart::fixed(bootstrap::fixed_pins("1.2.3"))
    }

    /// The outcome a launch that stopped short of the sweep hands back.
    fn reported(launch: Launch) -> Outcome {
        match launch {
            Launch::Reported(outcome) => *outcome,
            Launch::SweepConfirmed => panic!("this launch must not have started a sweep"),
        }
    }

    pub(crate) fn registered_ids(session: &Session) -> Vec<String> {
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
        let mut tty = FakeTty::new(["acme/api", "not a target", "acme/schema", "", "p"]);

        let outcome = reported(
            guided_at_the_terminal(&session, &pins(), &mut tty)
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

    /// 🔴 The ordering #5970's ruling fixes, asserted as an order rather than as
    /// a set of things that happened.
    ///
    /// A directory with no `engagement.toml` must be asked for the OpenRouter
    /// key FIRST, have the config written, and only then be asked what the audit
    /// covers. Registration going first is what produced the reported transcript:
    /// the operator named repositories for an engagement that did not exist, was
    /// told `Tools: 0/4 installed`, and was never asked for a key at all.
    ///
    /// Against `8cfdda3ab` this fails on the first assertion — the only prompts
    /// issued are registration's.
    #[tokio::test]
    async fn the_key_is_asked_for_before_the_first_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Deliberately NOT `accepting_session`: this case starts from nothing.
        let session = Session::new(WorkDir::new(tmp.path().join("work")))
            .with_config_path(tmp.path().join("engagement.toml"))
            .with_repo_probe(RepoProbe::accepting())
            .with_auto_install(false);
        let mut tty = FakeTty::new([
            "sk-or-v1-cold-start",
            "sk-or-v1-cold-start",
            "acme/api",
            "",
            "p",
        ]);

        guided_at_the_terminal(&session, &pins(), &mut tty)
            .await
            .expect("the cold start runs");

        let first_target = tty
            .prompts
            .iter()
            .position(|p| p == PROMPT)
            .expect("the registration loop must still run");
        let last_key = tty
            .prompts
            .iter()
            .rposition(|p| p.contains("key"))
            .expect("the key must be asked for");
        assert!(
            last_key < first_target,
            "the key must be asked for before the first target: {:?}",
            tty.prompts
        );

        let config = crate::config::EngagementConfig::load(session.config_path())
            .expect("the launch must have written an engagement config");
        assert_eq!(config.openrouter_key.expose(), "sk-or-v1-cold-start");
        assert_eq!(config.tools.tga.version(), "1.2.3");
        assert_eq!(registered_ids(&session), vec!["acme/api"]);
    }

    /// The other half of the ordering: the config is on disk BEFORE the
    /// registration loop opens, not written at the end of a successful launch.
    ///
    /// A terminal that dies during registration therefore leaves a set-up
    /// engagement behind, so re-running does not ask for the key again.
    #[tokio::test]
    async fn the_engagement_survives_a_terminal_that_dies_during_registration() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = Session::new(WorkDir::new(tmp.path().join("work")))
            .with_config_path(tmp.path().join("engagement.toml"))
            .with_repo_probe(RepoProbe::accepting())
            .with_auto_install(false);
        let mut tty = FakeTty::new(["sk-or-v1-cold-start", "sk-or-v1-cold-start"]);
        tty.answers
            .push_back(Err(io::Error::other("the terminal went away")));

        let err = guided_at_the_terminal(&session, &pins(), &mut tty)
            .await
            .expect_err("a terminal that fails is an error");
        assert!(
            matches!(err, AuditError::RegistrationPromptFailed { .. }),
            "{err:?}"
        );
        assert!(
            session.config_path().is_file(),
            "the engagement must already be on disk when registration opens"
        );
    }

    /// Write a targets file beside the session's config.
    fn seed_targets_file(session: &Session, file: &str, body: &str) {
        let dir = session
            .config_path()
            .parent()
            .expect("the config has a directory")
            .to_path_buf();
        std::fs::write(dir.join(file), body).expect("write the targets file");
    }

    /// 🔴 The whole point of #5978: a `repos.txt` is the target list, and the
    /// per-target prompt loop never opens.
    ///
    /// Against the pre-fix commit the file is invisible — the operator is asked
    /// for each of these one at a time, which is the thirty-prompt registration
    /// this issue removes.
    #[tokio::test]
    async fn a_targets_file_registers_without_prompting_for_each_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());
        seed_targets_file(
            &session,
            targets_file::REPOS_FILE,
            "acme/api\n# the front end\nhttps://github.com/acme/web.git\n\
             https://github.com/acme/iac/tree/main\n",
        );
        // `p` leaves the review menu; nothing else is answered.
        let mut tty = FakeTty::new(["p"]);

        guided_at_the_terminal(&session, &pins(), &mut tty)
            .await
            .expect("the launch runs");

        assert_eq!(
            registered_ids(&session),
            vec!["acme/api", "acme/web", "acme/iac"],
            "every line of the file must be registered, `.git` and `/tree/` stripped"
        );
        assert!(
            !tty.prompts.iter().any(|p| p == PROMPT),
            "the per-target prompt loop must be skipped entirely: {:?}",
            tty.prompts
        );
    }

    /// 🔴 One unparseable line registers NOTHING and names that line — with a
    /// terminal, exactly as without one.
    ///
    /// Against a per-line-skip implementation `acme/api` is registered, the
    /// launch proceeds, and the audit covers two of the three repositories the
    /// operator listed while reporting success.
    #[tokio::test]
    async fn one_bad_line_registers_nothing_and_names_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());
        seed_targets_file(
            &session,
            targets_file::REPOS_FILE,
            "acme/api\nnot a repository\nacme/web\n",
        );
        let mut tty = FakeTty::new(["p"]);

        let err = guided_at_the_terminal(&session, &pins(), &mut tty)
            .await
            .expect_err("a bad line stops the launch");

        assert!(
            matches!(err, AuditError::TargetsFileRefused { .. }),
            "{err:?}"
        );
        assert!(
            registered_ids(&session).is_empty(),
            "nothing from a file with a bad line may be registered: {:?}",
            registered_ids(&session)
        );
        let message = err.to_string();
        assert!(message.contains("line 2"), "{message}");
        assert!(message.contains("key is saved"), "{message}");
    }

    /// Both files absent is the pre-#5978 world: the prompt loop opens as it
    /// always did.
    #[tokio::test]
    async fn both_files_absent_falls_back_to_the_prompt_loop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());
        let mut tty = FakeTty::new(["acme/api", "", "p"]);

        guided_at_the_terminal(&session, &pins(), &mut tty)
            .await
            .expect("the launch runs");

        assert!(
            tty.prompts.iter().any(|p| p == PROMPT),
            "with no targets file the loop must still ask: {:?}",
            tty.prompts
        );
        assert_eq!(registered_ids(&session), vec!["acme/api"]);
    }

    /// A line that PARSES but cannot be validated is the ordinary refusal, not
    /// the all-or-nothing case: it is reported and the rest of the file lands.
    /// The review menu's counts are where the operator sees the shortfall.
    #[tokio::test]
    async fn a_refused_line_does_not_stop_the_rest_of_the_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());
        seed_targets_file(&session, targets_file::REPOS_FILE, "acme/api\nacme/web\n");
        // A board whose credential is not configured: well formed, unreachable.
        seed_targets_file(&session, targets_file::BOARDS_FILE, "jira:ACME\n");

        let detected = targets_file::detect(session.config_path())
            .expect("every line parses")
            .expect("both files are present");
        let lines = adopt(&session, &detected).await.expect("adoption runs");

        assert_eq!(registered_ids(&session), vec!["acme/api", "acme/web"]);
        assert!(
            lines.iter().any(|l| l.starts_with("not registered:")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains(targets_file::REPOS_FILE)),
            "the operator must be told where the list came from: {lines:?}"
        );
    }

    /// 🔴 The counts come from the REGISTRY, never from the lines read.
    ///
    /// A file naming one repository twice — the URL an operator pasted and the
    /// short form they typed — is two lines and one target. A count taken from
    /// the lines tells them two repositories will be cloned, which is the
    /// misreported coverage this feature exists to remove, one direction over.
    /// Found by a smoke run of the real binary against a real `repos.txt`.
    #[tokio::test]
    async fn one_repository_named_twice_counts_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());
        seed_targets_file(
            &session,
            targets_file::REPOS_FILE,
            "https://github.com/acme/api.git\nacme/api\n",
        );

        let lines = adopt_without_a_terminal(&session)
            .await
            .expect("adoption runs")
            .expect("the file is present");

        assert_eq!(registered_ids(&session), vec!["acme/api"]);
        let shown = lines.join("\n");
        assert!(
            shown.contains("1 repository to clone"),
            "two spellings of one repository must count once: {shown}"
        );
    }

    /// 🔴 Without a terminal the files are still adopted, and the counts and the
    /// full list are what the caller prints before proceeding.
    #[tokio::test]
    async fn without_a_terminal_the_files_are_still_adopted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());
        seed_targets_file(
            &session,
            targets_file::REPOS_FILE,
            "acme/api\nhttps://github.com/acme/web\n",
        );
        seed_targets_file(
            &session,
            targets_file::BOARDS_FILE,
            "https://linear.app/wonka/team/ENG/active\n",
        );

        let lines = adopt_without_a_terminal(&session)
            .await
            .expect("adoption runs")
            .expect("the files are present");

        assert_eq!(registered_ids(&session), vec!["acme/api", "acme/web"]);
        let shown = lines.join("\n");
        assert!(
            shown.contains("2 repositories to clone, 0 boards to collect from"),
            "the counts must be printed: {shown}"
        );
        assert!(
            shown.contains("acme/api") && shown.contains("acme/web"),
            "the full list must be printed where there is no menu to ask at: {shown}"
        );
    }

    /// 🔴 The stop-on-parse-error rule holds without a terminal too. A scripted
    /// run that quietly audits some of the file reports success over the rest.
    #[tokio::test]
    async fn without_a_terminal_a_bad_line_still_refuses_everything() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());
        seed_targets_file(
            &session,
            targets_file::REPOS_FILE,
            "acme/api\nhttps://gitlab.com/acme/web\n",
        );

        let err = adopt_without_a_terminal(&session)
            .await
            .expect_err("a bad line refuses the run");

        assert!(
            matches!(err, AuditError::TargetsFileRefused { .. }),
            "{err:?}"
        );
        assert!(registered_ids(&session).is_empty());
    }

    /// Neither file present is not a failure without a terminal either — it is
    /// every invocation that predates this feature.
    #[tokio::test]
    async fn without_a_terminal_and_without_files_nothing_happens() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());

        assert!(
            adopt_without_a_terminal(&session)
                .await
                .expect("no files is not a failure")
                .is_none()
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
        let mut tty = FakeTty::new(["acme/api", "", "p", "n"]);

        let outcome = reported(
            guided_at_the_terminal(&session, &pins(), &mut tty)
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
        assert!(!confirmed(&mut tty, RUN_PROMPT).expect("end of input is not an error"));
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
        assert!(confirmed(&mut tty, RUN_PROMPT).expect("Enter still starts the sweep"));
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
            assert!(
                confirmed(&mut tty, RUN_PROMPT).expect("answers"),
                "{answer:?} declined"
            );
        }
        for answer in ["n", "N", "no"] {
            let mut tty = FakeTty::new([answer]);
            assert!(
                !confirmed(&mut tty, RUN_PROMPT).expect("answers"),
                "{answer:?} agreed"
            );
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
        match guided_at_the_terminal(session, &pins(), tty).await? {
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

        // Enter to finish adding targets, `p` to proceed from the review menu
        // (#5978), then Enter to start the sweep.
        let mut tty = FakeTty::new(["", "p", ""]);
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
