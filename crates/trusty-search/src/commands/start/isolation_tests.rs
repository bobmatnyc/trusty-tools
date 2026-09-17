//! Unit coverage for the isolation resolvers (#8149, #8176).
//!
//! Why: both decisions used to be inline branches in `handle_start`, reachable
//! only by booting a daemon — which is why each shipped with the wrong
//! precedence for months. Every test here is pure: no process env, no daemon,
//! no socket.
//!
//! Test: this file IS the coverage.

use super::{auto_discover_enabled, data_dir_is_fresh, resolve_data_dir_override};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Why (#8149): `handle_start` stamped `--data-dir` into `TRUSTY_DATA_DIR`
/// only when the variable was unset, so a second daemon that named its own
/// data dir kept resolving every per-instance path — the RPC socket included —
/// from the FIRST daemon's inherited value and bound its socket. The fix
/// reverses the precedence; this pins it.
/// Test: this function IS the test.
#[test]
fn data_dir_flag_wins_over_an_inherited_env_value() {
    let inherited = OsString::from("/srv/daemon-one");
    let flag = PathBuf::from("/srv/daemon-two");

    let resolved = resolve_data_dir_override(Some(inherited.clone()), Some(&flag))
        .expect("two absolute paths must resolve");

    assert_eq!(
        resolved.as_deref(),
        Some(flag.as_path()),
        "an explicit --data-dir must win over an inherited TRUSTY_DATA_DIR"
    );

    // The inherited value still applies when no flag came.
    let env_only = resolve_data_dir_override(Some(inherited.clone()), None)
        .expect("an absolute env value must resolve");
    assert_eq!(env_only.as_deref(), Some(Path::new("/srv/daemon-one")));

    // Neither: the machine default, which this resolver reports as `None`.
    assert!(resolve_data_dir_override(None, None)
        .expect("no override must resolve")
        .is_none());
}

/// Why: `var_os` reports an exported-but-empty `TRUSTY_DATA_DIR` as `Some("")`,
/// and treating it as a data dir would join every per-instance path onto a
/// relative root that resolves against the daemon's cwd (`/` under launchd).
/// Test: this function IS the test.
#[test]
fn an_empty_data_dir_env_value_is_treated_as_unset() {
    let resolved = resolve_data_dir_override(Some(OsString::new()), None)
        .expect("an empty env value must not be fatal");
    assert!(
        resolved.is_none(),
        "an empty TRUSTY_DATA_DIR must read as unset, not as a data dir"
    );
}

/// Why: a relative data dir resolves differently per cwd, so two invocations
/// that believe they are one instance would use two roots. The refusal is the
/// fail-closed answer; resolving it against cwd is the fail-open one.
/// Test: this function IS the test.
#[test]
fn a_relative_data_dir_is_refused() {
    let err = resolve_data_dir_override(None, Some(Path::new("relative/dir")))
        .expect_err("a relative flag must be refused");
    assert!(
        err.to_string().contains("absolute"),
        "the refusal must say why: {err:#}"
    );

    let err = resolve_data_dir_override(Some(OsString::from("relative/dir")), None)
        .expect_err("a relative env value must be refused");
    assert!(
        err.to_string().contains("absolute"),
        "the refusal must say why: {err:#}"
    );
}

/// Why (#8176): a daemon started for a throwaway test against a brand-new
/// `--data-dir` walked the machine and force-reindexed ~21 unrelated colocated
/// repositories, because auto-discovery was the default on every data dir. The
/// safe default now applies to a fresh isolated data dir, and only an explicit
/// opt-in re-enables it. Against the pre-fix resolution — auto-discovery on
/// unless `--no-auto-discover` — the first assertion below fails.
/// Test: this function IS the test.
#[test]
fn fresh_data_dir_does_not_auto_discover_without_opt_in() {
    let tmp = TempDir::new().expect("a tempdir must be creatable");
    let fresh = tmp.path().join("instance-two");
    assert!(
        data_dir_is_fresh(&fresh),
        "a directory that does not exist yet is fresh"
    );
    std::fs::create_dir_all(&fresh).expect("the fresh dir must be creatable");
    assert!(
        data_dir_is_fresh(&fresh),
        "an empty directory is still fresh"
    );

    assert!(
        !auto_discover_enabled(false, false, data_dir_is_fresh(&fresh)),
        "a fresh isolated data dir must not auto-discover without --auto-discover"
    );

    std::fs::write(fresh.join("indexes.toml"), "").expect("the registry stub must be writable");
    assert!(
        !data_dir_is_fresh(&fresh),
        "a data dir carrying a registry is not fresh"
    );
    assert!(
        auto_discover_enabled(false, false, data_dir_is_fresh(&fresh)),
        "a data dir this daemon has used before keeps the pre-#8176 default"
    );
}

/// Why (#8176): the opt-in must be able to turn the scan back on for the exact
/// case the safe default turns it off for, or an operator who wants a fresh
/// isolated daemon to discover has no way to ask.
/// Test: this function IS the test.
#[test]
fn auto_discover_opt_in_beats_a_fresh_data_dir() {
    assert!(
        auto_discover_enabled(false, true, true),
        "--auto-discover must grant the scan on a fresh isolated data dir"
    );
    assert!(
        !auto_discover_enabled(true, true, false),
        "--no-auto-discover must still refuse first, even against the opt-in"
    );
}

/// Why (#8176 blast radius): the change must not alter the machine's default
/// data dir, which is what every existing install boots on.
/// Test: this function IS the test.
#[test]
fn the_default_data_dir_still_auto_discovers() {
    assert!(
        auto_discover_enabled(false, false, false),
        "the default data dir must keep auto-discovering"
    );
    assert!(
        !auto_discover_enabled(true, false, false),
        "--no-auto-discover must keep working on the default data dir"
    );
}

/// Why (#8149): the reported failure was a second daemon binding the FIRST
/// daemon's `~/.local/share/trusty-search/trusty-search.sock` and capturing its
/// RPC traffic. `service::socket::resolve_socket_path` already derived the
/// socket from the data dir it was handed; what it was handed came from an
/// inherited `TRUSTY_DATA_DIR` rather than from the `--data-dir` the second
/// daemon named, so the derivation was correct and the input was the first
/// daemon's. This composes the two halves — which dir, then which socket — and
/// is the assertion that fails against the pre-fix precedence: there
/// `resolve_data_dir_override` answers `instance-one`, so the socket below is
/// the first daemon's.
/// Test: this function IS the test.
#[test]
fn socket_path_follows_trusty_data_dir_not_home() {
    let tmp = TempDir::new().expect("a tempdir must be creatable");
    let first = tmp.path().join("instance-one");
    let second = tmp.path().join("instance-two");

    let effective =
        resolve_data_dir_override(Some(first.clone().into_os_string()), Some(second.as_path()))
            .expect("two absolute paths must resolve")
            .expect("a data dir was supplied");

    let socket = crate::service::socket::resolve_socket_path(Some(effective.as_os_str()))
        .expect("an absolute data dir must yield a socket");

    assert_eq!(
        socket,
        second.join("trusty-search.sock"),
        "the socket must live under the data dir this daemon named"
    );

    // …and never under the first daemon's, which is the capture #8149 reported.
    let first_socket = crate::service::socket::resolve_socket_path(Some(first.as_os_str()))
        .expect("the first daemon's socket must also resolve");
    assert_ne!(
        socket, first_socket,
        "a second daemon must never bind the first daemon's socket"
    );

    // Nor under the HOME-derived shared location every daemon would otherwise
    // share. `resolve_socket_path(None)` IS that location.
    let shared =
        crate::service::socket::resolve_socket_path(None).expect("the shared path must resolve");
    assert_ne!(
        socket, shared,
        "an isolated instance must not fall back to the HOME-derived socket"
    );
    assert_eq!(
        socket.file_name(),
        shared.file_name(),
        "isolation must move the directory, never rename the socket"
    );
}
