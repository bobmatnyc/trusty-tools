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
/// Test: `super::credential_tests::a_resolved_credential_never_renders_its_value`.
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
/// `a_blank_environment_value_falls_through_to_the_config`,
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
