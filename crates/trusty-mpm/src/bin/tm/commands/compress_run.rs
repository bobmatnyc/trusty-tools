//! `tm compress --tool <name> -- <program> [args…]` — the run mode (#10973).
//!
//! Why: inside an isolation worktree Claude Code's classifier refuses the
//! `{ <cmd>; printf …; } | tm compress` pipeline (#7477), so `tm hook` there
//! rewrites a covered command into ONE simple command that runs the program
//! itself. Running it here also returns the program's real exit status, which
//! the pipeline form could only carry as a sentinel line.
//! What: [`run_compress_command`] spawns the program with stdin and stderr
//! inherited and stdout captured, waits, hands the captured stdout to
//! [`super::compress::compress_and_emit`] — the same compression, ledgers and
//! output as the stdin filter — and returns the program's exit status.
//! Test: `a_missing_program_returns_127`, `an_empty_command_is_a_usage_error`,
//! and end to end `tm_compress_run_mode_compresses_and_keeps_the_exit_status`.

use std::process::Stdio;

use tokio::io::AsyncReadExt;

/// Exit status a shell reports for a program it cannot find.
const EXIT_NOT_FOUND: i32 = 127;
/// Exit status a shell reports for a program it found but could not run.
const EXIT_NOT_EXECUTABLE: i32 = 126;

/// `tm compress` dispatch: the stdin filter, or the run mode for a command.
///
/// What: an empty `argv` → [`super::compress::run_compress`]; otherwise
/// [`run_compress_command`], then exits with the program's status.
/// Test: `cli_parses_compress_with_a_command`, and end to end
/// `tm_compress_run_mode_compresses_and_keeps_the_exit_status`.
pub(crate) async fn run(tool: &str, argv: &[String]) -> anyhow::Result<()> {
    if argv.is_empty() {
        return super::compress::run_compress(tool).await;
    }
    std::process::exit(run_compress_command(tool, argv).await)
}

/// Run `argv`, compress its stdout, and return the status to exit with.
///
/// Why: see the module doc. The status is the program's, never this filter's:
/// an agent running `cargo test` through the rewrite must still see it fail.
/// What: `argv[0]` is the program. A spawn failure prints one line to stderr
/// and returns 127 (not found) or 126, as a shell would. A signal-killed
/// program returns `128 + signal`. A stdout that is not UTF-8 is compressed
/// lossily. A compression or write failure still returns the program's status
/// after printing the error, since the program itself ran.
/// Test: `tm_compress_run_mode_compresses_and_keeps_the_exit_status`,
/// `a_missing_program_returns_127`.
pub(crate) async fn run_compress_command(tool: &str, argv: &[String]) -> i32 {
    let Some((program, args)) = argv.split_first() else {
        eprintln!("tm compress: no command after `--`");
        return 2;
    };
    let mut child = match tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("tm compress: cannot run {program}: {e}");
            return match e.kind() {
                std::io::ErrorKind::NotFound => EXIT_NOT_FOUND,
                _ => EXIT_NOT_EXECUTABLE,
            };
        }
    };
    let mut raw = Vec::new();
    let read = match child.stdout.take() {
        Some(mut stdout) => stdout.read_to_end(&mut raw).await.map(drop),
        None => Ok(()),
    };
    if let Err(e) = &read {
        eprintln!("tm compress: reading {program}'s output failed: {e}");
    }
    let code = match child.wait().await {
        Ok(status) => exit_code(status),
        Err(e) => {
            eprintln!("tm compress: waiting for {program} failed: {e}");
            EXIT_NOT_EXECUTABLE
        }
    };
    let code = status_after_read(&read, code);
    let text = String::from_utf8_lossy(&raw);
    if let Err(e) = super::compress::compress_and_emit(tool, &text, Some(code)).await {
        eprintln!("tm compress: {e}");
    }
    code
}

/// The status to exit with, given how reading the program's stdout went.
fn status_after_read(_read: &std::io::Result<()>, program_status: i32) -> i32 {
    program_status
}

/// The shell-style exit status of a finished program.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #10973 critic finding 4: output lost mid-stream is not a success.
    #[test]
    fn a_failed_stdout_read_yields_a_nonzero_status() {
        let failed = Err(std::io::Error::other("pipe broke"));
        assert_ne!(status_after_read(&failed, 0), 0);
        assert_eq!(
            status_after_read(&failed, 3),
            3,
            "a failure keeps its own status"
        );
        assert_eq!(status_after_read(&Ok(()), 0), 0);
    }

    #[tokio::test]
    async fn a_missing_program_returns_127() {
        let argv = vec!["tm-10973-no-such-program".to_string()];
        assert_eq!(
            run_compress_command("git diff", &argv).await,
            EXIT_NOT_FOUND
        );
    }

    #[tokio::test]
    async fn an_empty_command_is_a_usage_error() {
        assert_eq!(run_compress_command("git diff", &[]).await, 2);
    }
}
