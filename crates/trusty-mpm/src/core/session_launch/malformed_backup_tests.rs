//! Unit tests for [`super`] (preserve-then-rewrite a malformed `settings.json`, #7780).
//!
//! Why: split out of `malformed_backup.rs` so the production file stays under
//! the SLOC cap, matching the `backup.rs` / `backup_tests.rs` split this crate
//! already uses for the sibling snapshot writer.
//! What: pins the clock through `load_settings_object_at` to exercise the
//! pass-through, absent-file, whitespace-only, unparseable, valid-non-object,
//! same-second collision and fail-closed-copy arms.
//! Test: this module IS the test suite for `super`.

use super::*;
use tempfile::TempDir;

/// A fixed clock, so a copy's name never races a second boundary.
fn at(stamp: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(stamp)
        .expect("test stamp is valid RFC 3339")
        .with_timezone(&Utc)
}

/// Every preserved copy of `settings.json` in `dir`, name-sorted.
fn copies(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("readable temp dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("settings.json.malformed-"))
        .collect();
    names.sort();
    names
}

/// Seed `<tmp>/claude/settings.json` with `body` and return the directory and
/// the file path.
fn seed(tmp: &TempDir, body: &[u8]) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = tmp.path().join("claude");
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("settings.json");
    std::fs::write(&path, body).unwrap();
    (dir, path)
}

/// A well-formed file is the common case and must cost nothing: no copy, and
/// every key handed back for the caller to merge into.
#[test]
fn returns_a_json_object_untouched() {
    let tmp = TempDir::new().unwrap();
    let (dir, path) = seed(&tmp, br#"{"permissions": {"allow": ["Bash"]}}"#);

    let value = load_settings_object_at(&path, at("2026-09-13T10:00:00Z")).expect("object loads");

    assert_eq!(value["permissions"]["allow"][0], "Bash");
    assert!(copies(&dir).is_empty(), "a valid object must not be copied");
}

/// The first launch in a project has no settings file; that is not damage and
/// must not produce a copy.
#[test]
fn treats_a_missing_file_as_empty() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("claude");
    std::fs::create_dir(&dir).unwrap();

    let value = load_settings_object_at(&dir.join("settings.json"), at("2026-09-13T10:00:00Z"))
        .expect("an absent file is not an error");

    assert_eq!(value, serde_json::json!({}));
    assert!(copies(&dir).is_empty(), "nothing existed to copy");
}

/// #7789: a file that is empty or all whitespace holds nothing to preserve, and
/// the managed hook writer this loader now also serves has always read one as
/// `{}`. A zero-byte copy would be a record of nothing, under a name claiming
/// to be the settings the operator lost.
#[test]
fn treats_a_whitespace_only_file_as_empty() {
    let tmp = TempDir::new().unwrap();
    let (dir, path) = seed(&tmp, b"  \n\t\n");

    let value = load_settings_object_at(&path, at("2026-09-13T10:00:00Z"))
        .expect("a whitespace-only file is not an error");

    assert_eq!(value, serde_json::json!({}));
    assert!(copies(&dir).is_empty(), "nothing was worth copying");
}

/// The reported incident: a file holding `{ broken` was replaced by tm's keys
/// with no warning and no copy. The bytes must now survive under a stamped
/// sibling, byte for byte.
#[test]
fn backs_up_unparseable_bytes_under_a_stamped_name() {
    let tmp = TempDir::new().unwrap();
    let (dir, path) = seed(&tmp, b"{ broken");

    let value = load_settings_object_at(&path, at("2026-09-13T10:00:00Z"))
        .expect("a copied-aside file is not an error");

    assert_eq!(
        value,
        serde_json::json!({}),
        "the caller rewrites from {{}}"
    );
    assert_eq!(
        copies(&dir),
        vec!["settings.json.malformed-20260913T100000Z".to_string()]
    );
    assert_eq!(
        std::fs::read(dir.join("settings.json.malformed-20260913T100000Z")).unwrap(),
        b"{ broken".as_slice(),
        "the copy must hold the ORIGINAL bytes"
    );
}

/// A file that is valid JSON but not an object is malformed for this purpose —
/// every writer indexes into it as a map. It must be copied aside and replaced,
/// never panic.
#[test]
fn backs_up_a_valid_non_object() {
    for body in [br#"[1, 2, 3]"#.as_slice(), br#""a string""#, b"7", b"null"] {
        let tmp = TempDir::new().unwrap();
        let (dir, path) = seed(&tmp, body);

        let value = load_settings_object_at(&path, at("2026-09-13T10:00:00Z"))
            .expect("a non-object is copied aside, not refused");

        assert_eq!(value, serde_json::json!({}), "body: {body:?}");
        assert_eq!(copies(&dir).len(), 1, "body: {body:?}");
        assert_eq!(
            std::fs::read(dir.join("settings.json.malformed-20260913T100000Z")).unwrap(),
            body,
            "body: {body:?}"
        );
    }
}

