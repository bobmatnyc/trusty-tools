//! Streaming `git clone --progress` runner that emits byte/object percentages
//! as fine-grained provisioning detail (#2605).
//!
//! Why: the clone is the long pole of a large-repo spawn — minutes during which
//! the coarse `CloningRepo` stage alone shows no movement. Forcing
//! `git clone --progress` and parsing its stderr lets the daemon emit
//! `emit_with_detail(CloningRepo, "Receiving objects: 63%")` so the CLI spinner
//! shows real progress within the clone. Runs on the same task as the (already
//! scoped) provision, so the `provisioning_stage` task-local emitter is visible
//! to [`emit_with_detail`] without threading any handle through the backend.
//! What: [`clone_with_progress`] spawns a prepared `git` command with stderr
//! piped, splits the stream on `\r`/`\n` (git rewrites progress lines with a
//! carriage return), emits deduplicated percentage detail, and returns the exit
//! status plus the full captured stderr for the caller's error path.
//! [`parse_git_progress`] is the pure line parser, extracted so it is testable
//! without a subprocess.
//! Test: `parse_git_progress_extracts_known_phases`,
//! `parse_git_progress_ignores_non_progress_lines`.

use std::io::Read;
use std::process::{Command, Stdio};

use crate::core::provisioning_stage::{ProvisioningStage, emit_with_detail};

/// Outcome of a streamed clone: whether it succeeded and its full stderr.
///
/// Why: the caller formats a descriptive `ProvisionError` from the exit status
/// and the captured stderr, exactly as the previous `.output()` path did.
/// What: `success` (the child's exit status) and `stderr` (the complete stream,
/// progress lines included).
/// Test: exercised via the `#[ignore]` clone integration tests; the parse logic
/// is unit-tested directly.
pub(crate) struct CloneOutcome {
    /// Whether the git process exited successfully.
    pub success: bool,
    /// The complete captured stderr (used to build the error message on failure).
    pub stderr: String,
}

/// Parse a single git stderr line into a `"<Phase>: <pct>%"` detail string.
///
/// Why: only a handful of git progress phases carry a meaningful percentage;
/// isolating the match+parse keeps the streaming loop trivial and lets the
/// (drift-prone) parsing be unit-tested with plain fixtures.
/// What: strips an optional `remote: ` prefix, splits on the first `:`, and —
/// for a recognized progress phase — extracts the integer percent preceding the
/// first `%`. Returns `None` for any other line (banners, non-progress output).
/// Test: `parse_git_progress_extracts_known_phases`,
/// `parse_git_progress_ignores_non_progress_lines`.
pub(crate) fn parse_git_progress(line: &str) -> Option<String> {
    let line = line.trim();
    let line = line.strip_prefix("remote: ").unwrap_or(line);
    let (phase, rest) = line.split_once(':')?;
    let phase = phase.trim();
    if !matches!(
        phase,
        "Enumerating objects"
            | "Counting objects"
            | "Compressing objects"
            | "Receiving objects"
            | "Resolving deltas"
    ) {
        return None;
    }
    let pct_end = rest.find('%')?;
    let pct: u32 = rest[..pct_end].split_whitespace().last()?.parse().ok()?;
    Some(format!("{phase}: {pct}%"))
}

/// Run a prepared `git clone` command, streaming its progress as stage detail.
///
/// Why: replaces the backend's blocking `.output()` so the clone's byte/object
/// percentages reach the client live, while preserving the exact
/// success/stderr contract callers already depend on.
/// What: forces `stderr` to a pipe and `stdout` to null, spawns the child,
/// reads stderr to EOF splitting on `\r`/`\n`, emits deduplicated
/// [`parse_git_progress`] detail on the scoped `CloningRepo` stage, then waits
/// and returns a [`CloneOutcome`]. Callers must have already added `--progress`
/// to `cmd` (git suppresses progress on a non-TTY stderr otherwise).
/// Test: exercised via the `#[ignore]` clone integration tests; parsing by the
/// `tests` submodule.
pub(crate) fn clone_with_progress(mut cmd: Command) -> std::io::Result<CloneOutcome> {
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    // `take()` cannot be None here: we set `Stdio::piped()` just above.
    let mut stderr = match child.stderr.take() {
        Some(s) => s,
        None => {
            let status = child.wait()?;
            return Ok(CloneOutcome {
                success: status.success(),
                stderr: String::new(),
            });
        }
    };

    let mut all: Vec<u8> = Vec::new();
    let mut segment: Vec<u8> = Vec::new();
    let mut last_emit = String::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stderr.read(&mut buf)?;
        if n == 0 {
            break;
        }
        all.extend_from_slice(&buf[..n]);
        for &b in &buf[..n] {
            if b == b'\r' || b == b'\n' {
                emit_segment(&segment, &mut last_emit);
                segment.clear();
            } else {
                segment.push(b);
            }
        }
    }
    emit_segment(&segment, &mut last_emit);

    let status = child.wait()?;
    Ok(CloneOutcome {
        success: status.success(),
        stderr: String::from_utf8_lossy(&all).into_owned(),
    })
}

/// Emit one completed stderr segment as progress detail, deduplicating repeats.
///
/// Why: git rewrites a progress line many times per percent; emitting only when
/// the formatted `"<Phase>: <pct>%"` changes keeps the broadcast channel from
/// flooding while still tracking every percentage step.
/// What: parses `segment`; if it yields a detail distinct from `last_emit`,
/// emits it on the scoped `CloningRepo` stage and records it as the new last.
/// Test: covered transitively by the clone integration tests.
fn emit_segment(segment: &[u8], last_emit: &mut String) {
    if segment.is_empty() {
        return;
    }
    let line = String::from_utf8_lossy(segment);
    if let Some(detail) = parse_git_progress(&line)
        && detail != *last_emit
    {
        emit_with_detail(ProvisioningStage::CloningRepo, Some(&detail));
        *last_emit = detail;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_git_progress_extracts_known_phases() {
        assert_eq!(
            parse_git_progress("Receiving objects:  63% (12285/19500), 5.00 MiB | 2.5 MiB/s"),
            Some("Receiving objects: 63%".to_string())
        );
        assert_eq!(
            parse_git_progress("Resolving deltas: 100% (8000/8000), done."),
            Some("Resolving deltas: 100%".to_string())
        );
        // `remote:`-prefixed lines are normalized.
        assert_eq!(
            parse_git_progress("remote: Compressing objects:   7% (1/14)"),
            Some("Compressing objects: 7%".to_string())
        );
    }

    #[test]
    fn parse_git_progress_ignores_non_progress_lines() {
        assert!(parse_git_progress("Cloning into bare repository 'repo.git'...").is_none());
        assert!(parse_git_progress("remote: Total 19500 (delta 0), reused 0").is_none());
        assert!(parse_git_progress("").is_none());
        // A recognized phase with no percentage yet.
        assert!(parse_git_progress("Receiving objects: (no percent here)").is_none());
    }
}
