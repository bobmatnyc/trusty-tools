//! Creating the engagement a launch needs, when the recipient has none (#5970).
//!
//! Why: the guided launch was built for a recipient who already holds the
//! `engagement.toml` an auditor hands them. Nothing in the crate synthesised
//! one, so a bare `trusty-audit` in an empty directory registered targets,
//! printed `Tools: 0/4 installed`, named a command to run next, and stopped —
//! never asking for the OpenRouter key, because the key resolves inside a branch
//! that state could not reach. Three separate gates hit the same missing file
//! and none of them named it.
//!
//! The owner's ruling, 2026-08-18, fixes the ORDER as well as the gap: key,
//! then the config, then the tools, and registration LAST. Registration going
//! first is what produced that transcript — an operator naming repositories for
//! an engagement that did not exist yet.
//!
//! What: [`cold_start`] is the whole path. It runs once, when there is no
//! config; a launch that finds one returns `Ok(None)` and behaves exactly as it
//! did before. [`key_for_a_cold_start`] is the credential decision on its own,
//! [`create_engagement`] the write.
//!
//! Three properties this module exists to hold:
//!
//! - **The key is asked for FIRST and written before anything else runs.** A
//!   failed write STOPS the launch ([`AuditError::EngagementNotCreated`]) rather
//!   than carrying the key in memory for one process and leaving nothing behind.
//! - **The value never reaches the terminal, a log, or an error.** Reading is
//!   `crate::cli::credential`'s echo-suppressed prompt, the file is written at
//!   mode 0600 through [`crate::workdir::write_private_atomically`], and every
//!   line this module shows is a `const` or a path — no entered text is ever
//!   interpolated into one.
//! - **Nothing is installed that this machine cannot install.** The preflight
//!   runs before the download and refuses by name.
//!
//! It lives beside [`crate::cli::credential`] and [`crate::cli::registration`]
//! for the reason both give: [`Session::execute`] is the one door every front
//! end uses, and none of them has a terminal to prompt on. What crosses that
//! boundary is a written config, never a way to ask for one.
//!
//! Test: `super::bootstrap_tests`.

use std::path::Path;

use crate::cli::credential::{Resolved, Tty, resolve};
use crate::config::{EngagementConfig, SecretKey, ToolPins};
use crate::error::AuditError;
use crate::run::ENV_INFERENCE_CREDENTIAL;
use crate::session::{Command, Session};
use crate::tools::{self, ToolAvailability};
use crate::workdir;

/// Shown once, before anything is asked. Names the state, not a command.
const OPENING: &str = "\nNo engagement is configured here. Setting one up.";

/// What a new engagement is asked to assess until the operator edits it.
///
/// It is a field of the file they now own, and the file says so — this is a
/// starting point, not a policy.
const DEFAULT_INSTRUCTIONS: &str = "Assess the last 52 weeks across the registered repositories.";

/// Shown above the per-tool preflight lines.
const CHECKING_TOOLS: &str = "\nChecking the pinned tools";

/// Everything a cold start needs from outside this process.
///
/// Why: `main.rs` reads the process environment once and hands the result in —
/// that is this crate's standing rule (`crate::workdir::WorkDir::resolve`,
/// `crate::cli::credential::resolve`), and it is what makes the cold start's
/// branches provable from a test binary that has whatever the developer happens
/// to have exported. Bundled rather than passed as two arguments so
/// `crate::cli::registration::guided_at_the_terminal` forwards one value.
/// What: where the pins come from, and `OPENROUTER_API_KEY` as `main` read it.
/// Test: `super::bootstrap_tests` builds one with [`ColdStart::fixed`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ColdStart {
    /// Where the versions written into the new config come from.
    pub pins: PinResolver,
    /// `OPENROUTER_API_KEY`, or `None` when it is unset.
    pub exported_key: Option<String>,
}

impl ColdStart {
    /// The production wiring: latest published pins, and this process's
    /// environment.
    pub fn from_environment() -> Self {
        Self {
            pins: PinResolver::LatestPublished,
            exported_key: std::env::var(ENV_INFERENCE_CREDENTIAL).ok(),
        }
    }

