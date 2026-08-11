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
         nothing is lost — render it with `{binary} report --manifest {manifest} --analyze \
         --synthesize --out <dir>` once the binary is available."
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
    ///
    /// This is the process's exit status and nothing more. It does NOT mean a
    /// synthesis pass happened — a pre-0.15 renderer degrades to a
    /// narrative-free report and still exits 0. Use
    /// [`require_rendered_report_carries_synthesis`] to check that (#5454).
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
/// [`DEFAULT_REVIEW_BIN`] resolved on PATH by the OS. Reading the variable is
/// all this function does; the rule itself lives in [`binary_from_override`] so
/// it can be tested without mutating the process environment.
/// Test: `super::tests::binary_resolution_prefers_the_env_override`.
pub fn resolve_review_binary() -> String {
    binary_from_override(std::env::var(ENV_REVIEW_BIN).ok().as_deref())
}

/// The resolution rule itself: an override wins unless it is empty.
///
/// Why: taking the override as a parameter keeps the rule a pure function, so
/// the tests never call `std::env::set_var`. That call is `unsafe` in edition
/// 2024 because another thread reading the environment concurrently is UB, and
/// `cargo test` runs tests in parallel — a test-only guarantee of
/// single-threadedness does not exist (#5308 review).
/// What: `None` and `Some("")` both fall back to [`DEFAULT_REVIEW_BIN`].
/// Test: `super::tests::binary_resolution_prefers_the_env_override`.
pub(super) fn binary_from_override(override_value: Option<&str>) -> String {
    override_value
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_REVIEW_BIN)
        .to_string()
}

/// Render `manifest` into `out_dir` by invoking `trusty-review report`.
///
/// Why: the last step of DOC-67 §6's orchestration, and the only one that
/// produces the deliverable. It runs on the blocking pool because the child can
/// take minutes on a large repository set and must not occupy an async worker.
/// What: spawns `<binary> report --manifest <manifest> --analyze --synthesize
/// --out <out_dir>`, captures both streams and the exit status, and parses the
/// artifact paths from stdout. A non-zero exit returns `Ok` with
/// [`ReviewRun::success`] false — only a failure to *start* the child is an
/// `Err`, because that is the one case where the caller has nothing to report.
///
/// The `--analyze` flag is always passed: an AUDIT report without the live
/// analysis pass has no findings, no complexity distribution, and no health
/// factors (DOC-67 §8), and its absence is reported as a gap by trusty-review
/// rather than being silently accepted here.
///
/// `--synthesize` is always passed too (#5454). trusty-review 0.15 makes
/// synthesis unconditional and treats the flag as a deprecated no-op, but tga
/// resolves that binary from PATH rather than from a Cargo edge (DOC-67 §5), so
/// the installed copy may predate this change — on an older one the flag is the
/// only thing that turns inference on at all.
/// [`require_review_supports_required_inference`] normally rejects such a copy
/// before the sweep starts; the flag still matters for the copy whose version it
/// could not read.
/// Test: `super::tests::{missing_binary_is_a_named_actionable_error,
/// artifact_paths_are_parsed_from_stdout, invocation_requests_inference}`.
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
    run_review_report_with(resolve_review_binary(), manifest, out_dir).await
}

