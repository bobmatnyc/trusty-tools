//! Pre-init ONNX Runtime check for load-dynamic builds (#8616).
//!
//! Why: under `load-dynamic`, `ort` 2.0.0-rc.12 loads the runtime inside the
//! `OnceLock` initialiser behind `ort::api()`. When that load fails (missing
//! file, not a library, runtime older than `ort` requires), `ort` builds its
//! error through `ort::api()`, which re-enters the `OnceLock` it is still
//! initialising and waits forever. No error ever reaches the caller. The only
//! safe place to catch a bad runtime is before `ort` sees it.
//!
//! What: [`check_ort_runtime`] resolves the library path exactly as `ort`
//! does ([`resolve_dylib_path`]), loads it with `libloading`, calls
//! `OrtGetApiBase()->GetVersionString()`, and requires the version floor the
//! linked `ort` enforces ([`check_runtime_version`]). Every failure is an
//! error, including a version string it cannot parse: the check fails
//! closed and never lets `ort` initialise on an unverified runtime. A library
//! that passes stays loaded, so `ort`'s own load of the same path reuses it.
//!
//! Test: `check_ort_runtime_rejects_missing_dylib`,
//! `check_ort_runtime_rejects_empty_dylib`,
//! `check_ort_runtime_rejects_garbage_dylib`,
//! `check_ort_runtime_rejects_library_without_ort_entry_point`,
//! `check_runtime_version_accepts_required_floor_and_newer`,
//! `check_runtime_version_rejects_older_runtime`,
//! `check_runtime_version_fails_closed_on_unreadable_version`.

use std::ffi::CStr;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// The ONNX Runtime minor version the linked `ort` requires (`1.<this>.x`).
pub(crate) const REQUIRED_MINOR: u32 = ort::MINOR_VERSION;

/// The library name `ort` loads when `ORT_DYLIB_PATH` is unset or empty.
const DEFAULT_DYLIB_NAME: &str = if cfg!(target_os = "macos") {
    "libonnxruntime.dylib"
} else if cfg!(target_os = "windows") {
    "onnxruntime.dll"
} else {
    "libonnxruntime.so"
};

/// Resolve the runtime path the way `ort` rc.12's `setup_api` does.
///
/// Why: the check must test the exact file `ort` will load next.
/// What: `ORT_DYLIB_PATH` when set and non-empty, else the platform default
/// name. A relative path is joined to the executable's directory when that
/// file exists, and is otherwise left for the dynamic loader's search path.
/// Test: the `check_ort_runtime_rejects_*` tests set `ORT_DYLIB_PATH`.
pub(crate) fn resolve_dylib_path() -> PathBuf {
    let raw: PathBuf = match std::env::var("ORT_DYLIB_PATH") {
        Ok(s) if !s.is_empty() => s.into(),
        _ => DEFAULT_DYLIB_NAME.into(),
    };
    if raw.is_absolute() {
        return raw;
    }
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&raw)));
    match beside_exe {
        Some(path) if path.exists() => path,
        _ => raw,
    }
}

/// Verify the ONNX Runtime `ort` is about to load, before `ort` loads it.
///
/// Why (#8616): a runtime `ort` cannot load hangs the daemon inside
/// `ort::api()` instead of returning an error; see the module doc.
/// What: loads the library at [`resolve_dylib_path`], reads its version and
/// checks it with [`check_runtime_version`]. Returns the version string.
///
/// # Errors
///
/// When the library cannot be loaded, has no `OrtGetApiBase` entry point,
/// reports no version, or reports a version that is unreadable or older than
/// `1.REQUIRED_MINOR`.
///
/// Test: `check_ort_runtime_rejects_missing_dylib`,
/// `check_ort_runtime_rejects_empty_dylib`,
/// `check_ort_runtime_rejects_garbage_dylib`,
/// `check_ort_runtime_rejects_library_without_ort_entry_point`.
pub(crate) fn check_ort_runtime() -> Result<String> {
    let path = resolve_dylib_path();
    let version = read_runtime_version(&path).and_then(|version| {
        check_runtime_version(&version)?;
        Ok(version)
    });
    version.with_context(|| {
        format!(
            "ONNX Runtime at `{}` is not usable (#8616). Set ORT_DYLIB_PATH to the absolute \
             path of an ONNX Runtime 1.{REQUIRED_MINOR}.x (or newer 1.x) library",
            path.display()
        )
    })
}

