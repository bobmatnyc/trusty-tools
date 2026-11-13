//! Tests for the console token loader (#11074).
//!
//! Every refusal test also asserts the error text names the path and holds no
//! byte of the file, so a refusal printed at startup cannot leak the token.

use super::*;
use std::os::unix::fs::PermissionsExt;

/// A token long enough to pass, distinct enough to grep for.
const GOOD: &str = "c0ffee11074c0ffee11074c0ffee11074c0ffee11074";

fn write_token(dir: &tempfile::TempDir, body: &[u8], mode: u32) -> PathBuf {
    let path = dir.path().join("console-token");
    std::fs::write(&path, body).expect("write token file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    path
}

/// Assert the refusal names `path` and quotes no part of `secret`.
fn assert_names_path_not_content(err: &TokenFileError, path: &Path, secret: &str) {
    for text in [err.to_string(), format!("{err:?}")] {
        assert!(
            text.contains(&path.display().to_string()),
            "error must name the path: {text}"
        );
        assert!(
            !secret.is_empty() && !text.contains(secret),
            "error must not quote the file: {text}"
        );
    }
}

#[test]
fn load_from_var_unset_is_off() {
    assert!(matches!(load_from_var(None), Ok(None)));
}

#[test]
fn load_from_var_empty_is_refused() {
    assert_eq!(
        load_from_var(Some(OsString::new())).map(|t| t.is_some()),
        Err(TokenFileError::EmptyVariable)
    );
}

#[test]
fn valid_file_loads_with_its_newline_trimmed() {
    let dir = tempfile::tempdir().expect("tempdir");
    for mode in [0o400, 0o600] {
        let path = write_token(&dir, format!("{GOOD}\n").as_bytes(), mode);
        let token = load_from_var(Some(path.clone().into_os_string()))
            .expect("valid file loads")
            .expect("set variable turns the token on");
        assert_eq!(token.expose(), GOOD);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }
}

#[test]
fn matches_only_the_exact_token() {
    let dir = tempfile::tempdir().expect("tempdir");
    let token = load_from_path(&write_token(&dir, GOOD.as_bytes(), 0o600)).expect("load");
    assert!(token.matches(GOOD));
    assert!(!token.matches(&GOOD[1..]));
    assert!(!token.matches(&format!("{GOOD}x")));
    assert!(!token.matches(""));
    let mut flipped = GOOD.to_string();
    flipped.replace_range(..1, "d");
    assert!(!token.matches(&flipped));
}

#[test]
fn debug_is_redacted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let token = load_from_path(&write_token(&dir, GOOD.as_bytes(), 0o600)).expect("load");
    let shown = format!("{token:?}");
    assert!(!shown.contains(GOOD), "Debug leaked the token: {shown}");
    assert_eq!(shown, "ConsoleToken(<redacted>)");
}

#[test]
fn missing_file_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("absent");
    let err = load_from_path(&path).expect_err("missing file must refuse");
    assert_eq!(err, TokenFileError::Missing { path: path.clone() });
    assert!(err.to_string().contains("does not exist"));
}

#[test]
fn directory_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = load_from_path(dir.path()).expect_err("directory must refuse");
    assert_eq!(
        err,
        TokenFileError::NotAFile {
            path: dir.path().to_path_buf()
        }
    );
}

#[test]
fn group_readable_file_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_token(&dir, GOOD.as_bytes(), 0o640);
    let err = load_from_path(&path).expect_err("group-readable must refuse");
    assert!(matches!(
        err,
        TokenFileError::GroupOrOtherAccess { mode: 0o640, .. }
    ));
    assert_names_path_not_content(&err, &path, GOOD);
    assert!(err.to_string().contains("0640"), "{err}");
}

#[test]
fn other_readable_file_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_token(&dir, GOOD.as_bytes(), 0o604);
    let err = load_from_path(&path).expect_err("other-readable must refuse");
    assert!(matches!(
        err,
        TokenFileError::GroupOrOtherAccess { mode: 0o604, .. }
    ));
    assert_names_path_not_content(&err, &path, GOOD);
}

#[test]
fn short_token_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let short = &GOOD[..MIN_TOKEN_BYTES - 1];
    // A trailing newline must not count toward the minimum.
    let path = write_token(&dir, format!("{short}\n").as_bytes(), 0o600);
    let err = load_from_path(&path).expect_err("short token must refuse");
    assert_eq!(err, TokenFileError::TooShort { path: path.clone() });
    assert_names_path_not_content(&err, &path, short);
    // Exactly the minimum passes.
    let exact = write_token(&dir, &GOOD.as_bytes()[..MIN_TOKEN_BYTES], 0o600);
    assert!(load_from_path(&exact).is_ok());
}

#[test]
fn empty_file_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_token(&dir, b"\n", 0o600);
    assert_eq!(
        load_from_path(&path).map(|_| ()),
        Err(TokenFileError::TooShort { path })
    );
}

#[test]
fn long_file_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let body = "a".repeat(MAX_TOKEN_BYTES + 1);
    let path = write_token(&dir, body.as_bytes(), 0o600);
    let err = load_from_path(&path).expect_err("long file must refuse");
    assert_eq!(err, TokenFileError::TooLong { path: path.clone() });
    assert_names_path_not_content(&err, &path, &body[..64]);
}

#[test]
fn inner_space_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let body = format!("{} {}", &GOOD[..20], &GOOD[20..]);
    let path = write_token(&dir, body.as_bytes(), 0o600);
    let err = load_from_path(&path).expect_err("inner space must refuse");
    assert_eq!(err, TokenFileError::NotVisibleAscii { path: path.clone() });
    assert_names_path_not_content(&err, &path, &GOOD[..20]);
}

#[test]
fn unreadable_file_names_the_kind_not_the_content() {
    // root reads a 0000 file regardless, so the check only holds for a user.
    // SAFETY: `geteuid` has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_token(&dir, GOOD.as_bytes(), 0o000);
    let err = load_from_path(&path).expect_err("unreadable must refuse");
    assert!(matches!(err, TokenFileError::Unreadable { .. }), "{err:?}");
    assert_names_path_not_content(&err, &path, GOOD);
}
