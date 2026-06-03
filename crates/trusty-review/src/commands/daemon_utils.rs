//! Daemon discovery helpers for trusty-review CLI subcommands.
//!
//! Why: CLI subcommands and external tools (e.g. MPM autodetect) need to
//! locate the running trusty-review daemon without requiring manual
//! configuration. This module centralises the "where is it?" logic so every
//! caller uses the same resolution order.
//!
//! What: reads `~/.local/share/trusty-review/http_addr` (Linux) or
//! `~/Library/Application Support/trusty-review/http_addr` (macOS) via
//! `trusty_common::read_daemon_addr`, falling back to the default port when
//! the file is absent. Mirrors the trusty-search `commands/daemon_utils.rs`
//! pattern.
//!
//! Test: `daemon_base_url_uses_default_when_file_absent`,
//! `read_http_addr_returns_none_when_file_absent`,
//! `daemon_base_url_prefers_file_over_default`.

use trusty_review::service::DEFAULT_PORT;

/// Resolve the base URL of the running trusty-review daemon.
///
/// Why: CLI subcommands (`status`, future `port`) and MPM autodetect need
/// the daemon URL without requiring the user to pass `--port` explicitly.
/// Using the `http_addr` file written at bind time means non-default ports
/// are discovered automatically — the original motivation for issue #676.
///
/// What: reads `trusty_common::read_daemon_addr("trusty-review")`. If the
/// file exists and is non-empty, returns `http://{addr}`. Otherwise falls
/// back to `http://127.0.0.1:{DEFAULT_PORT}`. Any unexpected I/O error
/// from `read_daemon_addr` is silently treated as "not found" so the
/// fallback always succeeds.
///
/// Test: `daemon_base_url_uses_default_when_file_absent`,
/// `daemon_base_url_prefers_file_over_default`.
// Why: not yet wired to a subcommand; allow so -D warnings stays green.
#[allow(dead_code)]
pub fn daemon_base_url() -> String {
    match trusty_common::read_daemon_addr("trusty-review") {
        Ok(Some(addr)) if !addr.is_empty() => format!("http://{addr}"),
        _ => format!("http://127.0.0.1:{DEFAULT_PORT}"),
    }
}

/// Read the raw `host:port` from the trusty-review discovery file, if present.
///
/// Why: callers that need the raw address (not a URL with scheme) can use this
/// instead of parsing `daemon_base_url()`.
/// What: wraps `trusty_common::read_daemon_addr("trusty-review")`; returns
/// `None` when the file is absent or unreadable.
/// Test: `read_http_addr_returns_none_when_file_absent`.
// Why: not yet wired to a subcommand; allow so -D warnings stays green.
#[allow(dead_code)]
pub fn read_http_addr() -> Option<String> {
    trusty_common::read_daemon_addr("trusty-review")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify `daemon_base_url` falls back to the hardcoded default when no
    /// discovery file is present.
    ///
    /// Why: every fresh install starts with no discovery file; the CLI must
    /// still produce a usable URL rather than erroring.
    /// What: asserts that `daemon_base_url()` returns a URL containing the
    /// default port when `TRUSTY_DATA_DIR_OVERRIDE` is pointed at an empty
    /// temp dir (so no `http_addr` file can exist).
    /// Test: sets `TRUSTY_DATA_DIR_OVERRIDE` to a fresh tempdir, asserts the
    /// fallback URL. Serial via `serial_test` to prevent env-var races.
    #[test]
    #[serial_test::serial(trusty_data_dir)]
    fn daemon_base_url_uses_default_when_file_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Point trusty-common's data-dir resolver at an empty temp dir so the
        // http_addr file is guaranteed to be absent.
        // SAFETY: single-threaded test; serial_test serialises all tests that
        // share the `trusty_data_dir` serial key so no concurrent mutation.
        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", tmp.path());
        }
        let url = daemon_base_url();
        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE");
        }
        assert!(
            url.contains(&DEFAULT_PORT.to_string()),
            "fallback URL must contain default port {DEFAULT_PORT}, got: {url}"
        );
        assert!(url.starts_with("http://"), "URL must start with http://");
    }

    /// Verify `read_http_addr` returns `None` when the discovery file is absent.
    ///
    /// Why: callers that check `read_http_addr().is_some()` before deciding
    /// whether to probe the daemon must get a clean `None` rather than an error.
    /// What: overrides the data dir to an empty tempdir and asserts `None`.
    /// Test: pure unit test using env-var override; serial to avoid
    /// env-var races across parallel test threads.
    #[test]
    #[serial_test::serial(trusty_data_dir)]
    fn read_http_addr_returns_none_when_file_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: single-threaded test serialised by serial_test.
        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", tmp.path());
        }
        let result = read_http_addr();
        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE");
        }
        assert!(
            result.is_none(),
            "expected None when no discovery file is present, got: {result:?}"
        );
    }

    /// Verify `daemon_base_url` prefers the discovery file over the default.
    ///
    /// Why: the entire point of the http_addr file is to override the default
    /// port. If `daemon_base_url()` ignores the file and always returns the
    /// default, non-default-port deployments silently fail.
    /// What: writes a fake `http_addr` file with a non-default address, then
    /// asserts `daemon_base_url()` returns that address.
    /// Test: uses `TRUSTY_DATA_DIR_OVERRIDE` + `tempfile` for isolation.
    #[test]
    #[serial_test::serial(trusty_data_dir)]
    fn daemon_base_url_prefers_file_over_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // trusty-common appends the app_name under the override dir, so the
        // http_addr file lives at `<override>/trusty-review/http_addr`.
        let app_dir = tmp.path().join("trusty-review");
        std::fs::create_dir_all(&app_dir).expect("create app dir");
        let addr_file = app_dir.join("http_addr");
        std::fs::write(&addr_file, "127.0.0.1:19999").expect("write addr file");
        // SAFETY: single-threaded test serialised by serial_test.
        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", tmp.path());
        }
        let url = daemon_base_url();
        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE");
        }
        assert_eq!(
            url, "http://127.0.0.1:19999",
            "daemon_base_url must prefer the discovery file; got: {url}"
        );
    }

    /// Verify `read_http_addr` returns the address from the discovery file.
    ///
    /// Why: callers like MPM's autodetect use `read_http_addr()` directly to
    /// get the raw `host:port` without the scheme.
    /// What: writes a fake discovery file and asserts the raw address is returned.
    /// Test: uses `TRUSTY_DATA_DIR_OVERRIDE` + `tempfile` for isolation.
    #[test]
    #[serial_test::serial(trusty_data_dir)]
    fn read_http_addr_returns_addr_from_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app_dir = tmp.path().join("trusty-review");
        std::fs::create_dir_all(&app_dir).expect("create app dir");
        let addr_file = app_dir.join("http_addr");
        std::fs::write(&addr_file, "127.0.0.1:19999").expect("write addr file");
        // SAFETY: single-threaded test serialised by serial_test.
        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", tmp.path());
        }
        let result = read_http_addr();
        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE");
        }
        assert_eq!(
            result.as_deref(),
            Some("127.0.0.1:19999"),
            "expected addr from file; got: {result:?}"
        );
    }
}
