//! First-run credential entry, on the terminal, for the CLI front end (#5868).
//!
//! Why: an engagement config can arrive with a blank `openrouter_key` — the
//! auditor conveys the key out of band, or the client supplies their own — and
//! before this module the only way in was hand-editing the file. So the key is
//! asked for, once, at the moment it is needed.
//!
//! It lives HERE and not in `crate::session` on purpose. `Session::execute` is
//! the one door every front end uses, the Tauri shell (#5477) already calls it
//! and a TUI is next, and none of those has a terminal to prompt on. A prompt
//! reached from inside `execute` would block a GUI on a read nobody can answer.
//! What crosses that boundary is a RESOLVED credential, never a way to obtain
//! one.
//!
//! What: [`resolve_for`] is the production entry point — it reads the
//! environment, loads the engagement config, opens the terminal and persists
//! what was typed. [`resolve`] is the decision underneath it, which touches
//! neither the process environment nor the disk, so every branch is drivable
//! from a test. [`Tty`] is the seam between them, shaped after
//! `crate::validate::RepoProbe`.
//!
//! Three properties this module exists to hold:
//!
//! - **`/dev/tty`, never stdin.** The install path is `curl … | sh`, where
//!   stdin is the pipe carrying the script. Reading it would consume the
//!   installer's remaining bytes and could capture a line of shell as a
//!   credential. See [`DevTty::open`].
//! - **No terminal is an actionable refusal.** Never a hang, never a read of
//!   whatever stdin happened to be — [`crate::error::AuditError::NoCredentialSource`].
//! - **The value is never displayed.** Echo is off while it is typed, and what
//!   this module writes to the terminal is a fixed set of constants that no
//!   entered text is ever interpolated into.
//!
//! Test: `super::credential_tests`.

use std::io;
use std::path::Path;

use crate::config::{EngagementConfig, SecretKey};
use crate::error::AuditError;
use crate::run::ENV_INFERENCE_CREDENTIAL;
use crate::session::{Command, CredentialNeed};
use crate::workdir;

/// How many times a blank or mismatched entry is asked for again.
///
/// Bounded rather than open-ended: a `/dev/tty` that opens and then reports
/// end-of-file returns an empty string every time, and an unbounded loop would
/// spin on it forever instead of failing.
const MAX_ATTEMPTS: usize = 3;

/// What an OpenRouter key conventionally starts with.
///
/// Advisory only — see [`prompt_for_key`]. OpenRouter is free to mint a
/// different shape tomorrow, and refusing over it would strand an operator
/// holding a perfectly good key.
const KEY_PREFIX: &str = "sk-or-v1-";

/// Shown once, before the first prompt.
///
/// Deliberately neutral about whose key it is: the common case is the auditor
/// conveying theirs out of band, and a client using their own OpenRouter
/// account is equally legitimate. It states where the key is written, and makes
/// no claim about what happens to anything sent to the provider — this process
/// cannot know the posture of an account it did not create.
const GUIDANCE: &str = "\nNo OpenRouter key is configured for this engagement.\n\
     Enter the key your auditor provided, or your own OpenRouter key.\n\
     It is saved to the engagement config, readable only by your account.\n";

/// The prompt itself. Neutral: it names what is wanted, not where to get it.
const PROMPT: &str = "OpenRouter key: ";

/// The retype.
const CONFIRM: &str = "Re-enter the key: ";

/// Shown when the entry was blank.
const WAS_BLANK: &str = "That entry was empty. The key cannot be blank.";

/// Shown when the two entries disagreed.
const DID_NOT_MATCH: &str = "Those two entries did not match. Try again.";

/// Shown when the key does not have the conventional prefix. Advisory.
const UNUSUAL_SHAPE: &str =
    "Note: OpenRouter keys usually begin `sk-or-v1-`. Using what you entered.";