    /// A cold start that reaches no release list and reads no environment.
    ///
    /// The seam #5970's tests drive, and the shape a `--pins` flag would take.
    pub fn fixed(pins: ToolPins) -> Self {
        Self {
            pins: PinResolver::Fixed(pins),
            exported_key: None,
        }
    }
}

/// Where a cold start gets the versions it writes into the new config.
///
/// Why: resolving the latest published release reaches GitHub, and every branch
/// of the cold start above that call has to be provable from a test binary with
/// no network — the same reason `crate::validate::RepoProbe` is a seam rather
/// than a direct `gh` call.
/// What: [`PinResolver::LatestPublished`] is production;
/// [`PinResolver::Fixed`] answers with what it was given and touches nothing.
/// Test: `super::bootstrap_tests` drives every case through `Fixed`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum PinResolver {
    /// Ask the release list for the latest published stable of each tool.
    LatestPublished,
    /// Answer with these exact pins, without a network call.
    Fixed(ToolPins),
}

impl PinResolver {
    /// The pins to record in the new engagement config.
    ///
    /// # Errors
    ///
    /// [`AuditError::PinsUnresolved`] when a release list could not be read.
    pub async fn resolve(&self) -> Result<ToolPins, AuditError> {
        match self {
            PinResolver::LatestPublished => tools::latest_pins().await,
            PinResolver::Fixed(pins) => Ok(pins.clone()),
        }
    }
}

/// Set this directory up as an engagement, when it is not one yet.
///
/// Why: see the module docs — this is #5970's ruling, in the order it states.
///
/// # Preconditions
/// `tty` is a terminal somebody is at. `crate::cli::registration::is_interactive`
/// and `DevTty::open` are what establish that, in `main`.
///
/// # Postconditions
/// On `Ok(Some(_))`, an `engagement.toml` exists at the session's config path,
/// owner-readable only, carrying the key and an exact version per tool; and
/// unless auto-install is off, all four tools are on disk at those versions. On
/// `Ok(None)` a config was already there and nothing was asked or written. On
/// `Err`, either nothing was written, or the config was written and the tools
/// were not — never a key held only in memory.
///
/// What: prompt, write, preflight, install. The caller registers targets AFTER
/// this returns, which is the half of the ruling that cannot be seen from inside
/// this function — `crate::cli::registration::guided_at_the_terminal` holds it,
/// and `the_key_is_asked_for_before_the_first_target` is what pins it.
/// Test: `super::bootstrap_tests::a_cold_start_writes_the_engagement_owner_only`,
/// `super::bootstrap_tests::a_config_that_cannot_be_written_stops_the_launch`,
/// `super::bootstrap_tests::an_existing_engagement_is_left_alone`.
///
/// # Errors
///
/// [`AuditError::NoCredentialSource`] and the prompt's own failures;
/// [`AuditError::PinsUnresolved`]; [`AuditError::EngagementNotCreated`] when the
/// file cannot be written; [`AuditError::ToolNotInstallable`] when the preflight
/// refuses; and whatever [`Command::InstallTools`] fails with.
pub async fn cold_start(
    session: &Session,
    cold: &ColdStart,
    tty: &mut dyn Tty,
) -> Result<Option<EngagementConfig>, AuditError> {
    let config_path = session.config_path().to_path_buf();
    if EngagementConfig::load_if_present(&config_path)?.is_some() {
        return Ok(None);
    }
    say(tty, OPENING)?;

    // Step 1 — the key, before anything else. A cold start passes `None` for the
    // configured tier: there is no config to have carried one.
    let resolved = key_for_a_cold_start(cold.exported_key.clone(), &config_path, Some(tty))?;
    let source = resolved.source();

    // Step 2 — the pins, then the file. Resolving BEFORE the write is what keeps
    // the config either complete or absent; there is no half-written engagement.
    let pins = cold.pins.resolve().await?;
    let config = create_engagement(&config_path, &resolved.into_key(), &pins)?;
    say(tty, source.describe())?;
    say(tty, &format!("  ok: written to {}", config_path.display()))?;

    // Steps 3 and 4 — preflight, then install. `--no-install` skips both, and
    // the guided card that follows names the install step, exactly as it does
    // for an auditor-supplied config.
    if session.auto_install() {
        say(tty, CHECKING_TOOLS)?;
        let checked = tools::preflight(session.work_dir(), &config.tools).await?;
        for entry in &checked {
            say(tty, &describe(entry))?;
        }
        tools::ensure_installable(checked)?;
        session.execute(Command::InstallTools).await?;
    }
    Ok(Some(config))
}