/// [`run_review_report`] with the binary already resolved.
///
/// Why: the environment is read exactly once, at the public entry point, so a
/// test can drive the whole spawn-and-map path at a binary that certainly does
/// not exist without touching the process environment (#5308 review).
/// What: everything `run_review_report` does apart from resolution.
/// Test: `super::tests::missing_binary_is_a_named_actionable_error`.
pub(super) async fn run_review_report_with(
    binary: String,
    manifest: &Path,
    out_dir: &Path,
) -> Result<ReviewRun, ReviewRunError> {
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
        .args(report_args(manifest, out_dir))
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

/// The exact argument vector [`invoke`] hands `trusty-review`.
///
/// Why: this list IS the tga→trusty-review contract, and the website documents it
/// verbatim as the by-hand recovery command. Building it in a pure function is
/// what lets a test assert its contents without spawning anything — the same
/// reason [`binary_from_override`] takes its input as a parameter.
/// What: `report --manifest <m> --analyze --synthesize --out <dir>`.
/// Test: `super::tests::invocation_requests_inference`.
pub(super) fn report_args(manifest: &Path, out_dir: &Path) -> Vec<std::ffi::OsString> {
    vec![
        "report".into(),
        "--manifest".into(),
        manifest.as_os_str().to_owned(),
        "--analyze".into(),
        // #5454: inference is required for a DD report.
        "--synthesize".into(),
        "--out".into(),
        out_dir.as_os_str().to_owned(),
    ]
}

/// Environment variable carrying the credential the renderer's inference needs.
///
/// Why: named through `trusty_common` so tga and trusty-review cannot drift on
/// the spelling — trusty-review reads the same constant to build its provider.
pub const ENV_INFERENCE_CREDENTIAL: &str = trusty_common::env_vars::ENV_OPENROUTER_API_KEY;

/// The audit cannot start without an inference credential.
///
/// Why: #5454 made the DD report's narrative required, and DOC-67 §2 gives the
/// sweep exactly one non-interactive shot. Those two together decide WHERE this
/// check goes: a sweep can run for many minutes over an org, and discovering an
/// unset key at the render step — the last thing it does — throws all of it away
/// for a fault that was knowable before stage 1.
/// What: a typed error naming the variable and how to set it. Only OpenRouter is
/// checked, per the #5454 owner decision that it is the audit's only inference
/// path. The value is tested for emptiness and never read into a message.
/// Test: `super::tests::{absent_credential_is_a_named_actionable_error,
/// present_credential_passes_the_precheck}`.
#[derive(Debug, thiserror::Error)]
#[error(
    "{ENV_INFERENCE_CREDENTIAL} is not set. `tga audit` renders its report with \
     trusty-review, whose analysis requires inference, so the sweep would collect \
     for minutes and then fail at the last step. Set it first:\n\n    export \
     {ENV_INFERENCE_CREDENTIAL}=<your OpenRouter API key>\n\nKeys are issued at \
     https://openrouter.ai/keys."
)]
#[non_exhaustive]
pub struct MissingInferenceCredential;

/// Check that the inference credential is present, before the sweep starts.
///
/// Why/What: reads [`ENV_INFERENCE_CREDENTIAL`] and applies
/// [`credential_is_present`]. Reading the variable is all this does; the rule
/// lives in that pure function so tests never call `std::env::set_var`, which is
/// `unsafe` in edition 2024 and unsound under parallel tests (#5308 review).
///
/// # Errors
///
/// [`MissingInferenceCredential`] when the variable is unset or blank.
///
/// Test: `super::tests::absent_credential_is_a_named_actionable_error`.
pub fn require_inference_credential() -> Result<(), MissingInferenceCredential> {
    if credential_is_present(std::env::var(ENV_INFERENCE_CREDENTIAL).ok().as_deref()) {
        Ok(())
    } else {
        Err(MissingInferenceCredential)
    }
}

/// The presence rule itself: set, and not only whitespace.
///
/// Why: a variable exported as an empty string is the shape a half-finished
/// shell profile leaves behind, and it fails at the provider exactly as an unset
/// one does — so the preflight must treat them the same.
/// What: `None`, `Some("")` and `Some("  ")` are all absent.
/// Test: `super::tests::present_credential_passes_the_precheck`.
pub(super) fn credential_is_present(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.trim().is_empty())
}

/// The oldest `trusty-review` whose report is guaranteed to carry inference.
///
/// Why: 0.15.0 is the release that made synthesis unconditional and turned every
/// degrade path into a hard error (#5454). A 0.14 renderer accepts
/// `--synthesize`, falls back to a deterministic, narrative-free report on any
/// provider failure, and still exits 0.
pub const MIN_REVIEW_VERSION: (u64, u64, u64) = (0, 15, 0);

/// The installed renderer predates required inference.
///
/// Why: tga resolves `trusty-review` from PATH, not from a Cargo edge (DOC-67
/// §5), so the two versions are installed and upgraded separately and this
/// pairing is ordinary rather than exotic. The remedy is one command, and the
/// message has to say which command — "your report has no narrative" is a
/// symptom the operator cannot act on.
/// What: the binary that was asked, the version it reported, and the floor.
/// Test: `super::tests::stale_renderer_is_rejected_before_the_sweep`.
#[derive(Debug, thiserror::Error)]
#[error(
    "`{binary}` reports version {found}, and `tga audit` needs {major}.{minor}.{patch} or newer. \
     A renderer older than that accepts `--synthesize` but falls back to a deterministic, \
     narrative-free report whenever the model call fails — and still exits 0, so the audit would \
     report success over a report with no written analysis. Upgrade it before running the \
     audit:\n\n    tctl install trusty-review\n\nOr set TRUSTY_REVIEW_BIN to a newer copy.",
    major = MIN_REVIEW_VERSION.0,
    minor = MIN_REVIEW_VERSION.1,
    patch = MIN_REVIEW_VERSION.2,
)]
#[non_exhaustive]
pub struct ReviewBinaryTooOld {
    /// The binary name or path that was asked for its version.
    pub binary: String,
    /// The version string it reported.
    pub found: String,
}

