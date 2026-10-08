//! `gchat-mcp` command-line parsing and project-dir resolution.
//!
//! Why: the binary serves one project, chosen by flag, then environment,
//! then cwd (#9448 ruling); keeping the parsing here makes it testable
//! without spawning the binary.
//! What: [`parse_args`] reads `gchat-mcp [--project-dir D]
//! [--poll-interval-secs N]` and `gchat-mcp doctor [--project-dir D]
//! [--offline]`; [`resolve_project_dir`] applies the precedence.
//! Test: `parse_args_reads_serve_and_doctor`,
//! `project_dir_prefers_flag_then_env_then_cwd`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use crate::gchat::poller::DEFAULT_POLL_INTERVAL;

/// The environment variable naming the project dir when no flag does.
pub const PROJECT_DIR_ENV: &str = "TRUSTY_CHANNELS_PROJECT_DIR";

/// Usage text, printed to stderr on a usage error.
pub const USAGE: &str = "usage:\n  gchat-mcp [--project-dir DIR] [--poll-interval-secs N]\n  \
gchat-mcp doctor [--project-dir DIR] [--offline]\n\nThe project dir defaults to \
$TRUSTY_CHANNELS_PROJECT_DIR, then the current directory.";

/// A parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Serve MCP on stdio.
    Serve {
        /// `--project-dir`, if given.
        project_dir: Option<PathBuf>,
        /// The poller's tick period.
        poll_interval: Duration,
    },
    /// Print the doctor rows and exit.
    Doctor {
        /// `--project-dir`, if given.
        project_dir: Option<PathBuf>,
        /// Skip the token mint.
        offline: bool,
    },
    /// `--help` / `-h`.
    Help,
}

/// A command line that does not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct UsageError(pub String);

/// Parse the arguments after the program name.
///
/// Why: two modes, three flags; no parser dependency is needed.
/// What: a leading `doctor` selects doctor mode; `--flag value` and
/// `--flag=value` both work. Unknown flags, a missing value, `--offline`
/// outside doctor, or a non-positive interval are usage errors.
/// Test: `parse_args_reads_serve_and_doctor`.
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Command, UsageError> {
    let mut args = args.into_iter().peekable();
    let doctor = args.next_if(|a| a == "doctor").is_some();
    let mut project_dir = None;
    let mut offline = false;
    let mut poll_interval = DEFAULT_POLL_INTERVAL;
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| {
            inline
                .clone()
                .or_else(|| args.next())
                .ok_or_else(|| UsageError(format!("{name} needs a value")))
        };
        match flag.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--project-dir" => project_dir = Some(PathBuf::from(value("--project-dir")?)),
            "--offline" if doctor && inline.is_none() => offline = true,
            "--poll-interval-secs" if !doctor => {
                let raw = value("--poll-interval-secs")?;
                let secs: u64 =
                    raw.parse().ok().filter(|s| *s > 0).ok_or_else(|| {
                        UsageError(format!("invalid --poll-interval-secs {raw:?}"))
                    })?;
                poll_interval = Duration::from_secs(secs);
            }
            other => return Err(UsageError(format!("unexpected argument {other:?}"))),
        }
    }
    Ok(if doctor {
        Command::Doctor {
            project_dir,
            offline,
        }
    } else {
        Command::Serve {
            project_dir,
            poll_interval,
        }
    })
}

/// The project dir: `flag`, else a non-empty `env`, else `cwd()`.
///
/// Why: the #9448 precedence; inputs are parameters so the test sets no
/// process-global environment.
/// What: returns the first source present, unmodified.
/// Test: `project_dir_prefers_flag_then_env_then_cwd`.
pub fn resolve_project_dir(
    flag: Option<PathBuf>,
    env: Option<OsString>,
    cwd: impl FnOnce() -> std::io::Result<PathBuf>,
) -> std::io::Result<PathBuf> {
    if let Some(dir) = flag {
        return Ok(dir);
    }
    if let Some(dir) = env.filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    cwd()
}
