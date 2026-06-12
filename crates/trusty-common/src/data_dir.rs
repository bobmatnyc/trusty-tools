//! Data-directory resolution and filesystem utilities.
//!
//! Why: All trusty-* tools want a per-machine, per-app directory under the
//! OS-standard data dir. macOS's `dirs::data_dir()` calls `NSFileManager`
//! which ignores `HOME`/`XDG_DATA_HOME`, so tests need a separate bypass.
//! What: `resolve_data_dir` finds/creates the app data dir; `sanitize_data_root`
//! validates any candidate path; `is_dir` is a convenience predicate.
//! Test: `cargo test -p trusty-common` covers the full battery of data-dir tests.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Environment variable name for the data-directory test escape hatch.
///
/// Why: macOS's `dirs::data_dir()` delegates to `NSFileManager`, a native Cocoa
/// API that ignores `HOME` and `XDG_DATA_HOME`. Setting `HOME` in a test process
/// does **not** redirect `dirs::data_dir()` on macOS, making path isolation
/// impossible without a separate bypass. This constant names that bypass.
///
/// What: When `TRUSTY_DATA_DIR_OVERRIDE` is set in the environment,
/// [`resolve_data_dir`] uses its value as the base directory and skips the
/// `dirs::data_dir()` call entirely. The final path is
/// `${TRUSTY_DATA_DIR_OVERRIDE}/<app_name>`, identical in structure to the
/// normal OS-standard path.
///
/// **Intended for tests only.** Do not set this variable in production; it
/// bypasses the OS-standard application-data directory.
///
/// Test: All `resolve_data_dir` tests in this module set this var to a
/// temporary directory so they run identically on macOS, Linux, and Windows.
pub const DATA_DIR_OVERRIDE_ENV: &str = "TRUSTY_DATA_DIR_OVERRIDE";

/// Validate and, if necessary, replace an unsafe data-root path.
///
/// Why: `dirs::data_dir()` and the HOME-relative fallback can return dangerous
/// paths when the daemon environment is degenerate — e.g. `HOME="/"` on Linux
/// yields `/.trusty-memory`, and `XDG_DATA_HOME="/"` yields `/trusty-memory`.
/// Neither of those are literal `/`, but both scatter application data directly
/// under the filesystem root. This pure helper applies post-resolution
/// validation to any candidate path regardless of which branch produced it, and
/// returns a known-safe fallback path if any guard fires. Being infallible
/// (always returns a usable path) avoids adding an error return to the many
/// existing `resolve_data_dir` call sites while still preventing root-scatter.
///
/// What: checks, in order:
/// 1. `candidate` must be absolute. If not, falls back to
///    `$TMPDIR/trusty-<app_name>` and emits `tracing::error!`.
/// 2. `candidate` must not be exactly `/`. If so, falls back and logs error.
/// 3. `candidate`'s parent must not be `/` unless `candidate` is a normal
///    user-data path (guards against e.g. `/.trusty-memory` from `HOME=/`).
///    Paths whose sole parent is `/` receive the safe-temp fallback.
///
/// The safe fallback is `std::env::temp_dir().join(format!("trusty-{app_name}"))`.
/// This lets the daemon still start (and log a clear error) rather than
/// crash-looping when the host environment is misconfigured.
///
/// Test: `sanitize_data_root_rejects_relative`, `sanitize_data_root_rejects_root`,
/// `sanitize_data_root_rejects_bare_root_child`, `sanitize_data_root_passes_valid_path`.
pub fn sanitize_data_root(candidate: PathBuf, app_name: &str) -> PathBuf {
    let safe_fallback = || std::env::temp_dir().join(format!("trusty-{app_name}"));

    if !candidate.is_absolute() {
        tracing::error!(
            path = %candidate.display(),
            app = app_name,
            "resolved data root is not absolute; \
             falling back to temp dir to prevent CWD-relative palace creation. \
             Check HOME and TRUSTY_DATA_DIR_OVERRIDE in the daemon environment."
        );
        return safe_fallback();
    }

    if candidate == Path::new("/") {
        tracing::error!(
            app = app_name,
            "resolved data root is the filesystem root (/); \
             falling back to temp dir. \
             Check HOME and TRUSTY_DATA_DIR_OVERRIDE in the daemon environment."
        );
        return safe_fallback();
    }

    if candidate.parent() == Some(Path::new("/")) {
        tracing::error!(
            path = %candidate.display(),
            app = app_name,
            "resolved data root is a direct child of the filesystem root; \
             this usually means HOME or XDG_DATA_HOME is set to '/'. \
             Falling back to temp dir to prevent data scatter under /."
        );
        return safe_fallback();
    }

    candidate
}