/// Where the credential this invocation runs with came from.
///
/// Why: the operator is told which source won, so a run using a stale key in a
/// config instead of the variable they just exported is visible rather than
/// mysterious. The SOURCE is reportable precisely because the VALUE is not.
/// What: one variant per tier of [`resolve`]'s precedence order.
/// Test: `super::credential_tests::the_environment_beats_the_config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CredentialSource {
    /// `OPENROUTER_API_KEY` was set in the environment.
    Environment,
    /// The engagement config already carried one.
    Config,
    /// It was typed at the terminal just now, and written to the config.
    Prompt,
}

impl CredentialSource {
    /// One line naming the source. Never contains a key.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Environment => "credential: from OPENROUTER_API_KEY",
            Self::Config => "credential: from the engagement config",
            Self::Prompt => "credential: entered now, saved to the engagement config",
        }
    }
}

/// A credential and the source it came from.
///
/// `Debug` is derived and safe to use: [`SecretKey`]'s own `Debug` redacts, so
/// the derive prints `SecretKey(<redacted>)` for the value.
/// Test: `super::credential_tests::no_entered_credential_reaches_any_rendered_output`.
#[derive(Debug, Clone)]
pub struct Resolved {
    key: SecretKey,
    source: CredentialSource,
}

impl Resolved {
    /// Where this credential came from.
    pub fn source(&self) -> CredentialSource {
        self.source
    }

    /// The credential, for handing to [`crate::session::Session::with_credential`].
    pub fn into_key(self) -> SecretKey {
        self.key
    }
}

/// The terminal a credential is typed at.
///
/// Why: a trait rather than a direct `rpassword` call, for the same reason
/// `crate::validate::RepoProbe` is one — every branch of the retry loop
/// (blank, mismatch, exhaustion, a terminal that errors) becomes drivable from
/// a test binary that has no controlling terminal at all.
/// What: read a line with echo suppressed, and show a line of guidance. Every
/// string passed to [`Tty::say`] in this module is a `const`; nothing the
/// operator typed is ever interpolated into one.
/// Test: `super::credential_tests` drives the whole resolver through `FakeTty`.
pub trait Tty {
    /// Show `prompt` and read one line with the terminal's echo disabled.
    ///
    /// # Errors
    ///
    /// Whatever reading the terminal failed with.
    fn read_hidden(&mut self, prompt: &str) -> io::Result<String>;

    /// Show `prompt` and read one line with the terminal's echo left ON.
    ///
    /// Why: the guided registration loop (#5885) asks for repository and board
    /// names, which the operator must be able to see and correct as they type.
    /// It lives on THIS trait rather than a second one so the crate has one
    /// terminal seam: [`DevTty`] is the only production implementation of
    /// either read, and a fake drives both.
    /// What: `Ok(None)` at end of input, which is how the loop stops when the
    /// terminal closes rather than spinning on empty reads.
    ///
    /// # Errors
    ///
    /// Whatever reading the terminal failed with.
    fn read_line(&mut self, prompt: &str) -> io::Result<Option<String>>;

    /// Show one line of operator-facing guidance. Never a credential.
    ///
    /// # Errors
    ///
    /// Whatever writing to the terminal failed with.
    fn say(&mut self, line: &str) -> io::Result<()>;
}

/// The process's controlling terminal, reached through `/dev/tty`.
///
/// Why: `/dev/tty` DELIBERATELY, never stdin. Under `curl … | sh` — the install
/// path this feature is built for — stdin is the pipe carrying the script text.
/// Reading it would consume bytes the shell has not executed yet AND could
/// capture a line of that script as a credential. `/dev/tty` is the controlling
/// terminal whatever stdin is attached to, and its absence is exactly the
/// "nobody is there to answer" condition (#5868).
/// What: a write handle on `/dev/tty` for [`Tty::say`]. The hidden read goes
/// through `rpassword`, whose unix defaults are `/dev/tty` for BOTH the prompt
/// and the read, and which clears `ECHO` with `tcsetattr`, restoring the
/// previous terminal state when it drops.
/// Test: a test binary has no controlling terminal, so the resolver's
/// no-terminal branch is what is covered —
/// `super::credential_tests::without_a_terminal_the_refusal_names_both_sources`.
pub struct DevTty {
    /// Open handle on `/dev/tty`, for guidance lines.
    out: std::fs::File,
}

