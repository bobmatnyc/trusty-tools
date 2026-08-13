//! The workspace's single entry point for invoking the GitHub CLI (`gh`) — #5475.
//!
//! Why: before this module, `gh` was spawned from a dozen independent
//! `Command::new("gh")` sites across four crates, each re-deriving its own
//! answer to the same four questions: what to do when the binary is missing,
//! what to do when the user is not authenticated, whether a non-zero exit is
//! an error or a signal, and whether stderr reaches the caller. That is the
//! shape the repo's "common entry point, clean domain demarcation" rule
//! exists to prevent — a message improvement or an auth fix had to land N
//! times or silently drift. #5487 (repo discovery) and #5215 (clone) are both
//! about to route through `gh`, so the entry point lands before them rather
//! than after two more bespoke copies.
//! What: [`GhCommand`], a builder that renders `gh <args>` with an optional
//! `--repo`, working directory, and environment overlay, and runs it either
//! blocking ([`GhCommand::output_blocking`]) or on tokio
//! ([`GhCommand::output`]). Every runner returns a [`GhOutput`] carrying the
//! full triple (exit code, stdout, stderr) and NEVER decides on the caller's
//! behalf that a non-zero exit is fatal — `gh pr checks` uses its exit code to
//! report check state, so that policy belongs at the call site. The policies
//! callers actually share are offered as thin combinators on top:
//! [`GhCommand::stdout`] (non-zero is an error), [`GhCommand::nonempty_stdout`]
//! (also rejects empty output), [`GhCommand::json`] (also parses), and
//! [`gh_available`] (a pure probe). Spawn failure is classified: a missing
//! binary becomes [`GhError::NotInstalled`] with the `gh auth login` hint,
//! distinct from any other IO failure.
//! Test: `tests::*` in this file cover argv rendering, `--repo` placement,
//! the missing-binary classification, non-zero-exit mapping, empty-output
//! rejection, and JSON parse failure — all without a real `gh` install, by
//! pointing `PATH`-independent spawns at a nonexistent binary and by
//! exercising the pure `GhOutput` policy combinators directly.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Output;

use serde::de::DeserializeOwned;

/// The GitHub CLI binary name. The single place the string `"gh"` is spelled.
pub const GH_BIN: &str = "gh";

/// Guidance appended whenever the `gh` binary itself cannot be spawned.
pub const GH_MISSING_HINT: &str =
    "The GitHub CLI must be installed and authenticated (`gh auth login`) for this to work.";

/// Every way a `gh` invocation can fail, as a structured library error.
///
/// Why: the call sites this replaced each hand-rolled a message, so "gh is not
/// installed" and "gh is installed but you are not logged in" were
/// indistinguishable to a caller that wanted to degrade differently for each.
/// What: `NotInstalled` is the missing-binary case specifically;
/// `Spawn` is any other IO failure starting the process; `NonZero` carries the
/// exit status and trimmed stderr; `Empty` is a zero exit with no stdout
/// (which for `gh auth token` means unauthenticated); `Json` is a `--json`
/// payload that did not parse.
/// Test: `missing_binary_is_classified_not_installed`,
/// `nonzero_exit_carries_stderr`, `empty_stdout_is_rejected`,
/// `malformed_json_is_reported_as_json_error`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GhError {
    /// `gh` is not on `PATH`.
    #[error("`gh` is not installed or not on PATH. {GH_MISSING_HINT}")]
    NotInstalled,
    /// `gh` is present but the process could not be started.
    #[error("failed to spawn `gh {args}`: {source}")]
    Spawn {
        /// The rendered argv, for the message.
        args: String,
        /// The underlying IO error.
        #[source]
        source: std::io::Error,
    },
    /// `gh` ran and exited non-zero.
    #[error("`gh {args}` failed ({status}): {stderr}")]
    NonZero {
        /// The rendered argv.
        args: String,
        /// The exit status, rendered.
        status: String,
        /// Trimmed stderr.
        stderr: String,
    },
    /// `gh` exited zero but printed nothing where output was required.
    #[error("`gh {args}` returned empty output. {GH_MISSING_HINT}")]
    Empty {
        /// The rendered argv.
        args: String,
    },
    /// A `--json` payload did not parse.
    #[error("failed to parse `gh {args}` JSON output: {source}")]
    Json {
        /// The rendered argv.
        args: String,
        /// The serde failure.
        #[source]
        source: serde_json::Error,
    },
}