/// Resolve `<data_dir>/<app_name>`, creating it if it doesn't exist.
///
/// Why: All trusty-* tools want a per-machine, per-app directory under the
/// OS-standard data dir (`~/Library/Application Support/`, `~/.local/share/`,
/// `%APPDATA%/`). If `dirs::data_dir()` is unavailable (rare — locked-down
/// containers), falls back to `~/.<app_name>` so the tool still works.
///
/// The [`DATA_DIR_OVERRIDE_ENV`] (`TRUSTY_DATA_DIR_OVERRIDE`) environment
/// variable provides a test escape hatch: when set to a *non-empty absolute
/// path*, `dirs::data_dir()` is **never called** and the variable's value is
/// used as the base directory instead. This is necessary because macOS's
/// `dirs::data_dir()` calls `NSFileManager` — a native Cocoa API that
/// resolves the application-support directory through the system rather than
/// through the process environment — so setting `HOME` or `XDG_DATA_HOME` in
/// a test process does not redirect it. `TRUSTY_DATA_DIR_OVERRIDE` is the
/// only reliable cross-platform way to isolate test data paths. **It is
/// intended for tests only; do not set it in production.**
///
/// Safety guards: an empty/whitespace-only override is treated as unset; a
/// non-absolute override is rejected; a root `/` override is rejected. The
/// final resolved path passes through [`sanitize_data_root`].
///
/// What: returns the absolute path `${base}/<app_name>` (created if absent).
/// Resolution order:
/// 1. `$TRUSTY_DATA_DIR_OVERRIDE/<app_name>` — when the env var is non-empty, absolute, and non-root.
/// 2. `$(dirs::data_dir())/<app_name>` — normal OS-standard path.
/// 3. `~/.<app_name>` — fallback when `dirs::data_dir()` returns `None`.
///
/// Test: `resolve_data_dir_creates_directory`, `resolve_data_dir_empty_override_uses_platform_dir`,
/// `resolve_data_dir_whitespace_override_uses_platform_dir`,
/// `resolve_data_dir_relative_override_errors`, `resolve_data_dir_root_override_errors`.
pub fn resolve_data_dir(app_name: &str) -> Result<PathBuf> {
    let base = match std::env::var(DATA_DIR_OVERRIDE_ENV) {
        Ok(raw) if raw.trim().is_empty() => {
            tracing::warn!(
                env = DATA_DIR_OVERRIDE_ENV,
                "TRUSTY_DATA_DIR_OVERRIDE is set but empty; ignoring and using \
                 the platform data directory instead. An empty override would \
                 produce a relative path that resolves against the daemon's \
                 working directory (/ under launchd), which is never correct."
            );
            dirs::data_dir()
                .or_else(|| dirs::home_dir().map(|h| h.join(format!(".{app_name}"))))
                .context("could not resolve data directory or home directory")?
        }
        Ok(raw) => {
            let p = PathBuf::from(&raw);
            if !p.is_absolute() {
                anyhow::bail!(
                    "TRUSTY_DATA_DIR_OVERRIDE={raw:?} is a relative path; only \
                     absolute paths are accepted to prevent the data directory \
                     from depending on the daemon's working directory"
                );
            }
            if p == Path::new("/") {
                anyhow::bail!(
                    "TRUSTY_DATA_DIR_OVERRIDE={raw:?} resolves to the filesystem \
                     root (/); refusing to create palace directories directly \
                     under / as that would scatter data across the root filesystem"
                );
            }
            p
        }
        Err(_) => dirs::data_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join(format!(".{app_name}"))))
            .context("could not resolve data directory or home directory")?,
    };
    let dir = if base.ends_with(format!(".{app_name}")) {
        base
    } else {
        base.join(app_name)
    };
    let dir = sanitize_data_root(dir, app_name);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create data directory {}", dir.display()))?;
    Ok(dir)
}