impl DevTty {
    /// The controlling terminal's path.
    const PATH: &'static str = "/dev/tty";

    /// Open the controlling terminal, or report that there is not one.
    ///
    /// Opening for WRITE both proves a terminal exists and gives [`Tty::say`]
    /// somewhere to go, so there is one probe rather than an `is_terminal`
    /// check that could disagree with the open that follows it.
    ///
    /// # Errors
    ///
    /// Whatever opening `/dev/tty` failed with — `ENXIO` or `ENOENT` when the
    /// process has no controlling terminal.
    pub fn open() -> io::Result<Self> {
        let out = std::fs::OpenOptions::new().write(true).open(Self::PATH)?;
        Ok(Self { out })
    }
}

impl Tty for DevTty {
    fn read_hidden(&mut self, prompt: &str) -> io::Result<String> {
        rpassword::prompt_password(prompt)
    }

    /// A READ handle on `/dev/tty` is opened per call, and dropped with it.
    ///
    /// Two reasons, both about not stealing bytes. The handle this struct holds
    /// is write-only, so nothing here changes what [`Tty::say`] or `rpassword`
    /// see. And a reader that outlived one call could buffer past the newline —
    /// on a terminal in canonical mode one `read` returns exactly one line, so a
    /// fresh reader per prompt cannot carry the next entry away with it.
    fn read_line(&mut self, prompt: &str) -> io::Result<Option<String>> {
        use std::io::{BufRead as _, Write as _};
        write!(self.out, "{prompt}")?;
        self.out.flush()?;

        let mut line = String::new();
        let read = io::BufReader::new(std::fs::File::open(Self::PATH)?).read_line(&mut line)?;
        Ok((read > 0).then_some(line))
    }

    fn say(&mut self, line: &str) -> io::Result<()> {
        use std::io::Write as _;
        writeln!(self.out, "{line}")?;
        self.out.flush()
    }
}

/// Resolve the credential `command` needs, prompting and persisting if it must.
///
/// Why: the one function `main` calls, so the production wiring — which
/// variable, which file, which terminal — is decided once and in the library
/// where it is readable, rather than accumulating in the binary shim.
/// What: reads `OPENROUTER_API_KEY`, loads the engagement config when the
/// command needs more than the environment, opens `/dev/tty` if there is one,
/// and delegates the decision to [`resolve`]. A key that came from the prompt
/// is written back to the config through [`persist`] before returning, so the
/// next run does not ask again.
///
/// An ABSENT engagement config returns `Ok(None)` rather than prompting:
/// `Session::execute` refuses over the missing file itself, which names the
/// real problem, and there would be nothing to persist into anyway — a file
/// holding only a key is not a loadable [`EngagementConfig`].
/// Test: `super::credential_tests` covers the decision through [`resolve`];
/// persistence through `a_prompted_key_is_persisted_owner_only`.
///
/// # Errors
///
/// [`AuditError::NoCredentialSource`] when nothing supplies a key and there is
/// no terminal; the prompt errors from [`resolve`]; and read, parse or write
/// failures on the engagement config.
pub fn resolve_for(command: &Command, config_path: &Path) -> Result<Option<Resolved>, AuditError> {
    let need = command.credential_need();
    if need == CredentialNeed::None {
        return Ok(None);
    }

    let env = std::env::var(ENV_INFERENCE_CREDENTIAL).ok();

    // `Environment` commands read nothing else: `distribute` owns its own
    // template fallback (#5825) and must keep it.
    if need == CredentialNeed::Environment {
        return Ok(resolve_environment(env));
    }

    let Some(config) = EngagementConfig::load_if_present(config_path)? else {
        return Ok(None);
    };

    let mut tty = DevTty::open().ok();
    let resolved = resolve(
        env,
        Some(&config.openrouter_key),
        config_path,
        tty.as_mut().map(|t| t as &mut dyn Tty),
    )?;

    if resolved.source == CredentialSource::Prompt {
        persist(config_path, &resolved.key)?;
    }
    Ok(Some(resolved))
}