/// The complete result of one `gh` invocation.
///
/// Why: a non-zero exit is a FAILURE at some call sites and a SIGNAL at others
/// (`gh pr checks` exits non-zero while checks are pending), so the runner
/// must hand back the raw triple and let the caller pick. Every shared policy
/// is a method here rather than a behaviour baked into the spawn.
/// What: the rendered argv, the exit code (`None` if signalled), whether the
/// exit was zero, and lossy-decoded stdout/stderr.
/// Test: `output_ok_maps_nonzero_to_error`, `combined_joins_streams`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct GhOutput {
    /// The argv this output came from, rendered space-separated (no `gh`).
    pub args: String,
    /// Exit code, or `None` when the process was terminated by a signal.
    pub code: Option<i32>,
    /// Whether the process exited zero.
    pub success: bool,
    /// Lossy-decoded stdout, verbatim (never trimmed).
    pub stdout: String,
    /// Lossy-decoded stderr, verbatim (never trimmed).
    pub stderr: String,
}

impl GhOutput {
    /// Assemble an outcome from a run this module did not spawn.
    ///
    /// Why: [`GhCommand::to_std_command`] exists for call sites that own their
    /// own runner (a kill-on-expiry timeout, a bounded worker thread). Those
    /// callers still want the shared policy combinators — `ok`, `combined`,
    /// `stdout_trimmed` — so they need a way to build the triple the runner
    /// produced. `#[non_exhaustive]` is what makes that a constructor rather
    /// than a struct literal (#5475).
    /// What: `code` is the process exit code, `None` when signalled; `success`
    /// is derived as `code == Some(0)`.
    /// Test: `from_parts_derives_success_from_the_exit_code`.
    pub fn from_parts(
        args: impl Into<String>,
        code: Option<i32>,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
    ) -> Self {
        Self {
            args: args.into(),
            code,
            success: code == Some(0),
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    fn from_output(args: String, out: Output) -> Self {
        Self {
            args,
            code: out.status.code(),
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Treat a non-zero exit as an error, returning `self` otherwise.
    pub fn ok(self) -> Result<Self, GhError> {
        if self.success {
            return Ok(self);
        }
        Err(GhError::NonZero {
            args: self.args,
            status: self
                .code
                .map_or_else(|| "signalled".to_string(), |c| format!("exit {c}")),
            stderr: self.stderr.trim().to_string(),
        })
    }

    /// stdout with surrounding whitespace removed.
    pub fn stdout_trimmed(&self) -> &str {
        self.stdout.trim()
    }

    /// stdout followed by stderr, for callers that surface `gh`'s full report.
    pub fn combined(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

/// A `gh` invocation, built then run.
///
/// Why: the one place that decides how `gh` is located, how its argv is
/// rendered for error messages, and how a spawn failure is classified.
/// What: an argv plus an optional `--repo` selector, working directory, and
/// environment overlay. Never goes through a shell, so no argument is ever
/// word-split or glob-expanded.
/// Test: `argv_renders_without_the_binary_name`, `repo_is_prepended_once`.
#[derive(Debug, Clone, Default)]
pub struct GhCommand {
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    envs: Vec<(OsString, OsString)>,
    env_removals: Vec<OsString>,
}

impl GhCommand {
    /// Build an invocation from a fully-formed argv (without the `gh` itself).
    pub fn new<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Self {
            args: args
                .into_iter()
                .map(|a| a.as_ref().to_os_string())
                .collect(),
            ..Self::default()
        }
    }

    /// Prepend `--repo <owner/repo>`; `None` leaves `gh`'s cwd-remote default.
    #[must_use]
    pub fn repo(mut self, repo: Option<&str>) -> Self {
        if let Some(repo) = repo.filter(|r| !r.trim().is_empty()) {
            let mut prefixed: Vec<OsString> = vec![OsString::from("--repo"), OsString::from(repo)];
            prefixed.append(&mut self.args);
            self.args = prefixed;
        }
        self
    }

    /// Run `gh` with this working directory rather than the caller's.
    #[must_use]
    pub fn cwd(mut self, dir: impl AsRef<Path>) -> Self {
        self.cwd = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Overlay one environment variable on the child process.
    #[must_use]
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.envs
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    /// Remove one environment variable from the child process.
    ///
    /// Why: `gh` reads `GH_REPO`, `GH_HOST` and `GH_TOKEN` from the
    /// environment, so a probe that must answer for a SPECIFIC directory or
    /// account has to strip the ambient overrides rather than inherit them.
    #[must_use]
    pub fn env_remove(mut self, key: impl AsRef<OsStr>) -> Self {
        self.env_removals.push(key.as_ref().to_os_string());
        self
    }

    /// The argv rendered for error messages — lossy, space-separated, no `gh`.
    pub fn argv_display(&self) -> String {
        self.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn configure<C: CommandLike>(&self, mut cmd: C) -> C {
        cmd.apply_args(&self.args);
        if let Some(dir) = &self.cwd {
            cmd.apply_cwd(dir);
        }
        for (k, v) in &self.envs {
            cmd.apply_env(k, v);
        }
        for k in &self.env_removals {
            cmd.apply_env_remove(k);
        }
        cmd
    }

    fn classify(&self, err: std::io::Error) -> GhError {
        // #5475: a missing binary is the one spawn failure callers want to
        // degrade on differently, so it gets its own variant here rather than
        // a string match at each call site.
        if err.kind() == std::io::ErrorKind::NotFound {
            GhError::NotInstalled
        } else {
            GhError::Spawn {
                args: self.argv_display(),
                source: err,
            }
        }
    }

    /// The configured [`std::process::Command`], not yet spawned.
    ///
    /// Why: a call site that owns its own spawn machinery — a wall-clock
    /// timeout, a kill-on-expiry probe — still has to agree with everyone else
    /// on WHICH binary, WHICH argv, and WHICH environment scrub. This hands it
    /// the configured command and lets it keep its own runner, rather than
    /// forcing every such caller to re-spell `Command::new("gh")` (#5475).
    /// What: applies argv, working directory, and the environment overlay and
    /// removals to a fresh `std::process::Command`.
    /// Test: `to_std_command_carries_argv_and_env`.
    pub fn to_std_command(&self) -> std::process::Command {
        self.configure(std::process::Command::new(GH_BIN))
    }

    /// Run `gh` to completion on the current thread.
    ///
    /// A non-zero exit is NOT an error here — see [`GhOutput::ok`].
    pub fn output_blocking(&self) -> Result<GhOutput, GhError> {
        let out = self
            .configure(std::process::Command::new(GH_BIN))
            .output()
            .map_err(|e| self.classify(e))?;
        Ok(GhOutput::from_output(self.argv_display(), out))
    }

    /// Run `gh` to completion on the tokio runtime.
    ///
    /// A non-zero exit is NOT an error here — see [`GhOutput::ok`].
    pub async fn output(&self) -> Result<GhOutput, GhError> {
        let out = self
            .configure(tokio::process::Command::new(GH_BIN))
            .output()
            .await
            .map_err(|e| self.classify(e))?;
        Ok(GhOutput::from_output(self.argv_display(), out))
    }

    /// Run `gh`, requiring a zero exit, and return verbatim stdout.
    pub async fn stdout(&self) -> Result<String, GhError> {
        Ok(self.output().await?.ok()?.stdout)
    }

    /// Blocking [`GhCommand::stdout`].
    pub fn stdout_blocking(&self) -> Result<String, GhError> {
        Ok(self.output_blocking()?.ok()?.stdout)
    }

    /// Run `gh`, requiring a zero exit AND non-empty output, trimmed.
    ///
    /// Why: `gh auth token` exits zero and prints nothing when no account is
    /// logged in, so a caller that only checked the exit status would hand an
    /// empty string onward as if it were a credential.
    pub async fn nonempty_stdout(&self) -> Result<String, GhError> {
        require_nonempty(self.output().await?.ok()?)
    }

    /// Blocking [`GhCommand::nonempty_stdout`].
    pub fn nonempty_stdout_blocking(&self) -> Result<String, GhError> {
        require_nonempty(self.output_blocking()?.ok()?)
    }

    /// Run `gh --json …`, requiring a zero exit, and deserialize stdout.
    pub async fn json<T: DeserializeOwned>(&self) -> Result<T, GhError> {
        let out = self.output().await?.ok()?;
        serde_json::from_str(&out.stdout).map_err(|source| GhError::Json {
            args: out.args,
            source,
        })
    }
}

fn require_nonempty(out: GhOutput) -> Result<String, GhError> {
    let trimmed = out.stdout_trimmed().to_string();
    if trimmed.is_empty() {
        return Err(GhError::Empty { args: out.args });
    }
    Ok(trimmed)
}

/// Is `gh` installed AND authenticated?
///
/// Why: a fallback chooser needs one boolean, and every failure mode (missing
/// binary, missing login, IO error) means the same thing to it: do not pick
/// the `gh` backend. This collapses them deliberately — it is a PROBE, not a
/// gate, and no state advances on the strength of a `true` it did not earn.
/// Callers that must distinguish the modes use [`GhCommand`] directly and read
/// [`GhError`].
/// What: runs `gh auth status`, returns whether it exited zero.
/// Test: `gh_available_is_a_pure_probe` (asserts no panic; the boolean is
/// environment-dependent).
pub async fn gh_available() -> bool {
    matches!(GhCommand::new(["auth", "status"]).output().await, Ok(o) if o.success)
}

/// Blocking [`gh_available`].
pub fn gh_available_blocking() -> bool {
    matches!(GhCommand::new(["auth", "status"]).output_blocking(), Ok(o) if o.success)
}

/// The two `Command` types share no trait, so this is the seam that lets
/// [`GhCommand::configure`] be written once for both.
trait CommandLike {
    fn apply_args(&mut self, args: &[OsString]);
    fn apply_cwd(&mut self, dir: &Path);
    fn apply_env(&mut self, k: &OsStr, v: &OsStr);
    fn apply_env_remove(&mut self, k: &OsStr);
}

impl CommandLike for std::process::Command {
    fn apply_args(&mut self, args: &[OsString]) {
        self.args(args);
    }
    fn apply_cwd(&mut self, dir: &Path) {
        self.current_dir(dir);
    }
    fn apply_env(&mut self, k: &OsStr, v: &OsStr) {
        self.env(k, v);
    }
    fn apply_env_remove(&mut self, k: &OsStr) {
        self.env_remove(k);
    }
}

impl CommandLike for tokio::process::Command {
    fn apply_args(&mut self, args: &[OsString]) {
        self.args(args);
    }
    fn apply_cwd(&mut self, dir: &Path) {
        self.current_dir(dir);
    }
    fn apply_env(&mut self, k: &OsStr, v: &OsStr) {
        self.env(k, v);
    }
    fn apply_env_remove(&mut self, k: &OsStr) {
        self.env_remove(k);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(code: i32, stdout: &str, stderr: &str) -> GhOutput {
        GhOutput::from_parts("auth token", Some(code), stdout, stderr)
    }

    #[test]
    fn from_parts_derives_success_from_the_exit_code() {
        assert!(GhOutput::from_parts("x", Some(0), "", "").success);
        assert!(!GhOutput::from_parts("x", Some(1), "", "").success);
        // A signalled process is never a success.
        assert!(!GhOutput::from_parts("x", None, "", "").success);
    }

    #[test]
    fn argv_renders_without_the_binary_name() {
        let cmd = GhCommand::new(["issue", "list", "--limit", "5"]);
        assert_eq!(cmd.argv_display(), "issue list --limit 5");
    }

    #[test]
    fn repo_is_prepended_once() {
        let cmd = GhCommand::new(["issue", "list"]).repo(Some("o/r"));
        assert_eq!(cmd.argv_display(), "--repo o/r issue list");
    }

    #[test]
    fn repo_none_or_blank_leaves_argv_untouched() {
        assert_eq!(
            GhCommand::new(["issue", "list"]).repo(None).argv_display(),
            "issue list"
        );
        assert_eq!(
            GhCommand::new(["issue", "list"])
                .repo(Some("   "))
                .argv_display(),
            "issue list"
        );
    }

    #[test]
    fn output_ok_maps_nonzero_to_error() {
        let err = out(1, "", "  not logged in  ").ok().unwrap_err();
        let GhError::NonZero { status, stderr, .. } = &err else {
            panic!("expected NonZero, got {err:?}");
        };
        assert_eq!(status, "exit 1");
        assert_eq!(stderr, "not logged in");
    }

    #[test]
    fn nonzero_exit_carries_stderr() {
        assert!(
            out(2, "", "boom")
                .ok()
                .unwrap_err()
                .to_string()
                .contains("boom")
        );
    }

    #[test]
    fn zero_exit_passes_through_ok() {
        assert_eq!(out(0, "tok\n", "").ok().unwrap().stdout_trimmed(), "tok");
    }

    #[test]
    fn empty_stdout_is_rejected() {
        // #5475: `gh auth token` exits ZERO with empty stdout when no account
        // is logged in — the fail-open this combinator exists to close.
        let err = require_nonempty(out(0, "  \n ", "")).unwrap_err();
        assert!(matches!(err, GhError::Empty { .. }), "got {err:?}");
    }

    #[test]
    fn nonempty_stdout_trims_a_real_value() {
        assert_eq!(require_nonempty(out(0, " a-token \n", "")).unwrap(), "a-token");
    }

    #[test]
    fn combined_joins_streams() {
        assert_eq!(out(0, "a", "b").combined(), "a\nb");
    }

    #[test]
    fn malformed_json_is_reported_as_json_error() {
        let e = serde_json::from_str::<serde_json::Value>("{oops").unwrap_err();
        let err = GhError::Json {
            args: "issue list --json number".to_string(),
            source: e,
        };
        assert!(err.to_string().contains("JSON"), "{err}");
    }

    #[test]
    fn missing_binary_is_classified_not_installed() {
        let cmd = GhCommand::new(["auth", "status"]);
        let err = cmd.classify(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert!(matches!(err, GhError::NotInstalled), "got {err:?}");
        assert!(err.to_string().contains("gh auth login"));
    }

    #[test]
    fn other_spawn_failures_stay_distinct_from_not_installed() {
        let cmd = GhCommand::new(["auth", "status"]);
        let err = cmd.classify(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert!(matches!(err, GhError::Spawn { .. }), "got {err:?}");
        assert!(err.to_string().contains("auth status"));
    }

    #[tokio::test]
    async fn gh_available_is_a_pure_probe() {
        let _ = gh_available().await;
    }

    #[test]
    fn cwd_and_env_survive_onto_the_builder() {
        let cmd = GhCommand::new(["repo", "view"])
            .cwd("/tmp")
            .env("GH_CONFIG_DIR", "/tmp/ghcfg");
        assert_eq!(cmd.cwd.as_deref(), Some(Path::new("/tmp")));
        assert_eq!(cmd.envs.len(), 1);
        assert_eq!(cmd.envs[0].0, OsString::from("GH_CONFIG_DIR"));
    }

    #[test]
    fn to_std_command_carries_argv_and_env() {
        let cmd = GhCommand::new(["pr", "list"])
            .cwd("/tmp")
            .env("GH_PAGER", "")
            .env_remove("GH_REPO")
            .to_std_command();
        assert_eq!(cmd.get_program(), OsStr::new("gh"));
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, vec![OsStr::new("pr"), OsStr::new("list")]);
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/tmp")));
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(envs.contains(&(OsStr::new("GH_PAGER"), Some(OsStr::new("")))));
        assert!(envs.contains(&(OsStr::new("GH_REPO"), None)));
    }

    #[test]
    fn env_removals_are_recorded() {
        let cmd = GhCommand::new(["pr", "list"]).env_remove("GH_REPO");
        assert_eq!(cmd.env_removals, vec![OsString::from("GH_REPO")]);
    }
}
