//! Typed venv-build failures and whether a retry can help (#9971).
//!
//! Why: trusty-search's background bootstrap loop (#9949) retries a transient
//! failure and stops on a permanent one. A network drop during the first
//! ~3 GB torch install was a permanent failure, so one bad connection kept
//! the daemon on ort until a restart. The caller must decide from a type,
//! never from the error text.
//! What: [`VenvBuildError`] for the build's own failures, and
//! [`failure_kind`], which reads the first typed cause in an `anyhow` chain:
//! a [`VenvBuildError`], a [`UvError`] or an [`io::Error`]. The smoke test
//! reports a network failure during its model download with
//! [`SMOKE_NETWORK_EXIT`].
//! Test: `venv_error_tests.rs`.

use std::io;
use std::path::{Path, PathBuf};

use crate::uv_fetch::UvError;

/// Approximate free space a venv build needs (torch is ~2-3 GB).
pub(crate) const REQUIRED_FREE_BYTES: u64 = 3 * 1024 * 1024 * 1024;

/// The smoke test's exit code for a network failure while it downloads the
/// model (`EX_TEMPFAIL`, #9971).
pub(crate) const SMOKE_NETWORK_EXIT: i32 = 75;

/// Whether a later attempt can succeed where this one failed (#9971).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// A network failure or timeout: a later attempt may pass.
    Transient,
    /// Retrying repeats the same failure: low disk, no uv, a broken build.
    Permanent,
}

/// A venv-build failure, typed for the retry decision (#9971).
///
/// Why: see the module doc. What: [`kind`](Self::kind) maps each variant
/// to a [`FailureKind`]. Test: `each_variant_maps_to_its_kind`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VenvBuildError {
    /// Too little free space to build, before the build or after a failed
    /// download step. Permanent: the next attempt's precheck refuses too.
    #[error(
        "insufficient disk space for the Python/MPS sidecar venv: {} free at {}, need ~{} GB \
         (torch + sentence-transformers)",
        human_bytes(*free_bytes),
        path.display(),
        REQUIRED_FREE_BYTES / (1024 * 1024 * 1024)
    )]
    InsufficientDisk { path: PathBuf, free_bytes: u64 },
    /// A step outran `TRUSTY_PY_BOOTSTRAP_TIMEOUT_SECS`. Transient.
    #[error("{step} timed out after {secs}s (TRUSTY_PY_BOOTSTRAP_TIMEOUT_SECS)")]
    Timeout { step: String, secs: u64 },
    /// A downloading uv step exited non-zero with disk space to spare: the
    /// network is the likely cause. Transient.
    #[error("{step} exited with {status} on a download step (the network is the likely cause)")]
    DownloadFailed { step: String, status: String },
    /// A local uv step exited non-zero. Permanent.
    #[error("{step} exited with {status}")]
    StepFailed { step: String, status: String },
}

impl VenvBuildError {
    /// Whether a retry can help. See each variant's doc.
    pub fn kind(&self) -> FailureKind {
        match self {
            Self::Timeout { .. } | Self::DownloadFailed { .. } => FailureKind::Transient,
            Self::InsufficientDisk { .. } | Self::StepFailed { .. } => FailureKind::Permanent,
        }
    }
}

/// Classify a venv bootstrap error for the retry loop (#9971).
///
/// Why: trusty-search must not match error strings to decide a retry.
/// What: the first typed cause in `err`'s chain decides. `UvError::Network`
/// is transient; every other `UvError` (the `TRUSTY_UV_FETCH=0` opt-out
/// included) and every `io::Error` (`ENOSPC` included) is permanent, and so
/// is an error with no typed cause.
/// Test: `failure_kind_reads_the_first_typed_cause`.
pub fn failure_kind(err: &anyhow::Error) -> FailureKind {
    for cause in err.chain() {
        if let Some(e) = cause.downcast_ref::<VenvBuildError>() {
            return e.kind();
        }
        if let Some(e) = cause.downcast_ref::<UvError>() {
            return match e {
                UvError::Network { .. } => FailureKind::Transient,
                _ => FailureKind::Permanent,
            };
        }
        if cause.downcast_ref::<io::Error>().is_some() {
            return FailureKind::Permanent;
        }
    }
    FailureKind::Permanent
}

/// The error for a uv step that exited non-zero (#9971).
///
/// What: `python install` and `pip sync` download; one that fails with less
/// than [`REQUIRED_FREE_BYTES`] free at `dir` is `InsufficientDisk` (uv's
/// own `ENOSPC` is not visible here), else `DownloadFailed`. A local step
/// is `StepFailed`.
/// Test: `a_failed_download_step_is_classified_by_free_space`.
pub(crate) fn step_exit(args: &[&str], status: String, dir: &Path) -> VenvBuildError {
    let free = fs4::available_space(dir).ok();
    step_exit_with(args, status, dir, free)
}

/// [`step_exit`] with the free-space reading injected.
pub(crate) fn step_exit_with(
    args: &[&str],
    status: String,
    dir: &Path,
    free: Option<u64>,
) -> VenvBuildError {
    let step = format!("uv {}", args.join(" "));
    if matches!(args, ["python", "install", ..] | ["pip", "sync", ..]) {
        download_exit_with(step, status, dir, free)
    } else {
        VenvBuildError::StepFailed { step, status }
    }
}

/// A failed download: `InsufficientDisk` below [`REQUIRED_FREE_BYTES`] free
/// at `dir`, else the transient `DownloadFailed`.
fn download_exit_with(
    step: String,
    status: String,
    dir: &Path,
    free: Option<u64>,
) -> VenvBuildError {
    match free {
        Some(free_bytes) if free_bytes < REQUIRED_FREE_BYTES => VenvBuildError::InsufficientDisk {
            path: dir.to_path_buf(),
            free_bytes,
        },
        _ => VenvBuildError::DownloadFailed { step, status },
    }
}

/// The error for a smoke test that exited non-zero (#9971).
///
/// What: [`SMOKE_NETWORK_EXIT`] means the model download failed on the
/// network, classified like a failed uv download step (free space at `dir`
/// decides). Any other exit, or a signal (`code` is `None`), stays the
/// untyped, permanent smoke-test failure.
/// Test: `a_smoke_test_network_exit_is_classified_by_free_space`.
pub(crate) fn smoke_exit_with(
    code: Option<i32>,
    status: String,
    dir: &Path,
    free: Option<u64>,
) -> anyhow::Error {
    if code == Some(SMOKE_NETWORK_EXIT) {
        let step = "py-embedder smoke test (model download)".to_owned();
        return download_exit_with(step, status, dir, free).into();
    }
    anyhow::anyhow!("py-embedder smoke test failed: {status}")
}

pub(crate) fn human_bytes(n: u64) -> String {
    format!("{:.1} GB", n as f64 / (1024.0 * 1024.0 * 1024.0))
}

#[cfg(test)]
#[path = "venv_error_tests.rs"]
mod tests;
