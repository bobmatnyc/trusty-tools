//! How the resume verify pass reaches `gh` and `git` (#9390).
//!
//! Why: the pass must be testable without spawning real `gh` or touching the
//! network, so the process boundary is a trait the catch-up host carries.
//! What: [`VerifyRunner`] runs one argv in a directory and reports how it
//! ended. [`SystemRunner`] is the production runner: argv only, never a shell,
//! stdin closed, prompts disabled, and `kill_on_drop` so a caller that stops
//! waiting (the pass's timeout drops the future) kills the child.
//! Test: `the_system_runner_kills_a_child_that_outlives_its_bound`,
//! `the_system_runner_reports_a_missing_program`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

/// The most stdout bytes the pass reads back from one command.
const MAX_STDOUT_BYTES: usize = 16 * 1024;

/// How one command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunOutcome {
    /// The process exited; `code` is `None` when a signal ended it.
    Exited {
        /// The exit code.
        code: Option<i32>,
        /// Its stdout, lossily decoded and capped at [`MAX_STDOUT_BYTES`].
        stdout: String,
    },
    /// The program is not installed or not on `PATH`.
    Missing,
    /// The spawn failed for another reason.
    SpawnFailed(String),
}

/// Runs one allowlisted argv. The pass bounds every call with a timeout and
/// drops the future when it fires, so an implementation must not outlive that.
#[async_trait::async_trait]
pub(crate) trait VerifyRunner: Send + Sync {
    /// Run `program` with `args` in `cwd`.
    async fn run(&self, program: &str, args: &[String], cwd: &Path) -> RunOutcome;
}

/// The production runner: real processes, resolved on `PATH`.
///
/// What: `programs` maps a program name to the executable actually spawned;
/// the default maps each name to itself. Tests point it at a fixture script.
#[derive(Debug, Clone, Default)]
pub(crate) struct SystemRunner {
    /// `(name, executable)` overrides; a name not listed runs as itself.
    pub(crate) programs: Vec<(String, PathBuf)>,
}

impl SystemRunner {
    /// The executable `program` resolves to.
    fn executable(&self, program: &str) -> PathBuf {
        self.programs
            .iter()
            .find(|(name, _)| name == program)
            .map_or_else(|| PathBuf::from(program), |(_, exe)| exe.clone())
    }
}

#[async_trait::async_trait]
impl VerifyRunner for SystemRunner {
    async fn run(&self, program: &str, args: &[String], cwd: &Path) -> RunOutcome {
        let mut command = tokio::process::Command::new(self.executable(program));
        command
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            // #9390: the pass's timeout drops this future; the child dies with it.
            .kill_on_drop(true);
        match command.output().await {
            Ok(output) => {
                let stdout = &output.stdout[..output.stdout.len().min(MAX_STDOUT_BYTES)];
                RunOutcome::Exited {
                    code: output.status.code(),
                    stdout: String::from_utf8_lossy(stdout).into_owned(),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => RunOutcome::Missing,
            Err(e) => RunOutcome::SpawnFailed(e.to_string()),
        }
    }
}

/// A scripted [`VerifyRunner`] for tests: nothing is spawned.
#[cfg(test)]
pub(crate) mod fake {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use super::{RunOutcome, VerifyRunner};

    /// What the script answers for one call: an outcome, or `None` to hang
    /// until the caller's timeout drops the future.
    type Script = Box<dyn Fn(&str, &[String]) -> Option<RunOutcome> + Send + Sync>;

    /// Every `(program, args, cwd)` a [`FakeRunner`] was asked to run.
    pub(crate) type Calls = Vec<(String, Vec<String>, PathBuf)>;

    /// Answers each call from a script and records it.
    pub(crate) struct FakeRunner {
        script: Script,
        calls: Mutex<Calls>,
    }

    impl FakeRunner {
        /// A runner answering each call with `script`.
        pub(crate) fn new(
            script: impl Fn(&str, &[String]) -> Option<RunOutcome> + Send + Sync + 'static,
        ) -> Self {
            Self {
                script: Box::new(script),
                calls: Mutex::new(Vec::new()),
            }
        }

        /// A runner that fails the test if anything reaches it.
        pub(crate) fn never() -> Self {
            Self::new(|program, args| panic!("no command may run here: {program} {args:?}"))
        }

        /// The calls so far.
        pub(crate) fn calls(&self) -> Calls {
            self.calls.lock().map(|c| c.clone()).unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl VerifyRunner for FakeRunner {
        async fn run(&self, program: &str, args: &[String], cwd: &Path) -> RunOutcome {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push((program.to_string(), args.to_vec(), cwd.to_path_buf()));
            }
            match (self.script)(program, args) {
                Some(outcome) => outcome,
                None => std::future::pending().await,
            }
        }
    }
}