/// Decide which credential a cold start runs with.
///
/// Why: split out so the no-terminal branch is a test rather than a reading of
/// `main`. That branch matters: without a config there is no `openrouter_key` to
/// fall back on, so a cold start with no terminal and nothing exported has NO
/// source, and must refuse rather than proceed with an engagement it cannot
/// finish.
/// What: [`crate::cli::credential::resolve`] with the configured tier passed as
/// `None` — the environment first, then the prompt. One decision function, so a
/// cold start and an ordinary run cannot disagree about precedence.
/// Test: `super::bootstrap_tests::without_a_terminal_a_cold_start_refuses`,
/// `super::bootstrap_tests::an_exported_key_is_used_without_a_prompt`.
///
/// # Errors
///
/// [`AuditError::NoCredentialSource`] when nothing supplies a key and there is
/// no terminal; the prompt's own failures otherwise.
pub fn key_for_a_cold_start(
    env: Option<String>,
    config_path: &Path,
    tty: Option<&mut dyn Tty>,
) -> Result<Resolved, AuditError> {
    resolve(env, None, config_path, tty)
}

/// Write the first `engagement.toml`, owner-readable only, and load it back.
///
/// Why: a failed write here is the fail-open shape #5970 exists to close, so it
/// is its own error variant rather than a bare filesystem failure — the message
/// has to say that the key was not saved, which no generic write error can.
/// What: renders a template with a blank key, substitutes the real one through
/// [`crate::config::generate`] — the crate's ONE writer of a plaintext key into
/// a config, which also proves the result parses — and publishes it through
/// [`crate::workdir::write_private_atomically`] at mode 0600. Reloading from the
/// rendered text rather than from disk keeps the returned value the one that was
/// just validated.
/// Test: `super::bootstrap_tests::a_cold_start_writes_the_engagement_owner_only`,
/// `super::bootstrap_tests::a_config_that_cannot_be_written_stops_the_launch`.
///
/// # Errors
///
/// [`AuditError::Parse`] or [`AuditError::Render`] when the pins would not
/// produce a loadable config, and [`AuditError::EngagementNotCreated`] when the
/// file cannot be written.
pub fn create_engagement(
    path: &Path,
    key: &SecretKey,
    pins: &ToolPins,
) -> Result<EngagementConfig, AuditError> {
    let rendered = crate::config::generate(&template(pins), key, path)?;
    workdir::write_private_atomically(path, &rendered).map_err(|source| {
        AuditError::EngagementNotCreated {
            path: path.to_path_buf(),
            source: Box::new(source),
        }
    })?;
    EngagementConfig::from_toml(&rendered, path)
}

/// A new engagement's TOML, with the key left blank for `generate` to fill.
///
/// The blank field is deliberate: it keeps this function unable to write a
/// credential, so the crate still has exactly one place that does.
///
/// The `[models]` table is written out in full rather than left to the built-in
/// fallback. Owner ruling, 2026-08-18: all inference for this crate is
/// OpenRouter, and an unset `provider` falls through to `trusty-review`'s own
/// default, which is Bedrock. Naming it in the file is what makes the choice
/// visible to the recipient reading it, which is the transparency premise of the
/// whole handoff (#5473). All FOUR fields are named because
/// [`crate::inference`] treats the table as all-or-none: a config carrying only
/// `provider` is [`AuditError::SplitInferenceSelection`], not a partial
/// override. The values are [`crate::inference`]'s own constants, so the written
/// file states exactly what the crate would otherwise have fallen back to and
/// no slug is minted here.
fn template(pins: &ToolPins) -> String {
    format!(
        "openrouter_key = \"\"\n\
         instructions = \"{DEFAULT_INSTRUCTIONS}\"\n\
         \n[tools]\n\
         tga = \"{tga}\"\n\
         trusty-search = \"{search}\"\n\
         trusty-analyze = \"{analyze}\"\n\
         trusty-review = \"{review}\"\n\
         \n[models]\n\
         provider = \"{provider}\"\n\
         reviewer = \"{reviewer}\"\n\
         verifier = \"{verifier}\"\n\
         summarizer = \"{summarizer}\"\n",
        tga = pins.tga.version(),
        search = pins.trusty_search.version(),
        analyze = pins.trusty_analyze.version(),
        review = pins.trusty_review.version(),
        provider = crate::inference::PROVIDER_OPENROUTER,
        reviewer = crate::inference::DEFAULT_REVIEWER_MODEL,
        verifier = crate::inference::DEFAULT_VERIFIER_MODEL,
        summarizer = crate::inference::DEFAULT_SUMMARIZER_MODEL,
    )
}

