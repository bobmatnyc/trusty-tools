//! Unit coverage for the isolation resolvers (#8149, #8176).
//!
//! Why: both decisions used to be inline branches in `handle_start`, reachable
//! only by booting a daemon — which is why each shipped with the wrong
//! precedence for months. Every test here is pure: no process env, no daemon,
//! no socket.
//!
//! Test: this file IS the coverage.

use super::{auto_discover_enabled, resolve_data_dir_override, spawn_auto_discover_arg, StartPlan};
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

/// Simulate what a first start leaves in its data dir: the lockfile and the
/// registry. A second start sees a non-empty directory.
fn leave_first_start_state(dir: &Path) {
    std::fs::create_dir_all(dir).expect("the data dir must be creatable");
    std::fs::write(dir.join("daemon.lock"), "4242").expect("the lockfile must be writable");
    std::fs::write(dir.join("indexes.toml"), "").expect("the registry must be writable");
}

/// Why (#8176): a daemon started for a throwaway test against its own
/// `--data-dir` walked the machine and force-reindexed ~21 unrelated colocated
/// repositories. The first fix withheld the scan only while the directory was
/// empty, so the SECOND start of the same data dir — which finds `daemon.lock`
/// and `indexes.toml` from the first — scanned again. Against that logic the
/// second-start assertions below fail.
/// Test: this function IS the test.
#[test]
fn an_explicit_data_dir_never_auto_discovers_on_any_start() {
    let tmp = TempDir::new().expect("a tempdir must be creatable");
    let dir = tmp.path().join("instance-two");

    // First start: the directory does not exist yet.
    let first = StartPlan::resolve(None, Some(&dir), false, false).expect("must resolve");
    assert!(
        !first.discovery.runs_auto_discover(),
        "the first start of an explicit data dir must not auto-discover"
    );

    // Second start: the first start's lockfile and registry are present.
    leave_first_start_state(&dir);
    let second = StartPlan::resolve(None, Some(&dir), false, false).expect("must resolve");
    assert!(
        !second.discovery.runs_auto_discover(),
        "the second start of an explicit data dir must not auto-discover either"
    );
    assert!(
        second.discovery.warm_boot_skips_colocated(),
        "the second start must not run warm boot's colocated scan"
    );

    // The same data dir named only through TRUSTY_DATA_DIR.
    let via_env = StartPlan::resolve(Some(dir.clone().into_os_string()), None, false, false)
        .expect("must resolve");
    assert_eq!(via_env.data_dir.as_deref(), Some(dir.as_path()));
    assert!(
        !via_env.discovery.runs_auto_discover(),
        "TRUSTY_DATA_DIR is an explicit data dir too"
    );
}

/// Does a start with these inputs and `default` as the platform default
/// data dir run the auto-discovery scan?
fn scans_with_default(env: Option<OsString>, flag: Option<&Path>, default: &Path) -> bool {
    StartPlan::resolve_with_defaults(env, flag, &[default.to_path_buf()], false, false)
        .expect("must resolve")
        .discovery
        .runs_auto_discover()
}

/// Why (#8176): a launchd plist that follows #718's hint sets `TRUSTY_DATA_DIR`
/// to the default data dir. That is the default, not an isolated instance, so
/// it keeps the default's auto-discovery. Under the previous rule — any `Some`
/// is explicit — the scan was off and this test fails.
/// Test: this function IS the test.
#[test]
fn a_data_dir_env_equal_to_the_default_is_not_explicit() {
    let tmp = TempDir::new().expect("a tempdir must be creatable");
    let default = tmp.path().join("default");
    std::fs::create_dir_all(&default).expect("the default dir must be creatable");

    let env = Some(default.clone().into_os_string());
    assert!(scans_with_default(env, None, &default), "env == default");
    let slashed = OsString::from(format!("{}/", default.display()));
    assert!(
        scans_with_default(Some(slashed), None, &default),
        "env == default/"
    );

    // A sibling of the default is explicit.
    let other = Some(tmp.path().join("other").into_os_string());
    assert!(!scans_with_default(other, None, &default), "env != default");
}

/// Why (#8176): `--data-dir` naming the default is the default too, including
/// a default that does not exist yet and so compares lexically. Fails under
/// the previous any-`Some`-is-explicit rule.
/// Test: this function IS the test.
#[test]
fn a_data_dir_flag_equal_to_the_default_is_not_explicit() {
    let tmp = TempDir::new().expect("a tempdir must be creatable");
    let default = tmp.path().join("default");
    std::fs::create_dir_all(&default).expect("the default dir must be creatable");
    assert!(
        scans_with_default(None, Some(&default), &default),
        "flag == default"
    );

    let absent = tmp.path().join("absent-default");
    let dotted = PathBuf::from(format!("{}/./", absent.display()));
    assert!(
        scans_with_default(None, Some(&dotted), &absent),
        "flag == an absent default, lexically"
    );

    // --no-auto-discover still refuses on the default.
    let refused = StartPlan::resolve_with_defaults(
        None,
        Some(&default),
        std::slice::from_ref(&default),
        true,
        false,
    )
    .expect("must resolve");
    assert!(!refused.discovery.runs_auto_discover());
}

