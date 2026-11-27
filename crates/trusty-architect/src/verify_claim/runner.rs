//! `default_runner`: one command, a 60 s bound, `(rc, stdout, stderr)` (#12028 P1g).
//!
//! Why: every gh, git and curl call of `verify-claim` goes through one
//! function, so the timeout and the per-call environment live in one place.
//! What: [`run`] starts `argv` through [`crate::process::run_bounded`] with
//! the script's 60 s timeout. `env` `Some` replaces the whole environment
//! (`subprocess.run(env=...)`), `None` inherits it. Output is decoded the way
//! `text=True` does (UTF-8, universal newlines). A start failure or the
//! timeout answers 127 with the exception's text, as the script does; a
//! signal-ended child answers `-signal`.
//! Known gap: undecodable output is replaced lossily, where `text=True`
//! raises `UnicodeDecodeError`.
//! Test: `shipped_verify_claim_strips_tokens_from_gh_children`, the
//! `tests/replay_verify_claim.rs` recordings.

use std::io::ErrorKind;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::time::Duration;

use crate::process::{RunFailure, run_bounded};
use crate::pystr::py_repr;
use crate::writers::universal_newlines;

/// `subprocess.run(..., timeout=60)`.
pub const TIMEOUT: Duration = Duration::from_secs(60);

fn decode(bytes: &[u8]) -> String {
    universal_newlines(String::from_utf8_lossy(bytes).into_owned())
}

fn spawn_error(e: &std::io::Error, program: &str) -> String {
    let (class, errno) = match e.kind() {
        ErrorKind::NotFound => ("FileNotFoundError", 2),
        ErrorKind::PermissionDenied => ("PermissionError", 13),
        _ => ("OSError", e.raw_os_error().unwrap_or(0)),
    };
    let msg = e.to_string();
    let msg = msg.split(" (os error").next().unwrap_or(&msg).to_owned();
    format!("{class}: [Errno {errno}] {msg}: {}", py_repr(program))
}

/// Runs `argv` and returns `(rc, stdout, stderr)`; never panics.
///
/// Why: the script's `default_runner`, the only way the port starts a child.
/// What: see the module doc.
/// Test: `shipped_verify_claim_strips_tokens_from_gh_children`.
pub fn run(argv: &[String], env: Option<&[(String, String)]>) -> (i32, String, String) {
    let Some((program, args)) = argv.split_first() else {
        return (
            127,
            String::new(),
            "IndexError: list index out of range".into(),
        );
    };
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(vars) = env {
        cmd.env_clear();
        cmd.envs(vars.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }
    match run_bounded(cmd, TIMEOUT) {
        Ok(done) => {
            let rc = done
                .status
                .code()
                .or_else(|| done.status.signal().map(|s| -s))
                .unwrap_or(-1);
            (rc, decode(&done.stdout), decode(&done.stderr))
        }
        Err(RunFailure::Spawn(e)) => (127, String::new(), spawn_error(&e, program)),
        Err(RunFailure::Timeout) => {
            let shown: Vec<String> = argv.iter().map(|a| py_repr(a)).collect();
            (
                127,
                String::new(),
                format!(
                    "TimeoutExpired: Command '[{}]' timed out after 60 seconds",
                    shown.join(", ")
                ),
            )
        }
        Err(RunFailure::Wait(e)) => (127, String::new(), format!("OSError: {e}")),
    }
}
