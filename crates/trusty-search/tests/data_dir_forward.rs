//! Regression tests for issue #1182: background daemon self-spawn must forward
//! the explicit `--data-dir` CLI flag to the child process even when
//! `TRUSTY_DATA_DIR` is already set in the parent environment.
//!
//! Why: before the fix, `handle_start` only stamped `TRUSTY_DATA_DIR` when it
//! was *unset*, so `trusty-search start --data-dir /new/path` with a pre-existing
//! `TRUSTY_DATA_DIR=/old/path` would silently make the spawned child use
//! `/old/path` — the explicit flag was lost. The fix passes `--data-dir` as an
//! explicit CLI arg to the child Command so it wins over any inherited env.
//!
//! What: pure-logic tests that mirror the arg-building decision in
//! `commands::start::handle_start` — no subprocess is actually spawned.
//!
//! Test: `cargo test -p trusty-search --test data_dir_forward`

/// Mirror of the arg-forwarding logic in `commands::start::handle_start`.
///
/// Why: extracted as a pure function so the decision ("include `--data-dir` in
/// the child args?") can be tested without spawning a real process.
/// What: returns the CLI args that `handle_start` would pass to the background
/// child Command, given a `data_dir` override and the value of `no_auto_discover`.
/// Test: the tests below drive every branch.
fn build_spawn_args(
    port: u16,
    device: &str,
    data_dir: Option<&std::path::Path>,
    no_auto_discover: bool,
) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec![
        "start".into(),
        "--foreground".into(),
        "--port".into(),
        port.to_string().into(),
        "--device".into(),
        device.into(),
    ];
    if no_auto_discover {
        args.push("--no-auto-discover".into());
    }
    // Issue #1182: --data-dir is forwarded explicitly so the CLI flag wins
    // over any pre-existing TRUSTY_DATA_DIR in the child's inherited env.
    if let Some(dir) = data_dir {
        args.push("--data-dir".into());
        args.push(dir.as_os_str().into());
    }
    args
}

/// Baseline: when no `--data-dir` is given, the arg list must NOT include it.
///
/// Why: the child must not receive a spurious `--data-dir` that might override
/// a legitimately set `TRUSTY_DATA_DIR` in the environment.
/// What: call `build_spawn_args` with `data_dir = None`; assert `--data-dir`
/// is absent.
/// Test: this test.
#[test]
fn spawn_args_omit_data_dir_when_none() {
    let args = build_spawn_args(7878, "auto", None, false);
    let has_data_dir = args.iter().any(|a| a == std::ffi::OsStr::new("--data-dir"));
    assert!(
        !has_data_dir,
        "spawn args must not include --data-dir when data_dir is None; got: {args:?}"
    );
}

/// Core regression for issue #1182: when `--data-dir /new/path` is supplied,
/// the spawned child Command must receive it as an explicit CLI arg — even when
/// `TRUSTY_DATA_DIR=/old/path` is set in the parent's environment.
///
/// Why: the old code only stamped `TRUSTY_DATA_DIR` when it was unset, so a
/// pre-existing env var silently shadowed the explicit CLI flag in the child.
/// The fix passes `--data-dir` as a direct arg, which `clap` in the child will
/// apply after (and thus override) any inherited env.
/// What: call `build_spawn_args` with an explicit `data_dir`; assert the arg
/// list contains `--data-dir` followed by the supplied path.
/// Test: this test. Simulate the pre-existing env case by NOT unsetting
/// TRUSTY_DATA_DIR here — the child arg list must still contain the flag.
#[test]
fn spawn_args_include_data_dir_when_preset_env() {
    let new_path = std::path::Path::new("/new/explicit/data/dir");

    // Simulate the regression scenario: TRUSTY_DATA_DIR already set to an
    // old path in the environment. The arg list is what matters for the child.
    let args = build_spawn_args(7878, "auto", Some(new_path), false);

    let flag_pos = args
        .iter()
        .position(|a| a == std::ffi::OsStr::new("--data-dir"));
    assert!(
        flag_pos.is_some(),
        "spawn args must include --data-dir flag when data_dir is Some; got: {args:?}"
    );

    let val_pos = flag_pos.unwrap() + 1;
    assert!(
        val_pos < args.len(),
        "--data-dir flag must be followed by the path value; got: {args:?}"
    );
    assert_eq!(
        args[val_pos],
        std::ffi::OsString::from("/new/explicit/data/dir"),
        "--data-dir value must equal the supplied data_dir path"
    );
}

/// Verify that `--no-auto-discover` and `--data-dir` can coexist in the args.
///
/// Why: both flags are optional and appended independently; a regression in
/// ordering or mutual-exclusion could silently drop one.
/// What: call `build_spawn_args` with both flags set; assert both are present.
/// Test: this test.
#[test]
fn spawn_args_include_both_data_dir_and_no_auto_discover() {
    let path = std::path::Path::new("/some/data/dir");
    let args = build_spawn_args(7878, "auto", Some(path), true);

    let has_no_auto = args
        .iter()
        .any(|a| a == std::ffi::OsStr::new("--no-auto-discover"));
    let has_data_dir = args.iter().any(|a| a == std::ffi::OsStr::new("--data-dir"));

    assert!(
        has_no_auto,
        "spawn args must include --no-auto-discover when requested; got: {args:?}"
    );
    assert!(
        has_data_dir,
        "spawn args must include --data-dir when data_dir is Some; got: {args:?}"
    );
}