/// The environment tier on its own, for commands that use no other source.
fn resolve_environment(env: Option<String>) -> Option<Resolved> {
    let key = SecretKey::new(env?);
    (!key.is_empty()).then_some(Resolved {
        key,
        source: CredentialSource::Environment,
    })
}

/// Decide which credential this run uses, without touching the environment or
/// the disk.
///
/// Why: the whole precedence order in one pure function, so every tier and
/// every prompt branch is provable. The alternative — reading `std::env::var`
/// and opening `/dev/tty` inline — is what made #5825's environment tier
/// untestable until it was injected.
/// What: `env` when it is set and not blank; then `configured` when the
/// engagement config already carries one; then the terminal, when there is
/// one. No terminal and nothing configured is
/// [`AuditError::NoCredentialSource`], naming both ways to supply a key.
///
/// A BLANK value at any tier falls through to the next rather than winning.
/// `OPENROUTER_API_KEY=` in the environment is how a shell spells "unset it for
/// this command", and a generated config carries `openrouter_key = ""` when the
/// auditor left the key out (#5825) — neither is a credential.
/// Test: `super::credential_tests::the_environment_beats_the_config`,
/// `a_blank_value_at_any_tier_falls_through_to_the_next`,
/// `without_a_terminal_the_refusal_names_both_sources`.
///
/// # Errors
///
/// [`AuditError::NoCredentialSource`], [`AuditError::CredentialNotEntered`] and
/// [`AuditError::CredentialPromptFailed`] — see each variant.
pub fn resolve(
    env: Option<String>,
    configured: Option<&SecretKey>,
    config_path: &Path,
    tty: Option<&mut dyn Tty>,
) -> Result<Resolved, AuditError> {
    if let Some(resolved) = resolve_environment(env) {
        return Ok(resolved);
    }
    if let Some(key) = configured
        && !key.is_empty()
    {
        return Ok(Resolved {
            key: key.clone(),
            source: CredentialSource::Config,
        });
    }
    let Some(tty) = tty else {
        return Err(AuditError::NoCredentialSource {
            env: ENV_INFERENCE_CREDENTIAL,
            config: config_path.to_path_buf(),
        });
    };
    Ok(Resolved {
        key: prompt_for_key(tty)?,
        source: CredentialSource::Prompt,
    })
}

/// Ask for the key twice, until the two entries agree.
///
/// Why: a credential typed blind is a credential typed wrong, and a wrong one
/// surfaces as a provider auth failure an hour into a sweep. The retype costs
/// one line and moves that discovery to the moment of entry. Re-asking rather
/// than exiting is deliberate too — a mistyped key is the expected outcome, not
/// an exceptional one.
/// What: up to [`MAX_ATTEMPTS`] rounds. Each round reads the key, rejects a
/// blank one, reads it again, and accepts only when the two agree. Surrounding
/// whitespace is trimmed from both before comparison, because it is always a
/// paste artefact. A key without the conventional [`KEY_PREFIX`] draws a note
/// and is then ACCEPTED — OpenRouter can change the shape it mints, and
/// refusing over a guess about the format would strand an operator holding a
/// working key.
/// Test: `super::credential_tests::a_mismatched_retype_asks_again`,
/// `a_blank_entry_is_refused_and_asks_again`,
/// `a_prompt_that_never_agrees_gives_up`,
/// `an_unusual_looking_key_is_accepted_with_a_note`.
///
/// # Errors
///
/// [`AuditError::CredentialNotEntered`] once the attempts are spent, and
/// [`AuditError::CredentialPromptFailed`] when the terminal itself fails.
fn prompt_for_key(tty: &mut dyn Tty) -> Result<SecretKey, AuditError> {
    tty.say(GUIDANCE).map_err(prompt_failed)?;

    for _ in 0..MAX_ATTEMPTS {
        let first = tty.read_hidden(PROMPT).map_err(prompt_failed)?;
        let first = first.trim();
        if first.is_empty() {
            tty.say(WAS_BLANK).map_err(prompt_failed)?;
            continue;
        }

        let again = tty.read_hidden(CONFIRM).map_err(prompt_failed)?;
        if first != again.trim() {
            tty.say(DID_NOT_MATCH).map_err(prompt_failed)?;
            continue;
        }

        if !first.starts_with(KEY_PREFIX) {
            tty.say(UNUSUAL_SHAPE).map_err(prompt_failed)?;
        }
        return Ok(SecretKey::new(first));
    }

    Err(AuditError::CredentialNotEntered {
        attempts: MAX_ATTEMPTS,
    })
}

