//! Call-site coverage for #7789: both managed-tier writers of
//! `<claude_config_dir>/settings.json` preserve a malformed file before
//! replacing it.
//!
//! Why a separate file rather than asserts inside each writer's own tests: the
//! #7780 loader is already unit-tested in
//! `core::session_launch::malformed_backup_tests`; what this proves is that the
//! managed tier is WIRED to it. Before this fix both writers read the file with
//! `.ok().filter(is_object)` and rewrote from `{}`, so an unparseable managed
//! `settings.json` came back holding tm's own keys alone — with no warning and
//! no copy, and with only the 3-deep pruned `.bak` ring as recovery.
//! What: three arms per writer — a malformed file is copied aside byte-for-byte
//! and the rewrite still happens; a copy that cannot be written abandons the
//! write and leaves the original exactly as it was; a valid object costs no
//! copy. Plus one arm asserting the warning names the copy, since every other
//! assertion here passes with the `backup` field dropped from it, and one arm
//! running BOTH writers through `ensure_global_config_dir_with_exe` to pin the
//! copy count at one per provisioning run.
//! Test: this module IS the test suite.

use crate::core::session_launch::PrepError;
use crate::test_support::hermetic_temp_dir;
use std::path::{Path, PathBuf};

/// The bytes from the #7780 incident probe, reused here — the reported shape of
/// a hand-edit interrupted mid-save.
const BROKEN: &[u8] = b"{ broken";

/// An absolute, non-ephemeral path whose stem is one tm ships.
/// `resolve_stable_hook_exe` accepts it without stat'ing the file, so a host
/// with no `tm` installed still exercises the write rather than the refusal.
const TEST_EXE: &str = "/usr/local/bin/tm";

/// Seed `<dir>/settings.json` with `body` and return its path.
fn seed(dir: &Path, body: &[u8]) -> PathBuf {
    let path = dir.join("settings.json");
    std::fs::write(&path, body).expect("seed settings.json");
    path
}

/// Every preserved copy of `settings.json` in `dir`, name-sorted.
fn copies(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("readable config dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("settings.json.malformed-"))
        .collect();
    names.sort();
    names
}

/// Read `<dir>/settings.json` as JSON, failing the test when it is not an object.
fn read_settings(dir: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(dir.join("settings.json"))
        .expect("settings.json must exist after a write");
    let value: serde_json::Value =
        serde_json::from_str(&text).expect("the rewritten file must be valid JSON");
    assert!(
        value.is_object(),
        "rewritten file must be an object: {text}"
    );
    value
}

/// Assert the sole copy in `dir` holds `BROKEN` byte-for-byte.
fn assert_one_copy_holds_the_original(dir: &Path) {
    let names = copies(dir);
    assert_eq!(names.len(), 1, "exactly one copy expected: {names:?}");
    assert_eq!(
        std::fs::read(dir.join(&names[0])).expect("copy is readable"),
        BROKEN,
        "the copy must hold the ORIGINAL bytes, not a re-serialization"
    );
}

/// Run `f` with `dir` non-writable, restoring the mode before returning, so a
/// failed assertion still leaves a removable temp dir behind.
///
/// #7762: the settings lock's sidecar is created here, BEFORE the mode change.
/// A non-writable directory refuses a new entry, so without this the writer
/// would fail at the lock rather than at the copy — and these tests are about
/// the copy. Opening an existing file for write needs permission on the FILE,
/// not on its directory, so the lock is still acquirable while the copy is not.
#[cfg(unix)]
fn with_unwritable_dir<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(dir.join("settings.json.lock"), b"").expect("pre-create the lock sidecar");
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let out = f();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    out
}

#[test]
fn ensure_settings_defaults_backs_up_a_malformed_file_before_rewriting_it() {
    let tmp = hermetic_temp_dir();
    let cfg = tmp.path();
    seed(cfg, BROKEN);

    super::settings_defaults::ensure_settings_defaults(cfg)
        .expect("the rewrite proceeds once the copy is taken");

    assert_one_copy_holds_the_original(cfg);
    assert!(
        read_settings(cfg)["outputStyle"].is_string(),
        "the defaults must still be seeded over the damage"
    );
}

/// The fail-open check. A non-writable config dir denies creating the copy
/// while leaving `settings.json` itself replaceable — the exact shape in which
/// the pre-#7789 writer destroyed it.
#[cfg(unix)]
#[test]
fn ensure_settings_defaults_refuses_when_the_copy_cannot_be_written() {
    let tmp = hermetic_temp_dir();
    let cfg = tmp.path();
    let path = seed(cfg, BROKEN);

    let result = with_unwritable_dir(cfg, || {
        super::settings_defaults::ensure_settings_defaults(cfg)
    });

    let err = result.expect_err("a rewrite whose copy failed must not proceed");
    assert!(
        matches!(
            err.downcast_ref::<PrepError>(),
            Some(PrepError::SettingsBackup { .. })
        ),
        "unexpected error: {err:#}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        BROKEN,
        "the original must be left exactly as it was"
    );
    assert!(copies(cfg).is_empty(), "no partial copy may be left behind");
}