/// Reject a pre-0.15 renderer before the sweep starts.
///
/// Why: the version skew is the second whole-run precondition that is knowable
/// up front, alongside the credential — and DOC-67 §2 gives this command one
/// non-interactive shot, so an operator who learns about it only after eight
/// stages have run learns it at the worst possible moment. The check itself is
/// one process spawn that returns in milliseconds.
/// What: runs `<binary> --version` and compares against [`MIN_REVIEW_VERSION`].
/// A binary that cannot be spawned, exits non-zero, or prints something this
/// cannot parse is ALLOWED THROUGH deliberately: the not-installed case already
/// has a better-worded error at the render step, and a renderer whose version
/// cannot be read is still caught by
/// [`require_rendered_report_carries_synthesis`], which checks the delivered
/// artifact rather than a claim about it. This narrows the window early; it is
/// not the thing that closes it.
///
/// # Errors
///
/// [`ReviewBinaryTooOld`] when the binary reports a version below the floor.
///
/// Test: `super::tests::stale_renderer_is_rejected_before_the_sweep`.
pub fn require_review_supports_required_inference() -> Result<(), ReviewBinaryTooOld> {
    let binary = resolve_review_binary();
    let Ok(output) = Command::new(&binary).arg("--version").output() else {
        return Ok(());
    };
    if !output.status.success() {
        return Ok(());
    }
    // clap writes `--version` to stdout.
    version_verdict(&binary, &String::from_utf8_lossy(&output.stdout))
}

/// The version rule itself: reject only what is definitely below the floor.
///
/// Why: taking the reported text as a parameter keeps the decision — not merely
/// the parse — testable without a binary to spawn, the same split
/// [`binary_from_override`] and [`credential_is_present`] use for their rules.
/// What: `Err` when the parsed version is below [`MIN_REVIEW_VERSION`]; `Ok` for
/// anything at or above it, and for output this cannot read at all.
/// Test: `super::tests::stale_renderer_is_rejected_before_the_sweep`.
pub(super) fn version_verdict(
    binary: &str,
    version_output: &str,
) -> Result<(), ReviewBinaryTooOld> {
    match parse_review_version(version_output) {
        Some(found) if found < MIN_REVIEW_VERSION => Err(ReviewBinaryTooOld {
            binary: binary.to_string(),
            found: format!("{}.{}.{}", found.0, found.1, found.2),
        }),
        _ => Ok(()),
    }
}

/// Read `major.minor.patch` out of a `--version` line.
///
/// Why: a pure function so the comparison is tested without a binary to spawn,
/// the same split [`binary_from_override`] and [`credential_is_present`] use.
/// What: takes the last whitespace-separated token of the first non-empty line
/// (`trusty-review 0.14.1` → `0.14.1`), drops a leading `v`, and reads the three
/// leading numeric components; a pre-release or build suffix is cut at the first
/// `-` or `+`. `None` for anything it cannot read, which the caller treats as
/// "proceed" rather than "too old".
/// Test: `super::tests::stale_renderer_is_rejected_before_the_sweep`.
pub(super) fn parse_review_version(version_output: &str) -> Option<(u64, u64, u64)> {
    let token = version_output
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())?
        .split_whitespace()
        .next_back()?
        .trim_start_matches('v');
    let core = token.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    Some((next()?, next()?, next()?))
}

