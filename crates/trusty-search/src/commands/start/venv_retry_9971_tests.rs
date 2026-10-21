//! #9971: a venv-build network failure retries; a full disk does not.
//!
//! Why: a network drop during the first ~3 GB torch install ended the
//! bootstrap as `failed`, pinning ort until a restart.
//! What: the `tests_9949` fakes with a typed `ensure_venv` error.
//! Test: the tests below.

use super::*;

/// The torch install's network failure as `build_venv` reports it (#9971).
fn torch_download_failed() -> anyhow::Error {
    anyhow::Error::from(trusty_embedderd_py::VenvBuildError::DownloadFailed {
        step: "uv pip sync --python /v/bin/python requirements.txt".to_string(),
        status: "exit status: 2".to_string(),
    })
    .context("py-embedder venv bootstrap")
}

/// #9971 regression: a network failure during the first venv build is
/// transient. The wait reads `retrying` with the error in `/health` and the
/// agent note, and the next attempt's good build swaps to python. Before the
/// fix the first failure was terminal `failed` and ort served for good.
#[tokio::test(start_paused = true)]
#[serial_test::parallel]
async fn venv_network_failure_retries_then_swaps_to_python() {
    let switchable = ort_switchable();
    let fake = ContendedBootstrap::failing_probes(0);
    fake.venv_fails_before.store(1, Ordering::SeqCst);
    *fake.venv_error.lock().expect("venv_error lock") = torch_download_failed;
    spawn_drive(&switchable, &fake, 1);
    wait_until("a scheduled retry", || {
        switchable.active().bootstrap == BootstrapState::Retrying
    })
    .await;

    let view = EmbedderView::of(&switchable);
    assert_eq!(view.bootstrap, "retrying", "{view:?}");
    let note = view.note.expect("ort serves while python is intended");
    assert!(note.contains("on a download step"), "{note}");
    let health = PythonBootstrapHealth::new("retrying", &switchable.bootstrap_control().report());
    assert_eq!(health.attempts, 1);
    assert!(
        health
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("uv pip sync")),
        "{health:?}"
    );
    assert!(health.next_retry_at.is_some(), "{health:?}");

    tokio::time::sleep(background_retry_delay(0)).await;
    wait_until("the swap to python", || {
        switchable.active().kind == BackendKind::Python
    })
    .await;
    assert_eq!(switchable.active().bootstrap, BootstrapState::Ready);
    assert_eq!(fake.venv_checks.lock().expect("venv_checks lock").len(), 2);
    let report = switchable.bootstrap_control().report();
    assert_eq!(report.attempts, 2);
    assert_eq!(report.last_error, None, "a success clears the last error");
}

/// #9971: a full disk (ENOSPC) during the venv build stays permanent: one
/// build, `failed`, no retry scheduled. Classifying every venv error as
/// transient fails this test.
#[tokio::test(start_paused = true)]
#[serial_test::parallel]
async fn venv_disk_full_failure_is_not_retried() {
    let switchable = ort_switchable();
    let fake = ContendedBootstrap::failing_probes(0);
    fake.venv_fails_from.store(0, Ordering::SeqCst);
    *fake.venv_error.lock().expect("venv_error lock") = || {
        anyhow::Error::from(std::io::Error::from(std::io::ErrorKind::StorageFull))
            .context("write /data/py-embedder/project/uv.lock")
            .context("py-embedder venv bootstrap")
    };

    let promoted = drive(&switchable, &fake, 3).await;

    assert!(promoted.is_none());
    assert_eq!(switchable.active().kind, BackendKind::Ort);
    assert_eq!(switchable.active().bootstrap, BootstrapState::Failed);
    assert_eq!(
        fake.venv_checks.lock().expect("venv_checks lock").len(),
        1,
        "a full disk must not be rebuilt"
    );
    let view = EmbedderView::of(&switchable);
    assert_eq!(view.bootstrap, "failed", "{view:?}");
    let report = switchable.bootstrap_control().report();
    assert_eq!(report.next_retry_at, None);
    assert!(
        report
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("uv.lock")),
        "{report:?}"
    );
}