/// Load the library at `path` and return the version string it reports.
///
/// The loaded library is leaked on purpose: `ort` loads the same path next,
/// and unloading a runtime just before that gains nothing.
fn read_runtime_version(path: &Path) -> Result<String> {
    // SAFETY: loading runs the library's initialisers. That is the same load
    // `ort` performs next, from the same path.
    let lib = unsafe { libloading::Library::new(path) }
        .with_context(|| format!("cannot load `{}`", path.display()))?;
    let lib: &'static libloading::Library = Box::leak(Box::new(lib));

    // SAFETY: `OrtGetApiBase` takes no arguments and returns a pointer to a
    // static `OrtApiBase`; the type matches `ort-sys`'s declaration.
    let get_api_base = unsafe {
        lib.get::<unsafe extern "C" fn() -> *const ort::sys::OrtApiBase>(b"OrtGetApiBase")
    }
    .context("the library has no `OrtGetApiBase` entry point; it is not ONNX Runtime")?;
    // SAFETY: calling the entry point resolved above, with no arguments.
    let base = unsafe { get_api_base() };
    if base.is_null() {
        bail!("`OrtGetApiBase` returned null");
    }
    // SAFETY: `base` is non-null and points at the runtime's static
    // `OrtApiBase`, whose `GetVersionString` takes no arguments.
    let raw = unsafe { ((*base).GetVersionString)() };
    if raw.is_null() {
        bail!("`GetVersionString` returned null, so the runtime version is unknown");
    }
    // SAFETY: `raw` is non-null and, per the ONNX Runtime C API, points at a
    // static NUL-terminated string the caller must not free.
    let version = unsafe { CStr::from_ptr(raw) }
        .to_str()
        .context("the runtime version string is not UTF-8")?;
    Ok(version.to_owned())
}

