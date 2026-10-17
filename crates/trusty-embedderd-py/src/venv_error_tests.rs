//! #9971: the transient/permanent classification of venv-build failures.

use super::*;
use anyhow::Context as _;

const GIB: u64 = 1024 * 1024 * 1024;

fn network() -> UvError {
    UvError::Network {
        url: "https://example.invalid/uv.tar.gz".to_owned(),
        reason: "connection reset by peer".to_owned(),
    }
}

/// Each typed variant maps to the kind its doc names.
#[test]
fn each_variant_maps_to_its_kind() {
    let step = || "uv pip sync".to_owned();
    let status = || "exit status: 2".to_owned();
    let cases = [
        (
            VenvBuildError::Timeout {
                step: step(),
                secs: 600,
            },
            FailureKind::Transient,
        ),
        (
            VenvBuildError::DownloadFailed {
                step: step(),
                status: status(),
            },
            FailureKind::Transient,
        ),
        (
            VenvBuildError::StepFailed {
                step: step(),
                status: status(),
            },
            FailureKind::Permanent,
        ),
        (
            VenvBuildError::InsufficientDisk {
                path: PathBuf::from("/data"),
                free_bytes: GIB,
            },
            FailureKind::Permanent,
        ),
    ];
    for (err, want) in cases {
        assert_eq!(err.kind(), want, "{err}");
    }
}

/// The first typed cause in the chain decides, under any added context.
/// A network failure is transient; ENOSPC, the fetch opt-out and an untyped
/// error are permanent.
#[test]
fn failure_kind_reads_the_first_typed_cause() {
    let dir = Path::new("/data");
    let enospc = || io::Error::from(io::ErrorKind::StorageFull);
    let cases: Vec<(&str, anyhow::Error, FailureKind)> = vec![
        (
            "uv fetch network failure",
            anyhow::Error::from(network()).context("py-embedder venv bootstrap"),
            FailureKind::Transient,
        ),
        (
            "uv step timeout",
            anyhow::Error::from(VenvBuildError::Timeout {
                step: "uv pip sync".to_owned(),
                secs: 600,
            })
            .context("py-embedder venv bootstrap"),
            FailureKind::Transient,
        ),
        (
            "TRUSTY_UV_FETCH=0",
            UvError::Disabled {
                cache: PathBuf::from("/data/uv"),
            }
            .into(),
            FailureKind::Permanent,
        ),
        (
            "ENOSPC writing the project",
            Err::<(), _>(enospc())
                .context("write /data/project/uv.lock")
                .unwrap_err(),
            FailureKind::Permanent,
        ),
        (
            "ENOSPC inside the uv cache write",
            UvError::Io {
                action: "write",
                path: PathBuf::from("/data/uv/uv"),
                source: enospc(),
            }
            .into(),
            FailureKind::Permanent,
        ),
        (
            "low disk before the build",
            VenvBuildError::InsufficientDisk {
                path: PathBuf::from("/data"),
                free_bytes: GIB,
            }
            .into(),
            FailureKind::Permanent,
        ),
        (
            "smoke test failed on something other than the network",
            smoke_exit_with(Some(1), "exit status: 1".to_owned(), dir, Some(50 * GIB)),
            FailureKind::Permanent,
        ),
        (
            "smoke test's model download failed on the network",
            smoke_exit_with(
                Some(SMOKE_NETWORK_EXIT),
                "exit status: 75".to_owned(),
                dir,
                None,
            )
            .context("py-embedder venv bootstrap"),
            FailureKind::Transient,
        ),
        (
            "untyped",
            anyhow::anyhow!("something else"),
            FailureKind::Permanent,
        ),
    ];
    for (name, err, want) in cases {
        assert_eq!(failure_kind(&err), want, "{name}: {err:#}");
    }
}

/// ENOSPC from the OS decodes to `StorageFull`, which the io arm above covers.
#[cfg(unix)]
#[test]
fn raw_enospc_decodes_to_storage_full() {
    // ENOSPC is 28 on Linux and macOS.
    let err = io::Error::from_raw_os_error(28);
    assert_eq!(err.kind(), io::ErrorKind::StorageFull);
    assert_eq!(
        failure_kind(&anyhow::Error::from(err)),
        FailureKind::Permanent
    );
}

/// A failed download step is transient only while disk space remains; a
/// local step is permanent whatever the disk says.
#[test]
fn a_failed_download_step_is_classified_by_free_space() {
    let dir = Path::new("/data");
    let exit = |args: &[&str], free| step_exit_with(args, "exit status: 2".to_owned(), dir, free);
    let sync = ["pip", "sync", "--python", "/v/bin/python", "req.txt"];
    let install = ["python", "install", "3.11"];
    let export = ["export", "--frozen"];

    assert!(matches!(
        exit(&sync, Some(50 * GIB)),
        VenvBuildError::DownloadFailed { .. }
    ));
    assert!(matches!(
        exit(&install, None),
        VenvBuildError::DownloadFailed { .. }
    ));
    assert!(matches!(
        exit(&sync, Some(0)),
        VenvBuildError::InsufficientDisk { free_bytes: 0, .. }
    ));
    assert!(matches!(
        exit(&export, Some(50 * GIB)),
        VenvBuildError::StepFailed { .. }
    ));
    assert!(exit(&sync, Some(50 * GIB))
        .to_string()
        .starts_with("uv pip sync --python"));
}

/// #9971: the smoke test's network exit is a download failure, so it is
/// transient only while disk space remains; any other exit, or a signal, is
/// the permanent untyped failure.
#[test]
fn a_smoke_test_network_exit_is_classified_by_free_space() {
    let dir = Path::new("/data");
    let exit = |code, free| smoke_exit_with(code, "exit status".to_owned(), dir, free);
    let net = Some(SMOKE_NETWORK_EXIT);

    assert_eq!(
        failure_kind(&exit(net, Some(50 * GIB))),
        FailureKind::Transient
    );
    assert_eq!(failure_kind(&exit(net, None)), FailureKind::Transient);
    let full = exit(net, Some(GIB));
    assert!(matches!(
        full.downcast_ref::<VenvBuildError>(),
        Some(VenvBuildError::InsufficientDisk { .. })
    ));
    assert_eq!(failure_kind(&full), FailureKind::Permanent);
    for other in [Some(1), Some(2), None] {
        let err = exit(other, Some(50 * GIB));
        assert_eq!(
            failure_kind(&err),
            FailureKind::Permanent,
            "{other:?}: {err:#}"
        );
        assert!(err.downcast_ref::<VenvBuildError>().is_none(), "{err:#}");
    }
}