/// One preflight line, in the crate's terminal style.
fn describe(entry: &ToolAvailability) -> String {
    let state = match (entry.installed, entry.problem.as_ref()) {
        (true, _) => "installed".to_owned(),
        (false, None) => "not installed -> installable".to_owned(),
        (false, Some(problem)) => format!("not installed -> CANNOT INSTALL: {problem}"),
    };
    format!(
        "  {name:<16}{version:<12}{state}",
        name = entry.tool.binary_name(),
        version = entry.version,
    )
}

/// Show one line, turning a terminal failure into the credential prompt's error.
///
/// The same variant `crate::cli::credential` uses: everything this module shows
/// belongs to the credential exchange, and an operator whose terminal died has
/// one problem, not two.
fn say(tty: &mut dyn Tty, line: &str) -> Result<(), AuditError> {
    tty.say(line)
        .map_err(|source| AuditError::CredentialPromptFailed { source })
}

/// A pin set naming versions this crate's tests never reach a network for.
///
/// `pub(crate)` so `crate::cli::registration`'s tests script a cold start
/// without one, rather than each growing its own copy.
#[cfg(test)]
pub(crate) fn fixed_pins(version: &str) -> ToolPins {
    let pin = || crate::config::ToolPin::Version(version.to_owned());
    ToolPins {
        tga: pin(),
        trusty_search: pin(),
        trusty_analyze: pin(),
        trusty_review: pin(),
    }
}

#[cfg(test)]
mod bootstrap_tests {
    use super::*;
    use crate::cli::credential::CredentialSource;
    use crate::config::ToolPin;
    use crate::tools::RequiredTool;
    use crate::validate::RepoProbe;
    use crate::workdir::WorkDir;
    use std::collections::VecDeque;
    use std::io;

    const KEY: &str = "sk-or-v1-entered-at-the-cold-start";