/// The delivered report could not be shown to carry a written analysis.
///
/// Why: the child's exit status is not evidence of a synthesis pass, so the
/// artifact itself is what gets checked — and a check that cannot be performed
/// must fail rather than pass, or it re-opens the hole it was added to close.
/// What: `NoSynthesis` when the report JSON was read and carries no verified
/// narrative — the pre-0.15 degrade, whose remedy is an upgrade; `NotCheckable`
/// when the JSON was missing, unreadable, or not JSON, whose cause is local.
/// Test: `super::tests::{exit_zero_over_a_narrative_free_report_is_a_failure,
/// an_uncheckable_report_fails_rather_than_passes}`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UnverifiedReport {
    /// The report JSON was read, and carries no verified narrative.
    #[error(
        "{path} carries no written analysis: the report's executive summary, top-risk rationale \
         and finding prose were all produced deterministically, with no inference. The installed \
         `trusty-review` predates the change that made that inference required (#5454) — before \
         {major}.{minor}.{patch} it degraded to a narrative-free report whenever the model call \
         failed, and exited 0 regardless. Upgrade the renderer:\n\n    tctl install trusty-review",
        major = MIN_REVIEW_VERSION.0,
        minor = MIN_REVIEW_VERSION.1,
        patch = MIN_REVIEW_VERSION.2,
    )]
    NoSynthesis {
        /// The report JSON that was checked.
        path: PathBuf,
    },

    /// The report JSON could not be read, so nothing about it can be asserted.
    #[error(
        "the rendered report could not be checked for a written analysis, so this run cannot \
         claim to have produced one: {reason}"
    )]
    NotCheckable {
        /// What stopped the check.
        reason: String,
    },
}

/// Require that the report trusty-review just wrote carries a written analysis.
///
/// Why: `ReviewRun::success` is `output.status.success()` and nothing else, and
/// a pre-0.15 renderer exits 0 over a report whose narrative sections were never
/// written by a model. Checking the exit status alone is what let #5454's defect
/// survive in the mid-run-provider-failure arm — an audit that reports a clean
/// pass while delivering the very report the ticket exists to abolish.
/// What: finds the `.json` twin among the artifact paths the child printed and
/// requires its `synthesis` object to carry at least one verified field. That
/// is the invariant 0.15 guarantees (a pass with nothing left is
/// `SynthesisError::NoVerifiableContent`, which fails the render), so it holds
/// regardless of what version produced the file — including a future one whose
/// degrade shape nobody here anticipated.
///
/// # Errors
///
/// [`UnverifiedReport`] — see its variants.
///
/// Test: `super::tests::{exit_zero_over_a_narrative_free_report_is_a_failure,
/// a_synthesized_report_passes_the_check,
/// an_uncheckable_report_fails_rather_than_passes}`.
pub fn require_rendered_report_carries_synthesis(run: &ReviewRun) -> Result<(), UnverifiedReport> {
    let json = run
        .artifacts
        .iter()
        .find(|p| p.extension().is_some_and(|e| e == "json"))
        .ok_or_else(|| UnverifiedReport::NotCheckable {
            reason: "the renderer printed no `.json` report path".to_string(),
        })?;

    let text = std::fs::read_to_string(json).map_err(|source| UnverifiedReport::NotCheckable {
        reason: format!("could not read {}: {source}", json.display()),
    })?;

    match json_carries_synthesis(&text) {
        Some(true) => Ok(()),
        Some(false) => Err(UnverifiedReport::NoSynthesis { path: json.clone() }),
        None => Err(UnverifiedReport::NotCheckable {
            reason: format!("{} is not valid JSON", json.display()),
        }),
    }
}

/// The synthesis rule itself, over the report JSON's text.
///
/// Why: a pure function over a string, so the pre-0.15 degraded shape and the
/// 0.15 shape are both tested as literal fixtures rather than reconstructed from
/// whichever version of `Synthesis` happens to be linked in.
/// What: `Some(true)` when `synthesis` holds a non-blank `executive_summary`, a
/// non-empty `top_risks`, or a non-empty `findings`; `Some(false)` for valid
/// JSON without one — which covers 0.14's `"status": {"state": "unavailable"}`
/// shape, whose prose fields are all empty, and an absent `synthesis` key.
/// `None` when the text is not JSON at all.
/// Test: `super::tests::exit_zero_over_a_narrative_free_report_is_a_failure`.
pub(super) fn json_carries_synthesis(report_json: &str) -> Option<bool> {
    let value: serde_json::Value = serde_json::from_str(report_json).ok()?;
    let Some(synthesis) = value.get("synthesis").and_then(|s| s.as_object()) else {
        return Some(false);
    };
    let non_blank = |key: &str| {
        synthesis
            .get(key)
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty())
    };
    let non_empty = |key: &str| {
        synthesis
            .get(key)
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty())
    };
    Some(non_blank("executive_summary") || non_empty("top_risks") || non_empty("findings"))
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