/// Wrap a terminal I/O failure. Never carries anything that was typed.
fn prompt_failed(source: io::Error) -> AuditError {
    AuditError::CredentialPromptFailed { source }
}

/// Write `key` into the engagement config at `path`, owner-readable only.
///
/// Why: the key is asked for ONCE. Without this the recipient retypes it on
/// every command, which is how a credential ends up in shell history instead.
/// What: re-renders the existing config through [`crate::config::generate`] —
/// the crate's one writer of a plaintext key into a config file, which also
/// proves the result still loads — and publishes it through
/// [`workdir::write_private_atomically`] at mode 0600. Every other field
/// survives verbatim, so this cannot quietly drop the pins.
/// Test: `super::credential_tests::a_prompted_key_is_persisted_owner_only`.
///
/// # Errors
///
/// [`AuditError::Read`] when the existing config cannot be re-read,
/// [`AuditError::Parse`] or [`AuditError::Render`] from `generate`, and
/// [`AuditError::WorkDir`] when the write fails.
fn persist(path: &Path, key: &SecretKey) -> Result<(), AuditError> {
    let template = std::fs::read_to_string(path).map_err(|source| AuditError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let rendered = crate::config::generate(&template, key, path)?;
    workdir::write_private_atomically(path, &rendered)
}

#[cfg(test)]
mod credential_tests {
    use super::*;

    /// A representative engagement config with no key in it — the shape a
    /// client receives when the auditor conveys the key out of band (#5825).
    const BLANK_KEY_CONFIG: &str = r#"
openrouter_key = ""
instructions = "Assess the last 52 weeks."

[tools]
tga = "2.9.4"
trusty-search = "0.47.0"
trusty-analyze = "0.9.2"
trusty-review = "0.15.1"
"#;

    /// A scripted terminal: answers come from a queue, and everything shown to
    /// the operator is recorded so a test can assert on it.
    ///
    /// This is what makes the retry loop provable. A test binary has no
    /// controlling terminal, so `DevTty` cannot be exercised at all — but every
    /// decision the loop makes is above the terminal, not inside it.
    struct FakeTty {
        /// Hidden reads, answered front to back.
        answers: std::collections::VecDeque<io::Result<String>>,
        /// Every prompt string passed to `read_hidden`, in order.
        prompts: Vec<String>,
        /// Every line passed to `say`, in order.
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

        /// A terminal whose first read fails outright.
        fn broken() -> Self {
            let mut fake = Self::new([]);
            fake.answers
                .push_back(Err(io::Error::other("terminal went away")));
            fake
        }

        /// Everything this terminal displayed, prompts and guidance together.
        fn everything_displayed(&self) -> String {
            format!("{}\n{}", self.prompts.join("\n"), self.shown.join("\n"))
        }

        /// How many times the key was asked for.
        fn read_count(&self) -> usize {
            self.prompts.len()
        }
    }

    impl Tty for FakeTty {
        fn read_hidden(&mut self, prompt: &str) -> io::Result<String> {
            self.prompts.push(prompt.to_owned());
            self.answers
                .pop_front()
                .unwrap_or_else(|| Err(io::Error::other("the script ran out of answers")))
        }

        /// Answered from the same queue, so a script that mixes the two reads
        /// stays in one place. Nothing in this module calls it; the guided
        /// registration loop is what does (#5885).
        fn read_line(&mut self, prompt: &str) -> io::Result<Option<String>> {
            self.prompts.push(prompt.to_owned());
            self.answers.pop_front().transpose()
        }

        fn say(&mut self, line: &str) -> io::Result<()> {
            self.shown.push(line.to_owned());
            Ok(())
        }
    }

    fn config_path() -> &'static Path {
        Path::new("/engagement/engagement.toml")
    }

    /// The top tier: an exported variable wins over a config that already has a
    /// perfectly good key, and nothing is asked.
    #[test]
    fn the_environment_beats_the_config() {
        let mut tty = FakeTty::new(["sk-or-v1-typed"]);
        let resolved = resolve(
            Some("sk-or-v1-exported".to_owned()),
            Some(&SecretKey::new("sk-or-v1-configured")),
            config_path(),
            Some(&mut tty),
        )
        .expect("resolves");

        assert_eq!(resolved.source(), CredentialSource::Environment);
        assert_eq!(resolved.into_key().expose(), "sk-or-v1-exported");
        assert_eq!(tty.read_count(), 0, "nothing should have been asked");
    }

    /// The second tier, when nothing is exported.
    #[test]
    fn the_config_is_used_when_nothing_is_exported() {
        let mut tty = FakeTty::new(["sk-or-v1-typed"]);
        let resolved = resolve(
            None,
            Some(&SecretKey::new("sk-or-v1-configured")),
            config_path(),
            Some(&mut tty),
        )
        .expect("resolves");

        assert_eq!(resolved.source(), CredentialSource::Config);
        assert_eq!(resolved.into_key().expose(), "sk-or-v1-configured");
        assert_eq!(tty.read_count(), 0);
    }

    /// `OPENROUTER_API_KEY=` is how a shell says "not for this command", and a
    /// generated config carries `openrouter_key = ""` when the auditor left the
    /// key out (#5825). Neither is a credential, and neither may win a tier.
    #[test]
    fn a_blank_value_at_any_tier_falls_through_to_the_next() {
        let mut tty = FakeTty::new(["sk-or-v1-typed", "sk-or-v1-typed"]);
        let resolved = resolve(
            Some("   ".to_owned()),
            Some(&SecretKey::new("")),
            config_path(),
            Some(&mut tty),
        )
        .expect("resolves");

        assert_eq!(
            resolved.source(),
            CredentialSource::Prompt,
            "a blank environment value and a blank config must both fall through"
        );
        assert_eq!(resolved.into_key().expose(), "sk-or-v1-typed");
    }

    /// The whole point of the retype: two entries that disagree are asked for
    /// again rather than exiting, and the second round is accepted.
    #[test]
    fn a_mismatched_retype_asks_again() {
        let mut tty = FakeTty::new([
            "sk-or-v1-typo",
            "sk-or-v1-different",
            "sk-or-v1-correct",
            "sk-or-v1-correct",
        ]);
        let resolved = resolve(
            None,
            Some(&SecretKey::new("")),
            config_path(),
            Some(&mut tty),
        )
        .expect("the second round agrees");

        assert_eq!(resolved.source(), CredentialSource::Prompt);
        assert_eq!(resolved.into_key().expose(), "sk-or-v1-correct");
        assert_eq!(tty.read_count(), 4, "two rounds of two reads");
        assert!(
            tty.shown.iter().any(|l| l == DID_NOT_MATCH),
            "the operator must be told why they are being asked again: {:?}",
            tty.shown
        );
    }

    /// A blank entry is refused BEFORE the retype — asking someone to confirm
    /// nothing is a round trip that cannot succeed. Whitespace-only counts as
    /// blank: it is the shape a stray paste takes, and a config carrying it
    /// fails hours later at the report stage rather than here.
    #[test]
    fn a_blank_entry_is_refused_and_asks_again() {
        let mut tty = FakeTty::new(["", "   \t ", "sk-or-v1-real", "sk-or-v1-real"]);
        let resolved = resolve(
            None,
            Some(&SecretKey::new("")),
            config_path(),
            Some(&mut tty),
        )
        .expect("the third round is usable");

        assert_eq!(resolved.into_key().expose(), "sk-or-v1-real");
        assert_eq!(
            tty.read_count(),
            4,
            "a blank entry must not consume a retype: {:?}",
            tty.prompts
        );
        assert_eq!(
            tty.shown.iter().filter(|l| *l == WAS_BLANK).count(),
            2,
            "both blank entries are called out: {:?}",
            tty.shown
        );
    }

    /// Surrounding whitespace is a paste artefact, not part of the key — and a
    /// retype that differs only in whitespace must still count as a match.
    #[test]
    fn surrounding_whitespace_is_trimmed_from_the_entry() {
        let mut tty = FakeTty::new(["  sk-or-v1-padded  ", "sk-or-v1-padded\t"]);
        let resolved = resolve(
            None,
            Some(&SecretKey::new("")),
            config_path(),
            Some(&mut tty),
        )
        .expect("resolves");

        assert_eq!(resolved.into_key().expose(), "sk-or-v1-padded");
    }

    /// Bounded, because a `/dev/tty` that opens and then reports end-of-file
    /// answers every read with an empty string — an unbounded loop spins on it.
    #[test]
    fn a_prompt_that_never_agrees_gives_up() {
        let mut tty = FakeTty::new(["a", "b", "c", "d", "e", "f"]);
        let err = resolve(
            None,
            Some(&SecretKey::new("")),
            config_path(),
            Some(&mut tty),
        )
        .expect_err("three disagreements is enough");

        assert!(
            matches!(err, AuditError::CredentialNotEntered { attempts } if attempts == MAX_ATTEMPTS),
            "{err:?}"
        );
        assert_eq!(tty.read_count(), MAX_ATTEMPTS * 2);
    }

    /// The format check is advisory. OpenRouter can change the shape it mints,
    /// and refusing over a guess would strand an operator holding a live key.
    #[test]
    fn an_unusual_looking_key_is_accepted_with_a_note() {
        let mut tty = FakeTty::new(["not-the-usual-shape", "not-the-usual-shape"]);
        let resolved = resolve(
            None,
            Some(&SecretKey::new("")),
            config_path(),
            Some(&mut tty),
        )
        .expect("an unusual key is still a key");

        assert_eq!(resolved.into_key().expose(), "not-the-usual-shape");
        assert!(
            tty.shown.iter().any(|l| l == UNUSUAL_SHAPE),
            "the operator should be told: {:?}",
            tty.shown
        );
    }

    /// The `curl … | sh` case. No terminal and no other source must be an
    /// actionable refusal naming BOTH ways to supply a key — never a hang, and
    /// never a read of whatever stdin happened to be.
    #[test]
    fn without_a_terminal_the_refusal_names_both_sources() {
        let err = resolve(None, Some(&SecretKey::new("")), config_path(), None)
            .expect_err("nothing supplies a key and nobody can be asked");

        assert!(
            matches!(err, AuditError::NoCredentialSource { .. }),
            "{err:?}"
        );
        let text = err.to_string();
        assert!(text.contains(ENV_INFERENCE_CREDENTIAL), "{text}");
        assert!(text.contains("openrouter_key"), "{text}");
        assert!(text.contains("engagement.toml"), "{text}");
    }

    /// A terminal that is present but fails is a different problem from an
    /// operator who cannot type their key, and says so without quoting input.
    #[test]
    fn a_terminal_that_fails_is_reported_without_the_entry() {
        let mut tty = FakeTty::broken();
        let err = resolve(
            None,
            Some(&SecretKey::new("")),
            config_path(),
            Some(&mut tty),
        )
        .expect_err("a broken terminal is an error");

        assert!(
            matches!(err, AuditError::CredentialPromptFailed { .. }),
            "{err:?}"
        );
    }

    /// The hygiene gate, asserted on the strings: nothing the operator typed
    /// may appear in a prompt, a guidance line, an error, or a `Debug` render.
    #[test]
    fn no_entered_credential_reaches_any_rendered_output() {
        const SECRET: &str = "sk-or-v1-must-never-be-displayed";

        // A round that mismatches, a round that is blank, then a good one, so
        // every message the loop can emit is on the record.
        let mut tty = FakeTty::new([SECRET, "sk-or-v1-mismatch", "", SECRET, SECRET]);
        let resolved = resolve(
            None,
            Some(&SecretKey::new("")),
            config_path(),
            Some(&mut tty),
        )
        .expect("resolves on the third round");

        let displayed = tty.everything_displayed();
        assert!(
            !displayed.contains(SECRET),
            "the terminal displayed the key: {displayed}"
        );
        assert!(
            !displayed.contains("sk-or-v1-mismatch"),
            "the terminal displayed a rejected entry: {displayed}"
        );

        // The value survives to the caller even though nothing showed it.
        let debug = format!("{resolved:?}");
        assert!(!debug.contains(SECRET), "Debug leaked the key: {debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
        assert_eq!(resolved.into_key().expose(), SECRET);
    }

    /// The source is reportable precisely because the value is not — every
    /// description names where the key came from and quotes nothing.
    #[test]
    fn every_source_description_names_a_source_and_no_value() {
        for source in [
            CredentialSource::Environment,
            CredentialSource::Config,
            CredentialSource::Prompt,
        ] {
            let text = source.describe();
            assert!(text.starts_with("credential: "), "{text}");
            assert!(!text.contains("sk-or-v1"), "{text}");
        }
    }

    /// A key that was typed is written back, so the recipient is asked once
    /// rather than on every command — and the file it lands in is owner-only.
    #[test]
    fn a_prompted_key_is_persisted_owner_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("engagement.toml");
        std::fs::write(&path, BLANK_KEY_CONFIG).expect("seed the config");

        persist(&path, &SecretKey::new("sk-or-v1-entered")).expect("persists");

        let reloaded = EngagementConfig::load(&path).expect("the rewritten config still loads");
        assert_eq!(reloaded.openrouter_key.expose(), "sk-or-v1-entered");
        // Everything else survives: `generate` substitutes one field.
        assert_eq!(reloaded.tools.tga.version(), "2.9.4");
        assert_eq!(reloaded.instructions, "Assess the last 52 weeks.");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
        }
    }

    /// The production wrapper's cheapest branch, and the one every non-audit
    /// command takes: no need, no environment read, no config load, no prompt.
    #[test]
    fn a_command_that_needs_nothing_resolves_nothing() {
        let resolved = resolve_for(&Command::WorkDir, Path::new("/nonexistent/engagement.toml"))
            .expect("no need is not a failure");
        assert!(resolved.is_none());
    }

    /// An absent config is `Session::execute`'s refusal to make, and there
    /// would be nothing to persist into: a file holding only a key is not a
    /// loadable engagement config.
    #[test]
    fn an_absent_config_defers_rather_than_prompting() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let resolved = resolve_for(
            &Command::Run(crate::run::RunOptions::default()),
            &tmp.path().join("engagement.toml"),
        )
        .expect("an absent config is not this module's error");
        assert!(resolved.is_none());
    }
}