/// Why (#8176): a symlink to the default data dir names the same directory, so
/// it is the default. Fails under the previous any-`Some`-is-explicit rule.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn a_symlink_to_the_default_data_dir_is_not_explicit() {
    let tmp = TempDir::new().expect("a tempdir must be creatable");
    let default = tmp.path().join("default");
    std::fs::create_dir_all(&default).expect("the default dir must be creatable");
    let link = tmp.path().join("link-to-default");
    std::os::unix::fs::symlink(&default, &link).expect("the symlink must be creatable");
    assert!(
        scans_with_default(None, Some(&link), &default),
        "flag -> default"
    );
}

/// Why (#8176): the opt-in must be able to turn the scan back on for the exact
/// case the safe default turns it off for, or an operator who wants an
/// isolated daemon to discover has no way to ask.
/// Test: this function IS the test.
#[test]
fn auto_discover_opt_in_beats_an_explicit_data_dir() {
    assert!(
        auto_discover_enabled(false, true, true),
        "--auto-discover must grant the scan on an explicit data dir"
    );
    assert!(
        !auto_discover_enabled(true, true, false),
        "--no-auto-discover must still refuse first, even against the opt-in"
    );

    let tmp = TempDir::new().expect("a tempdir must be creatable");
    leave_first_start_state(tmp.path());
    let plan = StartPlan::resolve(None, Some(tmp.path()), false, true).expect("must resolve");
    assert!(plan.discovery.runs_auto_discover());
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
    let plan = StartPlan::resolve(None, None, false, false).expect("must resolve");
    assert!(plan.data_dir.is_none());
    assert!(plan.discovery.runs_auto_discover());
}

/// Why (#8176): `handle_start` reads one decision at three sites, and warm
/// boot's reads it negated. Flipping any site's reading must fail here.
/// What: for each input shape, the warm-boot argument is the negation of the
/// spawn decision, and both match the expected scan verdict.
/// Test: this function IS the test.
#[test]
fn start_plan_wires_every_scan_to_one_decision() {
    let tmp = TempDir::new().expect("a tempdir must be creatable");
    let dir = tmp.path().to_path_buf();
    // (flag, no_auto_discover, auto_discover, expected scan verdict)
    let cases: [(Option<&Path>, bool, bool, bool); 5] = [
        (None, false, false, true),
        (None, true, false, false),
        (Some(&dir), false, false, false),
        (Some(&dir), false, true, true),
        (Some(&dir), true, false, false),
    ];
    for (flag, no, opt_in, scans) in cases {
        let d = StartPlan::resolve(None, flag, no, opt_in)
            .expect("must resolve")
            .discovery;
        let shape = format!("flag={flag:?} no={no} opt_in={opt_in}");
        assert_eq!(
            d.runs_auto_discover(),
            scans,
            "auto-discover spawn: {shape}"
        );
        assert_eq!(d.warm_boot_skips_colocated(), !scans, "warm boot: {shape}");
        assert_eq!(
            d.spawn_arg(),
            spawn_auto_discover_arg(scans, opt_in),
            "{shape}"
        );
    }
}

/// Why (#8176): the plan is only as good as `handle_start`'s reading of it. A
/// unit test cannot boot the daemon, so this pins the call sites in source:
/// the resolver call, warm boot's argument and the spawn guard. Whitespace and
/// trailing commas are dropped first, so a `cargo fmt` rewrap does not trip it.
/// Test: this function IS the test.
#[test]
fn handle_start_reads_the_plan_at_every_scan_site() {
    fn normalize(code: &str) -> String {
        let flat: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        flat.replace(",)", ")")
    }
    let src = normalize(include_str!("daemon.rs"));
    for site in [
        "StartPlan::resolve(std::env::var_os(DATA_DIR_ENV), data_dir, no_auto_discover, auto_discover)?",
        "restore_indexes(&install_state, &embedder, discovery.warm_boot_skips_colocated())",
        "if discovery.runs_auto_discover() { tokio::spawn(crate::commands::discover::auto_discover_and_index())",
        "if let Some(flag) = discovery.spawn_arg() { cmd.arg(flag)",
    ] {
        assert!(
            src.contains(&normalize(site)),
            "handle_start must read the plan at: {site}"
        );
    }
}

/// Why (#8176): the detached child re-resolves the decision on its own. A
/// parent that granted the scan through `--auto-discover` must hand the child
/// the same grant, and a refusal must stay a refusal after the parent has
/// populated the data dir. This composes the parent's plan, the forwarded
/// flag, and the child's own plan.
/// Test: this function IS the test.
#[test]
fn spawn_forwards_the_parents_auto_discover_decision() {
    let tmp = TempDir::new().expect("a tempdir must be creatable");
    let dir = tmp.path().join("instance-two");

    for opt_in in [true, false] {
        let parent = StartPlan::resolve(None, Some(&dir), false, opt_in).expect("must resolve");
        assert_eq!(parent.discovery.runs_auto_discover(), opt_in);
        leave_first_start_state(&dir);

        // Child: the forwarded flag, the inherited env and the explicit flag.
        let forwarded = parent.discovery.spawn_arg();
        let child = StartPlan::resolve(
            Some(dir.clone().into_os_string()),
            Some(&dir),
            forwarded == Some("--no-auto-discover"),
            forwarded == Some("--auto-discover"),
        )
        .expect("must resolve");
        assert_eq!(
            child.discovery.runs_auto_discover(),
            opt_in,
            "the child must reach the parent's decision (opt_in={opt_in})"
        );
    }

    // A refusal is forwarded as a refusal; a default grant forwards nothing.
    assert_eq!(
        spawn_auto_discover_arg(false, false),
        Some("--no-auto-discover")
    );
    assert_eq!(spawn_auto_discover_arg(true, false), None);
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
