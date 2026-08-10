//! Invoking `trusty-review report` as a subprocess (#5238, DOC-67 §6 step 3).
//!
//! Why: tga and trusty-review meet at a file, not at a Cargo edge (DOC-67 §5) —
//! which leaves someone to actually run the renderer. trusty-review already
//! treats a sibling trusty-* binary as an invocable subprocess
//! (`SubprocessAnalyzeClient`, closing #632); AUDIT follows that same house
//! pattern in the other direction rather than inventing a second idiom or
//! taking a dependency edge that would risk an import cycle.
//! What: [`resolve_review_binary`], [`run_review_report`], and the
//! [`ReviewRun`] record of one invocation. The binary is `TRUSTY_REVIEW_BIN`
//! when set, else `trusty-review` on PATH — the same override-then-PATH
//! resolution `SubprocessAnalyzeClient` uses for `trusty-analyze`.
//! Test: `super::tests`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Environment variable that overrides the `trusty-review` binary path.
///
/// Why: lets an operator or a test pin the exact binary without touching PATH,
/// matching `TRUSTY_ANALYZE_BIN`'s role on the trusty-review side.
pub const ENV_REVIEW_BIN: &str = "TRUSTY_REVIEW_BIN";

/// Default binary name searched on PATH.
pub const DEFAULT_REVIEW_BIN: &str = "trusty-review";

/// Failures invoking the renderer.
///
/// Why: a library module, so a typed error. The distinction that matters to an
/// operator is "you have not installed the renderer" versus "the renderer ran
/// and failed" — the first is a one-line fix, the second needs the child's own
/// output, which the caller has already printed.
/// What: binary-not-found (carrying the remediation), any other spawn failure,
/// and a join failure from the blocking pool.
/// Test: `super::tests::missing_binary_is_a_named_actionable_error`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReviewRunError {
    /// The binary is not installed, or not where the override points.
    #[error(
        "`{binary}` was not found on PATH. Install it (`cargo install trusty-review`) or set \
         TRUSTY_REVIEW_BIN to its full path. The manifest is already written to {manifest}, so \
         nothing is lost — render it with `{binary} report --manifest {manifest} --analyze --out \
         <dir>` once the binary is available."
    )]
    BinaryNotFound {
        /// The binary name or path that was tried.
        binary: String,
        /// The manifest that was written and can still be rendered by hand.
        manifest: PathBuf,
    },

    /// The process could not be started for some other reason.
    #[error("failed to run `{binary}`: {source}")]
    Spawn {
        /// The binary name or path that was tried.
        binary: String,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The blocking task carrying the child process did not join.
    #[error("the `{binary}` subprocess task did not complete: {message}")]
    Join {
        /// The binary name or path that was tried.
        binary: String,
        /// The join error, rendered.
        message: String,
    },
}

/// The result of one `trusty-review report` invocation.
///
/// Why: DOC-67 §6 step 4 requires the child's exit code and both streams to
/// reach the operator, and step 5 requires the artifact paths — so the whole
/// invocation is one value the caller reports on, rather than side effects the
/// caller has to reconstruct.
/// What: the artifact paths trusty-review printed to stdout, both captured
/// streams, and the exit status. A non-zero exit is recorded here, not raised —
/// the caller decides what a failed render means for the run.
/// Test: `super::tests::artifact_paths_are_parsed_from_stdout`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ReviewRun {
    /// Whether the child exited successfully.
    pub success: bool,
    /// The child's exit code, or `None` if it was killed by a signal.
    pub code: Option<i32>,
    /// Captured stdout — trusty-review prints one written path per line.
    pub stdout: String,
    /// Captured stderr — trusty-review's progress and warning lines.
    pub stderr: String,
    /// The artifact paths parsed from stdout, in the order printed.
    pub artifacts: Vec<PathBuf>,
}

/// The `trusty-review` binary this process will invoke.
///
/// Why/What: `TRUSTY_REVIEW_BIN` when set to a non-empty value, else
/// [`DEFAULT_REVIEW_BIN`] resolved on PATH by the OS.
/// Test: `super::tests::binary_resolution_prefers_the_env_override`.
pub fn resolve_review_binary() -> String {
    std::env::var(ENV_REVIEW_BIN)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_REVIEW_BIN.to_string())
}

/// Render `manifest` into `out_dir` by invoking `trusty-review report`.
///
/// Why: the last step of DOC-67 §6's orchestration, and the only one that
/// produces the deliverable. It runs on the blocking pool because the child can
/// take minutes on a large repository set and must not occupy an async worker.
/// What: spawns `<binary> report --manifest <manifest> --analyze --out
/// <out_dir>`, captures both streams and the exit status, and parses the
/// artifact paths from stdout. A non-zero exit returns `Ok` with
/// [`ReviewRun::success`] false — only a failure to *start* the child is an
/// `Err`, because that is the one case where the caller has nothing to report.
///
/// The `--analyze` flag is always passed: an AUDIT report without the live
/// analysis pass has no findings, no complexity distribution, and no health
/// factors (DOC-67 §8), and its absence is reported as a gap by trusty-review
/// rather than being silently accepted here.
/// Test: `super::tests::{missing_binary_is_a_named_actionable_error,
/// artifact_paths_are_parsed_from_stdout}`.
///
/// # Errors
///
/// [`ReviewRunError::BinaryNotFound`] when the binary is not installed,
/// [`ReviewRunError::Spawn`] for any other start failure, and
/// [`ReviewRunError::Join`] if the blocking task is cancelled.
pub async fn run_review_report(
    manifest: &Path,
    out_dir: &Path,
) -> Result<ReviewRun, ReviewRunError> {
    let binary = resolve_review_binary();
    let (bin, manifest_owned, out_owned) = (
        binary.clone(),
        manifest.to_path_buf(),
        out_dir.to_path_buf(),
    );

    tokio::task::spawn_blocking(move || invoke(&bin, &manifest_owned, &out_owned))
        .await
        .map_err(|e| ReviewRunError::Join {
            binary,
            message: e.to_string(),
        })?
}

/// The synchronous half of [`run_review_report`].
///
/// Why: isolated so it carries no async context into the blocking pool, the
/// same split `SubprocessAnalyzeClient` uses.
/// What: builds the command, runs it to completion, maps `NotFound` onto the
/// actionable error, and parses stdout.
/// Test: exercised through [`run_review_report`].
fn invoke(binary: &str, manifest: &Path, out_dir: &Path) -> Result<ReviewRun, ReviewRunError> {
    let output = Command::new(binary)
        .arg("report")
        .arg("--manifest")
        .arg(manifest)
        .arg("--analyze")
        .arg("--out")
        .arg(out_dir)
        .output()
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                ReviewRunError::BinaryNotFound {
                    binary: binary.to_string(),
                    manifest: manifest.to_path_buf(),
                }
            } else {
                ReviewRunError::Spawn {
                    binary: binary.to_string(),
                    source,
                }
            }
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    Ok(ReviewRun {
        success: output.status.success(),
        code: output.status.code(),
        artifacts: artifact_paths(&stdout),
        stdout,
        stderr,
    })
}

/// Parse the written-artifact paths from trusty-review's stdout.
///
/// Why: trusty-review prints its progress to stderr and the written paths to
/// stdout precisely so a caller can consume them; DOC-67 §6 step 5 is that
/// caller.
/// What: every non-blank stdout line, trimmed, in order.
/// Test: `super::tests::artifact_paths_are_parsed_from_stdout`.
pub fn artifact_paths(stdout: &str) -> Vec<PathBuf> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect()
}