/// Check whether a path exists and is a directory.
///
/// Why: tiny but commonly-needed shim — clearer at call sites than
/// `path.exists() && path.is_dir()`.
/// What: returns `true` iff the path exists and metadata reports a directory.
/// Test: `is_dir_recognises_directories`.
pub fn is_dir(path: &Path) -> bool {
    path.metadata().map(|m| m.is_dir()).unwrap_or(false)
}

/// Mutex serialising all tests that mutate `TRUSTY_DATA_DIR_OVERRIDE`.
///
/// Why: `daemon_addr` tests also call `resolve_data_dir`, so tests across both
/// modules race on the same env var. Sharing one lock (exported from the module
/// that owns the constant) prevents spurious failures without pulling in an
/// external crate.
/// What: A `std::sync::Mutex<()>` that every env-mutating test locks before
/// touching `TRUSTY_DATA_DIR_OVERRIDE`.
/// Test: this is the synchronisation primitive itself — used by test helpers.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    use super::ENV_LOCK;

    fn tempfile_like_dir() -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("trusty-common-test-{pid}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn resolve_data_dir_creates_directory() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile_like_dir();
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let dir = resolve_data_dir("trusty-test-xyz").unwrap();
        assert!(
            dir.exists(),
            "data dir should be created at {}",
            dir.display()
        );
        assert!(dir.is_dir());
        assert!(
            dir.starts_with(&tmp),
            "data dir {} should live under override {}",
            dir.display(),
            tmp.display()
        );
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
    }

    /// Why: guard introduced in #503 — an empty override must not produce a
    /// relative path that resolves under the daemon CWD.
    /// What: sets TRUSTY_DATA_DIR_OVERRIDE="" and asserts the result is an
    /// absolute path that does NOT start with "".
    /// Test: this function.
    #[test]
    fn resolve_data_dir_empty_override_uses_platform_dir() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, "");
        }
        let result = resolve_data_dir("trusty-test-empty-override");
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        let dir = result.expect("empty override should fall back to platform dir");
        assert!(
            dir.is_absolute(),
            "resolved dir should be absolute, got {}",
            dir.display()
        );
        assert_ne!(
            dir,
            std::path::PathBuf::from("/"),
            "resolved dir must not be filesystem root"
        );
    }

    /// Why: whitespace-only overrides are as dangerous as empty ones.
    /// What: sets TRUSTY_DATA_DIR_OVERRIDE="   " and asserts an absolute fallback.
    /// Test: this function.
    #[test]
    fn resolve_data_dir_whitespace_override_uses_platform_dir() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, "   ");
        }
        let result = resolve_data_dir("trusty-test-ws-override");
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        let dir = result.expect("whitespace override should fall back to platform dir");
        assert!(dir.is_absolute(), "resolved dir should be absolute");
    }

    /// Why: a relative override is non-deterministic (depends on daemon CWD).
    /// What: sets TRUSTY_DATA_DIR_OVERRIDE="relative/path" and asserts an error.
    /// Test: this function.
    #[test]
    fn resolve_data_dir_relative_override_errors() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, "relative/path");
        }
        let result = resolve_data_dir("trusty-test-relative");
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        assert!(
            result.is_err(),
            "relative override should be rejected, but got Ok({})",
            result.unwrap().display()
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("relative"),
            "error should mention 'relative', got: {msg}"
        );
    }

    /// Why: override set to "/" would create palace dirs directly under the
    /// filesystem root, scattering data.
    /// What: sets TRUSTY_DATA_DIR_OVERRIDE="/" and asserts an error.
    /// Test: this function.
    #[test]
    fn resolve_data_dir_root_override_errors() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, "/");
        }
        let result = resolve_data_dir("trusty-test-root");
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        assert!(
            result.is_err(),
            "root '/' override should be rejected, but got Ok({})",
            result.unwrap().display()
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains('/'),
            "error should mention the path, got: {msg}"
        );
    }

    /// Why: confirms that a valid absolute override is still honoured.
    /// What: sets TRUSTY_DATA_DIR_OVERRIDE to a tempdir and asserts the resolved
    /// path lives under it.
    /// Test: this function (complements resolve_data_dir_creates_directory).
    #[test]
    fn resolve_data_dir_valid_absolute_override_is_honoured() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile_like_dir();
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let result = resolve_data_dir("trusty-test-abs-override");
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        let dir = result.expect("valid absolute override should succeed");
        assert!(
            dir.starts_with(&tmp),
            "resolved dir {} should be under override {}",
            dir.display(),
            tmp.display()
        );
        assert!(dir.is_absolute(), "resolved dir must be absolute");
    }

    /// Why: `sanitize_data_root` must catch a relative candidate.
    /// What: passes `PathBuf::from("relative/path")` and asserts the returned
    /// path is absolute and lives under `temp_dir()`.
    /// Test: this function.
    #[test]
    fn sanitize_data_root_rejects_relative() {
        let result = sanitize_data_root(PathBuf::from("relative/path"), "myapp");
        assert!(result.is_absolute(), "fallback must be absolute");
        let name = result.file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with("trusty-"),
            "fallback dir name should start with trusty-, got {name}"
        );
    }

    /// Why: a candidate equal to "/" must be replaced.
    /// What: passes `PathBuf::from("/")` and asserts a safe fallback is returned.
    /// Test: this function.
    #[test]
    fn sanitize_data_root_rejects_root() {
        let result = sanitize_data_root(PathBuf::from("/"), "myapp");
        assert!(result.is_absolute(), "fallback must be absolute");
        assert_ne!(result, PathBuf::from("/"), "must not still be /");
        let name = result.file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with("trusty-"),
            "fallback should start with trusty-"
        );
    }

    /// Why: `HOME="/"` on Linux yields `/.trusty-memory` — a bare root child
    /// is as dangerous as `/` itself.
    /// What: passes `/bare-child` (parent == "/") and asserts a safe fallback.
    /// Test: this function.
    #[test]
    fn sanitize_data_root_rejects_bare_root_child() {
        let result = sanitize_data_root(PathBuf::from("/bare-child"), "myapp");
        assert!(result.is_absolute(), "fallback must be absolute");
        assert_ne!(
            result,
            PathBuf::from("/bare-child"),
            "bare root-child must be replaced"
        );
        let name = result.file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with("trusty-"),
            "fallback should start with trusty-"
        );
    }

    /// Why: valid paths must pass through unchanged.
    /// What: passes a tempdir-based path and asserts it is returned unmodified.
    /// Test: this function.
    #[test]
    fn sanitize_data_root_passes_valid_path() {
        let tmp = tempfile_like_dir();
        let candidate = tmp.join("trusty-myapp");
        let result = sanitize_data_root(candidate.clone(), "myapp");
        assert_eq!(
            result, candidate,
            "valid absolute path should be returned unchanged"
        );
    }

    #[test]
    fn is_dir_recognises_directories() {
        let tmp = tempfile_like_dir();
        assert!(is_dir(&tmp));
        assert!(!is_dir(&tmp.join("nope")));
    }
}