#[test]
fn ensure_settings_defaults_takes_no_copy_for_a_valid_file() {
    let tmp = hermetic_temp_dir();
    let cfg = tmp.path();
    seed(cfg, br#"{"outputStyle":"mine"}"#);

    super::settings_defaults::ensure_settings_defaults(cfg).expect("a valid object merges");

    assert!(
        copies(cfg).is_empty(),
        "a well-formed file must cost nothing: {:?}",
        copies(cfg)
    );
    assert_eq!(
        read_settings(cfg)["outputStyle"],
        serde_json::json!("mine"),
        "an operator value must survive"
    );
}

#[test]
fn write_project_hooks_backs_up_a_malformed_managed_file() {
    let tmp = hermetic_temp_dir();
    let cfg = tmp.path();
    let path = seed(cfg, BROKEN);

    let wrote = super::hooks::write_project_hooks(&path, Some(Path::new(TEST_EXE)))
        .expect("the hook merge proceeds once the copy is taken");

    assert!(wrote, "the damaged file must be replaced, not left as-is");
    assert_one_copy_holds_the_original(cfg);
    assert!(
        read_settings(cfg)["hooks"]["PreToolUse"].is_array(),
        "the hook triad must still be installed over the damage"
    );
}

#[cfg(unix)]
#[test]
fn write_project_hooks_refuses_when_the_copy_cannot_be_written() {
    let tmp = hermetic_temp_dir();
    let cfg = tmp.path();
    let path = seed(cfg, BROKEN);

    let result = with_unwritable_dir(cfg, || {
        super::hooks::write_project_hooks(&path, Some(Path::new(TEST_EXE)))
    });

    let err = result.expect_err("a rewrite whose copy failed must not proceed");
    assert!(
        matches!(
            err.downcast_ref::<PrepError>(),
            Some(PrepError::SettingsBackup { .. })
        ),
        "unexpected error: {err:#}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        BROKEN,
        "the original must be left exactly as it was"
    );
    assert!(copies(cfg).is_empty(), "no partial copy may be left behind");
}

#[test]
fn write_project_hooks_takes_no_copy_for_a_valid_file() {
    let tmp = hermetic_temp_dir();
    let cfg = tmp.path();
    let path = seed(cfg, br#"{"env":{"KEEP":"1"}}"#);

    super::hooks::write_project_hooks(&path, Some(Path::new(TEST_EXE))).expect("a valid object");

    assert!(
        copies(cfg).is_empty(),
        "a well-formed file must cost nothing: {:?}",
        copies(cfg)
    );
    assert_eq!(
        read_settings(cfg)["env"]["KEEP"],
        serde_json::json!("1"),
        "an operator key must survive the merge"
    );
}

/// One copy per provisioning run, not one per writer.
///
/// Why (#7789): `ensure_global_config_dir_with_exe` calls
/// `ensure_settings_defaults` and then `ensure_managed_hooks_with_exe`, both of
/// which now preserve-then-rewrite. Exactly one copy results only because the
/// first ALWAYS rewrites after a copy — the seeded object can never equal `{}`
/// — so the second reads a valid object and finds nothing to preserve. Swapping
/// that order, or letting the first skip its rewrite, would start taking two
/// copies per launch with every other arm in this module still green.
/// What: provisions a managed config dir over a `{ broken` `settings.json`
/// through the production entry point, pinning the hook binary the way the
/// `global_config` test shim does, then asserts exactly one copy holding the
/// original bytes and that BOTH writers' keys landed in the rewrite.
/// Test: this IS the test.
#[test]
fn provisioning_takes_exactly_one_copy_when_both_managed_writers_fire() {
    let tmp = hermetic_temp_dir();
    let managed_root = tmp.path().join("managed");
    let cfg = managed_root.join("claude-config");
    std::fs::create_dir_all(&cfg).expect("create the managed config dir");
    seed(&cfg, BROKEN);

    super::global_config::ensure_global_config_dir_with_exe(
        &managed_root,
        &cfg,
        Some(Path::new(TEST_EXE)),
    )
    .expect("provisioning proceeds once the copy is taken");

    assert_one_copy_holds_the_original(&cfg);
    let settings = read_settings(&cfg);
    assert!(
        settings["outputStyle"].is_string(),
        "the defaults writer must have rewritten over the damage: {settings}"
    );
    assert!(
        settings["hooks"]["PreToolUse"].is_array(),
        "the hook writer must have merged into that rewrite: {settings}"
    );
}

/// #7789's second closure condition, on the managed tier: the operator is TOLD
/// where their bytes went. Serial because
/// [`crate::test_support::enable_event_capture`] raises the process-global
/// tracing level.
#[test]
#[serial_test::serial]
fn the_managed_warning_names_the_copy_it_took() {
    use tracing_subscriber::layer::SubscriberExt;

    let tmp = hermetic_temp_dir();
    let cfg = tmp.path().to_path_buf();
    seed(&cfg, BROKEN);

    // #4931: `with_default` is thread-local and never raises the process-global
    // MAX_LEVEL, so without this the capture records nothing.
    crate::test_support::enable_event_capture();
    let buffer = trusty_common::log_buffer::LogBuffer::new(16);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    tracing::subscriber::with_default(subscriber, || {
        super::settings_defaults::ensure_settings_defaults(&cfg).expect("copied aside");
    });

    let names = copies(&cfg);
    assert_eq!(names.len(), 1, "exactly one copy was taken: {names:?}");
    let copy = cfg.join(&names[0]);
    let lines = buffer.tail(16);
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&copy.display().to_string())),
        "the warning must name the copy at {}, else the operator cannot recover \
         their bytes: {lines:#?}",
        copy.display()
    );
}