/// Require `version` to be `1.REQUIRED_MINOR` or a newer `1.x`.
///
/// Why: `ort` rc.12 refuses a runtime whose minor version is below its API
/// version, and that refusal is what hangs (#8616). The check must fail
/// closed: a version it cannot read is an error, never a pass.
/// What: parses `<major>.<minor>[.<rest>]`; errors unless major is 1 and
/// minor is at least [`REQUIRED_MINOR`].
/// Test: `check_runtime_version_accepts_required_floor_and_newer`,
/// `check_runtime_version_rejects_older_runtime`,
/// `check_runtime_version_fails_closed_on_unreadable_version`.
pub(crate) fn check_runtime_version(version: &str) -> Result<()> {
    let mut parts = version.trim().split('.');
    let major = parts.next().and_then(|p| p.parse::<u32>().ok());
    let minor = parts.next().and_then(|p| p.parse::<u32>().ok());
    let (Some(major), Some(minor)) = (major, minor) else {
        bail!(
            "cannot read a version from {version:?}; refusing to initialise ONNX Runtime \
             without a verified 1.{REQUIRED_MINOR}.x or newer version"
        );
    };
    if major != 1 {
        bail!(
            "ONNX Runtime {version} has major version {major}; this build supports \
             1.{REQUIRED_MINOR}.x or a newer 1.x only"
        );
    }
    if minor < REQUIRED_MINOR {
        bail!("ONNX Runtime {version} is older than the 1.{REQUIRED_MINOR}.x this build requires");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::env_lock;

    /// Run `check_ort_runtime` with `ORT_DYLIB_PATH` set to `value`, under
    /// the crate env lock, restoring the previous value afterwards.
    fn check_with_dylib_path(value: &std::ffi::OsStr) -> Result<String> {
        let _guard = env_lock();
        let previous = std::env::var_os("ORT_DYLIB_PATH");
        // SAFETY: serialised under `env_lock()`; no other test thread touches
        // the environment while the guard is held.
        unsafe { std::env::set_var("ORT_DYLIB_PATH", value) };
        let result = check_ort_runtime();
        // SAFETY: as above.
        unsafe {
            match previous {
                Some(v) => std::env::set_var("ORT_DYLIB_PATH", v),
                None => std::env::remove_var("ORT_DYLIB_PATH"),
            }
        }
        result
    }

    /// Assert `result` is an error naming the probed path and the #8616 fix.
    fn assert_refused(result: Result<String>, path: &Path) {
        let err = match result {
            Ok(version) => panic!("expected a refusal for `{}`, got {version}", path.display()),
            Err(err) => format!("{err:#}"),
        };
        assert!(
            err.contains(&path.display().to_string()),
            "must name the path: {err}"
        );
        assert!(err.contains("ORT_DYLIB_PATH"), "must name the knob: {err}");
        assert!(err.contains("#8616"), "must name the issue: {err}");
    }

    /// Why (#8616): a missing runtime hung `ort`; it must be refused first.
    /// What: points `ORT_DYLIB_PATH` at a file that does not exist.
    /// Test: itself.
    #[test]
    fn check_ort_runtime_rejects_missing_dylib() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("libonnxruntime-missing.so");
        assert_refused(check_with_dylib_path(path.as_os_str()), &path);
    }

    /// Why (#8616): an empty file is not a loadable runtime.
    /// What: points `ORT_DYLIB_PATH` at a zero-byte file.
    /// Test: itself.
    #[test]
    fn check_ort_runtime_rejects_empty_dylib() {
        let file = tempfile::NamedTempFile::new().expect("tempfile");
        assert_refused(check_with_dylib_path(file.path().as_os_str()), file.path());
    }

    /// Why (#8616): a corrupt download is not a loadable runtime.
    /// What: points `ORT_DYLIB_PATH` at a file of non-library bytes.
    /// Test: itself.
    #[test]
    fn check_ort_runtime_rejects_garbage_dylib() {
        let file = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(file.path(), b"\x7fELF not really a shared object").expect("write");
        assert_refused(check_with_dylib_path(file.path().as_os_str()), file.path());
    }

    /// Why (#8616): a real library that is not ONNX Runtime must be refused,
    /// not called into.
    /// What: points `ORT_DYLIB_PATH` at the platform C library, which loads
    /// but has no `OrtGetApiBase`.
    /// Test: itself.
    #[test]
    fn check_ort_runtime_rejects_library_without_ort_entry_point() {
        let libc = Path::new(if cfg!(target_os = "macos") {
            "/usr/lib/libSystem.B.dylib"
        } else {
            "libc.so.6"
        });
        let result = check_with_dylib_path(libc.as_os_str());
        let err = format!("{:#}", result.expect_err("libc is not ONNX Runtime"));
        assert!(
            err.contains("OrtGetApiBase"),
            "must name the missing entry point: {err}"
        );
    }

    /// Why: the floor itself and newer 1.x runtimes must pass.
    /// What: checks `1.REQUIRED_MINOR.2`, the next minor, and a bare
    /// `major.minor`.
    /// Test: itself.
    #[test]
    fn check_runtime_version_accepts_required_floor_and_newer() {
        for ok in [
            format!("1.{REQUIRED_MINOR}.2"),
            format!("1.{}.0", REQUIRED_MINOR + 1),
            format!("1.{REQUIRED_MINOR}"),
        ] {
            check_runtime_version(&ok).unwrap_or_else(|e| panic!("{ok} must pass: {e:#}"));
        }
    }

    /// Why (#8612, #8616): ORT 1.20.1 was the documented runtime and hung the
    /// daemon; one minor below the floor must fail too.
    /// What: checks `1.20.1` and `1.(REQUIRED_MINOR - 1).9`.
    /// Test: itself.
    #[test]
    fn check_runtime_version_rejects_older_runtime() {
        for old in ["1.20.1".to_owned(), format!("1.{}.9", REQUIRED_MINOR - 1)] {
            let err = check_runtime_version(&old).expect_err("older runtime must fail");
            assert!(err.to_string().contains("older than"), "{old}: {err}");
        }
    }

    /// Why (#8616): the Fail-Open Check — a version the probe cannot read must
    /// be an error, never a pass into `ort` init.
    /// What: feeds empty, non-numeric, one-component and wrong-major strings.
    /// Test: itself.
    #[test]
    fn check_runtime_version_fails_closed_on_unreadable_version() {
        for bad in ["", "garbage", "1", "1.x.0", "v1.24.2", "2.30.0"] {
            assert!(
                check_runtime_version(bad).is_err(),
                "{bad:?} must fail closed, not pass"
            );
        }
    }
}