    /// A scripted terminal, the shape both sibling modules' fakes have.
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
            self.answers.pop_front().transpose()
        }

        fn say(&mut self, line: &str) -> io::Result<()> {
            self.shown.push(line.to_owned());
            Ok(())
        }

        fn discard_typeahead(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A session rooted in `dir`, with the config beside it and no install.
    ///
    /// `with_auto_install(false)` is what keeps every case here offline: it is
    /// the production `--no-install` path, not a test-only door.
    fn session_in(dir: &Path) -> Session {
        Session::new(WorkDir::new(dir.join("work")))
            .with_config_path(dir.join("engagement.toml"))
            .with_repo_probe(RepoProbe::accepting())
            .with_auto_install(false)
    }

    /// The whole ruling's second and third steps: the key is written into a
    /// config that carries the resolved pins, and the file is owner-only.
    ///
    /// Against `8cfdda3ab` no config is written at all, so this fails on the
    /// first assertion.
    #[tokio::test]
    async fn a_cold_start_writes_the_engagement_owner_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());
        let mut tty = FakeTty::new([KEY, KEY]);

        let config = cold_start(&session, &ColdStart::fixed(fixed_pins("1.2.3")), &mut tty)
            .await
            .expect("the cold start runs")
            .expect("a directory with no config is a cold start");

        assert_eq!(config.openrouter_key.expose(), KEY);
        assert_eq!(config.tools.tga.version(), "1.2.3");
        assert_eq!(config.tools.trusty_review.version(), "1.2.3");

        let path = session.config_path();
        let reloaded = EngagementConfig::load(path).expect("the written config loads");
        assert_eq!(reloaded.openrouter_key.expose(), KEY);
        assert_eq!(reloaded.tools.trusty_search.version(), "1.2.3");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(path).expect("stat").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
        }
    }

    /// The hygiene gate, on the strings: nothing typed reaches a prompt, a
    /// guidance line, or the `Debug` render of what comes back.
    #[tokio::test]
    async fn the_entered_key_never_reaches_the_terminal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());
        let mut tty = FakeTty::new([KEY, "sk-or-v1-mistyped", KEY, KEY]);

        let config = cold_start(&session, &ColdStart::fixed(fixed_pins("1.2.3")), &mut tty)
            .await
            .expect("a mismatched retype is not a failure")
            .expect("a cold start");

        let displayed = tty.everything_displayed();
        assert!(
            !displayed.contains(KEY),
            "the terminal showed it: {displayed}"
        );
        assert!(
            !displayed.contains("sk-or-v1-mistyped"),
            "the terminal showed a rejected entry: {displayed}"
        );
        let debug = format!("{config:?}");
        assert!(!debug.contains(KEY), "Debug leaked the key: {debug}");
    }

    /// 🔴 The fail-open arm. A write that fails must STOP the launch — carrying
    /// on would run an hours-long sweep on a key held in memory for one process
    /// and leave the recipient with nothing set up.
    ///
    /// The write is made to fail by putting a regular FILE where the config's
    /// parent directory belongs, so `create_dir_all` cannot succeed. Against
    /// `8cfdda3ab` nothing writes the config, so no such error exists.
    #[tokio::test]
    async fn a_config_that_cannot_be_written_stops_the_launch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let blocker = tmp.path().join("not-a-directory");
        std::fs::write(&blocker, b"a file, where a directory is needed").expect("write blocker");
        let path = blocker.join("engagement.toml");

        let err = create_engagement(&path, &SecretKey::new(KEY), &fixed_pins("1.2.3"))
            .expect_err("an unwritable config must not read as a set-up engagement");

        let AuditError::EngagementNotCreated { path: named, .. } = &err else {
            panic!("expected EngagementNotCreated, got {err:?}");
        };
        assert_eq!(named, &path, "the refusal must name the file");
        let rendered = err.to_string();
        assert!(rendered.contains("was NOT saved"), "{rendered}");
        assert!(
            !rendered.contains(KEY),
            "the error quoted the key: {rendered}"
        );
        assert!(!path.exists(), "nothing may have been written");
    }

    /// The same arm through the whole cold start, which is where it matters: the
    /// key HAS been typed by the time the write fails, and the launch must stop
    /// rather than carry it forward.
    ///
    /// The config lands in a directory that is readable but not writable, so the
    /// absence probe still answers "no config here" — the state a cold start is
    /// for — and the write is what fails. Skipped as root, for whom mode bits
    /// are advisory.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_launch_that_cannot_write_the_engagement_stops_with_the_key_entered() {
        use std::os::unix::fs::PermissionsExt as _;

        // SAFETY: `geteuid` reads this process's own effective uid and writes
        // through no pointer.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let readonly = tmp.path().join("readonly");
        std::fs::create_dir(&readonly).expect("mkdir");
        std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o555)).expect("chmod");

        let session = Session::new(WorkDir::new(tmp.path().join("work")))
            .with_config_path(readonly.join("engagement.toml"))
            .with_auto_install(false);
        let mut tty = FakeTty::new([KEY, KEY]);

        let err = cold_start(&session, &ColdStart::fixed(fixed_pins("1.2.3")), &mut tty)
            .await
            .expect_err("a launch that cannot write its engagement must stop");

        assert!(
            matches!(err, AuditError::EngagementNotCreated { .. }),
            "{err:?}"
        );
        assert!(
            !session.config_path().exists(),
            "nothing may have been written"
        );
        let displayed = tty.everything_displayed();
        assert!(
            !displayed.contains(KEY),
            "the terminal showed it: {displayed}"
        );
        assert!(
            !err.to_string().contains(KEY),
            "the error quoted the key: {err}"
        );

        // Let the tempdir clean up after itself.
        std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o755))
            .expect("chmod back");
    }

    /// 🔴 The `curl … | sh` arm, in the state that has no config to fall back
    /// on. Nothing exported and no terminal must be an actionable refusal — never
    /// a silent proceed, and never a hang.
    #[test]
    fn without_a_terminal_a_cold_start_refuses() {
        let err = key_for_a_cold_start(None, Path::new("/engagement/engagement.toml"), None)
            .expect_err("nothing supplies a key and nobody can be asked");

        assert!(
            matches!(err, AuditError::NoCredentialSource { .. }),
            "{err:?}"
        );
        let text = err.to_string();
        assert!(text.contains(ENV_INFERENCE_CREDENTIAL), "{text}");
        assert!(text.contains("engagement.toml"), "{text}");
    }

    /// A blank export is how a shell says "not for this command", so it must
    /// fall through rather than winning the tier and writing an empty key.
    #[test]
    fn a_blank_export_does_not_count_as_a_source() {
        let err = key_for_a_cold_start(
            Some("   ".to_owned()),
            Path::new("/engagement/engagement.toml"),
            None,
        )
        .expect_err("a blank value is not a credential");
        assert!(
            matches!(err, AuditError::NoCredentialSource { .. }),
            "{err:?}"
        );
    }

    /// An auditor who exported the key is not asked for it again.
    #[test]
    fn an_exported_key_is_used_without_a_prompt() {
        let mut tty = FakeTty::new(["sk-or-v1-never-read"]);
        let resolved = key_for_a_cold_start(
            Some(KEY.to_owned()),
            Path::new("/engagement/engagement.toml"),
            Some(&mut tty),
        )
        .expect("the environment supplies it");

        assert_eq!(resolved.source(), CredentialSource::Environment);
        assert_eq!(resolved.into_key().expose(), KEY);
        assert!(tty.prompts.is_empty(), "{:?}", tty.prompts);
    }

    /// A recipient who received a config from their auditor must not be asked
    /// for anything, and their file must not be touched.
    #[tokio::test]
    async fn an_existing_engagement_is_left_alone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());
        let existing = create_engagement(
            session.config_path(),
            &SecretKey::new("sk-or-v1-from-the-auditor"),
            &fixed_pins("9.9.9"),
        )
        .expect("seed a config");
        let before = std::fs::read_to_string(session.config_path()).expect("read");

        let mut tty = FakeTty::new([]);
        let outcome = cold_start(&session, &ColdStart::fixed(fixed_pins("1.2.3")), &mut tty)
            .await
            .expect("an existing engagement is not a failure");

        assert!(
            outcome.is_none(),
            "a configured directory is not a cold start"
        );
        assert!(tty.prompts.is_empty(), "{:?}", tty.prompts);
        assert_eq!(
            std::fs::read_to_string(session.config_path()).expect("read"),
            before,
            "the auditor's config must survive verbatim"
        );
        assert_eq!(existing.tools.tga.version(), "9.9.9");
    }

    /// A config that is present and MALFORMED is not an absent one — treating it
    /// as a cold start would overwrite a file the recipient may have hand-edited.
    #[tokio::test]
    async fn a_malformed_engagement_is_not_treated_as_a_cold_start() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = session_in(tmp.path());
        std::fs::write(session.config_path(), "this is not toml = = =").expect("write");

        let mut tty = FakeTty::new([KEY, KEY]);
        let err = cold_start(&session, &ColdStart::fixed(fixed_pins("1.2.3")), &mut tty)
            .await
            .expect_err("a malformed config must not be overwritten");
        assert!(matches!(err, AuditError::Parse { .. }), "{err:?}");
        assert!(tty.prompts.is_empty(), "{:?}", tty.prompts);
    }

    /// The pins are written verbatim, so what the file says and what the
    /// resolver returned cannot drift.
    #[test]
    fn the_template_records_every_pin() {
        let mut pins = fixed_pins("1.2.3");
        pins.trusty_review = ToolPin::Version("4.5.6".to_owned());
        let text = template(&pins);

        let loaded = crate::config::generate(&text, &SecretKey::new(KEY), Path::new("e.toml"))
            .and_then(|t| EngagementConfig::from_toml(&t, Path::new("e.toml")))
            .expect("the template is a loadable engagement config");
        assert_eq!(loaded.tools.tga.version(), "1.2.3");
        assert_eq!(loaded.tools.trusty_review.version(), "4.5.6");
        assert!(
            !text.contains(KEY),
            "the template must not be able to carry a key: {text}"
        );
    }

    /// 🔴 Owner ruling, 2026-08-18: all inference for this crate is OpenRouter.
    ///
    /// A config that leaves `[models]` out falls through to `trusty-review`'s
    /// own default provider, which is Bedrock — a spend on an account this
    /// engagement never named. So the synthesised file names the provider, and
    /// therefore all four fields: `crate::inference` treats the table as
    /// all-or-none, so `provider` alone would be
    /// `AuditError::SplitInferenceSelection` rather than a partial override.
    ///
    /// The end-to-end proof is the second half: the child environment this
    /// config produces selects OpenRouter, which is the thing that actually
    /// decides where the spend goes.
    #[test]
    fn the_written_config_selects_openrouter_for_every_role() {
        let path = Path::new("engagement.toml");
        let text =
            crate::config::generate(&template(&fixed_pins("1.2.3")), &SecretKey::new(KEY), path)
                .expect("generates");
        let config = EngagementConfig::from_toml(&text, path).expect("loads");

        assert_eq!(
            config.models.provider.as_deref(),
            Some(crate::inference::PROVIDER_OPENROUTER)
        );
        assert_eq!(
            config.models.reviewer.as_deref(),
            Some(crate::inference::DEFAULT_REVIEWER_MODEL)
        );
        assert_eq!(
            config.models.verifier.as_deref(),
            Some(crate::inference::DEFAULT_VERIFIER_MODEL)
        );
        assert_eq!(
            config.models.summarizer.as_deref(),
            Some(crate::inference::DEFAULT_SUMMARIZER_MODEL)
        );
        assert!(
            !text.contains("bedrock"),
            "a synthesised engagement must never name another provider: {text}"
        );

        // What the `tga audit` child is actually handed. `|_| None` is an
        // environment naming none of the four, so the config is what decides.
        let env =
            crate::inference::inference_env(&config, |_| None).expect("the whole table resolves");
        let provider = env
            .iter()
            .find(|(name, _)| *name == crate::inference::ENV_PROVIDER)
            .map(|(_, value)| value.as_str());
        assert_eq!(
            provider,
            Some(crate::inference::PROVIDER_OPENROUTER),
            "the child must be pointed at OpenRouter: {env:?}"
        );
    }

    /// The preflight line an operator reads, in all three states.
    #[test]
    fn each_preflight_state_reads_differently() {
        let installed = describe(&ToolAvailability {
            tool: RequiredTool::Tga,
            version: "1.2.3".to_owned(),
            installed: true,
            problem: None,
        });
        assert!(installed.contains("tga") && installed.contains("installed"));
        assert!(!installed.contains("not installed"), "{installed}");

        let pending = describe(&ToolAvailability {
            tool: RequiredTool::TrustySearch,
            version: "0.4.0".to_owned(),
            installed: false,
            problem: None,
        });
        assert!(
            pending.contains("not installed -> installable"),
            "{pending}"
        );

        let refused = describe(&ToolAvailability {
            tool: RequiredTool::TrustyReview,
            version: "0.1.0".to_owned(),
            installed: false,
            problem: Some(Box::new(
                trusty_installer::download::pinned::PinnedError::VersionNotPublished {
                    crate_name: "trusty-review".to_owned(),
                    version: "0.1.0".to_owned(),
                    available: vec!["0.16.0".to_owned()],
                },
            )),
        });
        assert!(refused.contains("CANNOT INSTALL"), "{refused}");
        assert!(refused.contains("0.16.0"), "{refused}");
    }
}
