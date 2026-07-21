//! Unit tests for the bootstrap module — no torch, no venv, no network.

use super::*;
use std::sync::Mutex;

// Serialise tests that mutate TRUSTY_DATA_DIR_OVERRIDE / TRUSTY_UV_BIN.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_data_override<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::set_var(trusty_common::data_dir::DATA_DIR_OVERRIDE_ENV, dir) };
    let out = f();
    unsafe { std::env::remove_var(trusty_common::data_dir::DATA_DIR_OVERRIDE_ENV) };
    out
}

/// Write a fake, executable "venv python" shell script at `path` with `body`
/// as its script contents (no shebang needed — the caller invokes it directly
/// and the kernel's `#!` handling takes over when a shebang line is present).
/// Real venv interpreters are always executable; a plain `fs::write` is not
/// (mode 0o644), so tests that spawn the fake stub must set the exec bit
/// explicitly or every spawn attempt fails with "permission denied".
fn write_fake_venv_python(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn lockfile_hash_is_stable_and_hex() {
    let h1 = lockfile_hash();
    let h2 = lockfile_hash();
    assert_eq!(h1, h2, "hash must be deterministic");
    assert_eq!(h1.len(), 16, "16 hex chars (8 bytes)");
    assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn resolve_layout_places_venv_under_data_dir_and_hash() {
    let tmp = tempfile::tempdir().unwrap();
    with_data_override(tmp.path(), || {
        let layout = resolve_layout().unwrap();
        assert!(layout.base.starts_with(tmp.path()), "base under override");
        assert!(layout.base.ends_with(lockfile_hash()), "keyed by lock hash");
        assert!(layout.venv_dir.ends_with("venv"));
        assert!(layout.project_dir.ends_with("project"));
        assert_eq!(
            layout.venv_dir.join("bin").join("python"),
            layout.venv_python
        );
    });
}

#[test]
fn is_ready_requires_both_sentinel_and_python() {
    let tmp = tempfile::tempdir().unwrap();
    with_data_override(tmp.path(), || {
        let layout = resolve_layout().unwrap();
        assert!(!super::is_ready(&layout), "nothing built yet");

        // Sentinel only, no python → not ready.
        std::fs::create_dir_all(&layout.base).unwrap();
        std::fs::write(layout.base.join(".ready"), lockfile_hash()).unwrap();
        assert!(
            !super::is_ready(&layout),
            "sentinel without python is not ready"
        );

        // Add a fake venv python → ready.
        write_fake_venv_python(&layout.venv_python, "#!/bin/sh\nexit 0\n");
        assert!(super::is_ready(&layout), "sentinel + python is ready");

        // Wrong hash in sentinel → not ready.
        std::fs::write(layout.base.join(".ready"), "deadbeefdeadbeef").unwrap();
        assert!(
            !super::is_ready(&layout),
            "stale hash invalidates readiness"
        );
    });
}

#[test]
fn ensure_venv_fast_path_returns_when_ready_without_building() {
    let tmp = tempfile::tempdir().unwrap();
    with_data_override(tmp.path(), || {
        let layout = resolve_layout().unwrap();
        // An executable stub that exits 0 satisfies the import-smoke recheck
        // (it ignores the `-c "import ..."` args and just succeeds), so this
        // genuinely hits the fast path rather than falling through to a real
        // (network-dependent) rebuild.
        write_fake_venv_python(&layout.venv_python, "#!/bin/sh\nexit 0\n");
        std::fs::write(layout.base.join(".ready"), lockfile_hash()).unwrap();
        // Should hit the fast path (no uv, no build) and return Ok.
        let got = ensure_venv().expect("fast path");
        assert_eq!(got.venv_python, layout.venv_python);
    });
}

#[test]
fn verify_import_smoke_false_when_venv_python_missing() {
    let tmp = tempfile::tempdir().unwrap();
    with_data_override(tmp.path(), || {
        let layout = resolve_layout().unwrap();
        // Nothing written at `layout.venv_python` at all.
        assert!(!super::verify_import_smoke(&layout));
    });
}

#[test]
fn verify_import_smoke_false_when_stub_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    with_data_override(tmp.path(), || {
        let layout = resolve_layout().unwrap();
        // Simulates a corrupted venv (e.g. a broken native `.so`): the
        // interpreter itself runs but the import fails.
        write_fake_venv_python(&layout.venv_python, "#!/bin/sh\nexit 1\n");
        assert!(!super::verify_import_smoke(&layout));
    });
}

#[test]
fn verify_import_smoke_true_when_stub_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    with_data_override(tmp.path(), || {
        let layout = resolve_layout().unwrap();
        write_fake_venv_python(&layout.venv_python, "#!/bin/sh\nexit 0\n");
        assert!(super::verify_import_smoke(&layout));
    });
}

#[test]
fn ensure_venv_rebuilds_when_ready_sentinel_is_stale_corruption() {
    // A `.ready` sentinel + venv python that FAILS the import recheck must
    // not be trusted: `ensure_venv` must fall through past the fast path
    // (verified here via the `uv`-missing error it surfaces once it tries to
    // rebuild, proving the recheck actually gates reuse rather than silently
    // returning the broken venv).
    let tmp = tempfile::tempdir().unwrap();
    with_data_override(tmp.path(), || {
        let layout = resolve_layout().unwrap();
        write_fake_venv_python(&layout.venv_python, "#!/bin/sh\nexit 1\n");
        std::fs::write(layout.base.join(".ready"), lockfile_hash()).unwrap();

        // `with_data_override` already holds `ENV_LOCK` for the duration of
        // this closure, so `TRUSTY_UV_BIN` is mutated directly here.
        unsafe { std::env::set_var("TRUSTY_UV_BIN", "/nonexistent/uv/binary") };
        let err = ensure_venv().expect_err("stale/corrupt venv must not be reused");
        unsafe { std::env::remove_var("TRUSTY_UV_BIN") };
        assert!(
            err.to_string()
                .contains("does not point to an existing file")
                || format!("{err:#}").contains("does not point to an existing file"),
            "expected the rebuild attempt to surface the uv-missing error, got: {err:#}"
        );
    });
}

#[test]
fn locate_uv_rejects_bad_explicit_override() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::set_var("TRUSTY_UV_BIN", "/nonexistent/uv/binary") };
    let err = locate_uv().unwrap_err();
    unsafe { std::env::remove_var("TRUSTY_UV_BIN") };
    assert!(err
        .to_string()
        .contains("does not point to an existing file"));
}

#[test]
fn materialize_project_writes_package_and_lock_skips_tests() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("project");
    super::materialize_project(&dest).unwrap();
    assert!(dest.join("pyproject.toml").is_file());
    assert!(dest.join("uv.lock").is_file());
    assert!(dest
        .join("trusty_embed_sidecar")
        .join("__main__.py")
        .is_file());
    assert!(dest.join("trusty_embed_sidecar").join("model.py").is_file());
    // tests/ dir is intentionally skipped at runtime materialization.
    assert!(
        !dest.join("tests").exists(),
        "tests fixture dir must be skipped"
    );
}