/// Two copies claimed inside one wall-clock second must not collapse into one:
/// the second launch's copy would otherwise overwrite the first launch's, which
/// is the state an operator actually wants back.
#[test]
fn never_overwrites_a_copy_taken_in_the_same_second() {
    let tmp = TempDir::new().unwrap();
    let (dir, path) = seed(&tmp, b"{ first");

    load_settings_object_at(&path, at("2026-09-13T10:00:00Z")).expect("first copy");
    std::fs::write(&path, b"{ second").unwrap();
    load_settings_object_at(&path, at("2026-09-13T10:00:00Z")).expect("second copy");

    assert_eq!(
        copies(&dir),
        vec![
            "settings.json.malformed-20260913T100000Z".to_string(),
            "settings.json.malformed-20260913T100000Z-1".to_string(),
        ]
    );
    assert_eq!(
        std::fs::read(dir.join("settings.json.malformed-20260913T100000Z")).unwrap(),
        b"{ first".as_slice(),
        "the first copy must survive the second claim"
    );
}

/// The fail-open check (#7780): a rewrite whose prior state cannot be preserved
/// does not happen. Unix-only — the read-only directory bit is the portable way
/// to deny file creation while leaving the existing file itself writable.
#[cfg(unix)]
#[test]
fn refuses_when_the_copy_cannot_be_written() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let (dir, path) = seed(&tmp, b"{ broken");

    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = load_settings_object_at(&path, at("2026-09-13T10:00:00Z"));
    // Restore before asserting, so a failed assertion still leaves a removable
    // temp dir behind.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    let err = result.expect_err("an uncopyable original must refuse the load");
    assert!(
        matches!(err, PrepError::SettingsBackup { .. }),
        "unexpected error: {err}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"{ broken".as_slice(),
        "a refused load must not disturb the original"
    );
    assert!(
        copies(&dir).is_empty(),
        "no partial copy may be left behind"
    );
}

/// An unreadable file is refused for the same reason: contents that were never
/// seen cannot be preserved by anything downstream (#7490's rule, reused).
#[cfg(unix)]
#[test]
fn refuses_an_unreadable_file() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let (_dir, path) = seed(&tmp, b"{ broken");

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let result = load_settings_object_at(&path, at("2026-09-13T10:00:00Z"));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let err = result.expect_err("an unreadable file must refuse the load");
    assert!(
        matches!(err, PrepError::SettingsBackup { .. }),
        "unexpected error: {err}"
    );
}

/// #7780's second closure condition: the operator is TOLD where their bytes
/// went. Without this the `backup` field could be dropped from the warning and
/// every other test in this file would still pass, leaving a copy nobody can
/// locate. Serial because [`crate::test_support::enable_event_capture`] raises
/// the process-global tracing level.
#[test]
#[serial_test::serial]
fn warns_with_the_path_of_the_copy_it_took() {
    use tracing_subscriber::layer::SubscriberExt;

    let tmp = TempDir::new().unwrap();
    let (dir, path) = seed(&tmp, b"{ broken");

    // #4931: `with_default` is thread-local and never raises the process-global
    // MAX_LEVEL, so without this the capture records nothing.
    crate::test_support::enable_event_capture();
    let buffer = trusty_common::log_buffer::LogBuffer::new(16);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    tracing::subscriber::with_default(subscriber, || {
        load_settings_object_at(&path, at("2026-09-13T10:00:00Z")).expect("copied aside");
    });

    let copy = dir.join("settings.json.malformed-20260913T100000Z");
    assert_eq!(copies(&dir).len(), 1, "exactly one copy was taken");
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

/// The named wrapper the writer that does not need the value calls. It must take
/// the same copy and refuse on the same terms as the loader — a wrapper that
/// quietly swallowed the error would re-open #7780 for its caller.
#[test]
fn preserve_if_malformed_copies_aside_and_refuses_when_it_cannot() {
    let tmp = TempDir::new().unwrap();
    let (dir, path) = seed(&tmp, b"{ broken");

    preserve_if_malformed(&path).expect("a copied-aside file is not an error");
    assert_eq!(copies(&dir).len(), 1, "the wrapper must take the copy");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = preserve_if_malformed(&path);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        let err = result.expect_err("an uncopyable original must refuse");
        assert!(
            matches!(err, PrepError::SettingsBackup { .. }),
            "unexpected error: {err}"
        );
    }
}
