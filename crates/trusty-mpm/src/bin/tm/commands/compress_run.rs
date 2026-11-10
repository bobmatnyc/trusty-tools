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
    if let Some(mut stdout) = child.stdout.take()
        && let Err(e) = stdout.read_to_end(&mut raw).await
    {
        eprintln!("tm compress: reading {program}'s output failed: {e}");
    }
    let code = match child.wait().await {
        Ok(status) => exit_code(status),
        Err(e) => {
            eprintln!("tm compress: waiting for {program} failed: {e}");
            EXIT_NOT_EXECUTABLE
        }
    };
    let text = String::from_utf8_lossy(&raw);
    if let Err(e) = super::compress::compress_and_emit(tool, &text, Some(code)).await {
        eprintln!("tm compress: {e}");
    }
    code
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
