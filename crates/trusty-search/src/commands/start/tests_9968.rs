//! #9968: the eager `TRUSTY_EMBEDDER=python` path looks up the launcher
//! before it builds the venv.
//!
//! Why: the venv build downloads ~3 GB of torch, and without the launcher
//! that venv is never used.
//! What: drives `prepare_eager_python` with fake `locate` / `ensure_venv`
//! closures and records whether the venv step ran.
//! Test: the tests below.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::embedder::{prepare_eager_python, EagerPythonFailure};

/// A missing launcher must end the eager path before any venv work.
///
/// Mutation caught: calling `ensure_venv` before `locate` (the pre-#9968
/// order) runs the venv step once and fails the `venv_calls == 0` assert.
#[tokio::test]
async fn eager_python_missing_launcher_builds_no_venv() {
    let venv_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&venv_calls);

    let outcome = prepare_eager_python(
        || Err(anyhow::anyhow!("no trusty-embedderd-py launcher")),
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    )
    .await;

    assert!(
        matches!(&outcome, Err(EagerPythonFailure::Launcher(e)) if e.to_string().contains("launcher")),
        "a missing launcher is reported as a launcher failure: {outcome:?}"
    );
    assert_eq!(
        venv_calls.load(Ordering::SeqCst),
        0,
        "no venv work may run without a launcher"
    );
}

/// With the launcher found, a venv failure is still reported as one.
///
/// Mutation caught: dropping the venv step, or classing its error as a
/// launcher failure, fails the `Venv` match or the `venv_calls == 1` assert.
#[tokio::test]
async fn eager_python_venv_failure_is_reported_after_the_launcher_lookup() {
    let venv_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&venv_calls);

    let outcome = prepare_eager_python(
        || Ok(PathBuf::from("/opt/bin/trusty-embedderd-py")),
        move || -> anyhow::Result<()> {
            counter.fetch_add(1, Ordering::SeqCst);
            Err(anyhow::anyhow!("uv sync failed"))
        },
    )
    .await;

    assert!(
        matches!(&outcome, Err(EagerPythonFailure::Venv(e)) if format!("{e:#}").contains("uv sync failed")),
        "a venv error is reported as a venv failure: {outcome:?}"
    );
    assert_eq!(venv_calls.load(Ordering::SeqCst), 1);
}
