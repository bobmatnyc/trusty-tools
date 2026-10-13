//! Tests for the owner file schema and its validation.

use super::*;
use tempfile::TempDir;

fn parse(body: &str) -> Result<Option<(OwnerFile, Vec<u8>)>, OwnerPolicyError> {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join(OWNER_FILE);
    std::fs::write(&path, body).unwrap();
    parse_owner_file(&path)
}

#[test]
fn an_empty_owner_file_is_a_valid_policy() {
    let (file, _) = parse("").unwrap().unwrap();
    assert_eq!(file, OwnerFile::default());
}

#[test]
fn an_absent_owner_file_is_none() {
    let tmp = TempDir::new().unwrap();
    assert!(
        parse_owner_file(&tmp.path().join(OWNER_FILE))
            .unwrap()
            .is_none()
    );
}

/// #8988: a dangling symlink is a present-but-unreadable file, never an absent one.
#[cfg(unix)]
#[test]
fn a_dangling_symlink_owner_file_is_unreadable_not_absent() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join(OWNER_FILE);
    std::os::unix::fs::symlink(tmp.path().join("missing-target"), &path).unwrap();
    let err = parse_owner_file(&path).expect_err("a dangling symlink must be a bad file");
    assert!(matches!(err, OwnerPolicyError::Unreadable { .. }), "{err}");
}

/// Q5: `allow = []` is a policy, not an error.
#[test]
fn an_empty_allow_list_is_a_valid_policy() {
    let (file, _) = parse("[mcp]\nallow = []\n").unwrap().unwrap();
    assert_eq!(file.mcp.unwrap().allow, Some(vec![]));
}

/// Q4: both keys may share the table.
#[test]
fn allow_and_deny_may_share_the_table() {
    let (file, _) = parse("extends = \"bob\"\n[mcp]\nallow = [\"apex\"]\ndeny = [\"heygen\"]\nclaude_ai_connectors = false\n")
        .unwrap()
        .unwrap();
    let mcp = file.mcp.unwrap();
    assert_eq!(file.extends.as_deref(), Some("bob"));
    assert_eq!(mcp.allow, Some(vec!["apex".to_string()]));
    assert_eq!(mcp.deny, Some(vec!["heygen".to_string()]));
    assert_eq!(mcp.claude_ai_connectors, Some(false));
}

/// Q7: a PR 2 table is an unknown key, so the file is bad.
#[test]
fn a_pr2_table_is_an_unknown_key() {
    for body in [
        "[tools]\ndeny = []\n",
        "[skills]\n",
        "[agents]\n",
        "[launch]\n",
    ] {
        assert!(
            matches!(parse(body), Err(OwnerPolicyError::Malformed { .. })),
            "{body:?} must be a bad file"
        );
    }
}

#[test]
fn an_unknown_mcp_key_is_a_bad_file() {
    assert!(matches!(
        parse("[mcp]\nalow = []\n"),
        Err(OwnerPolicyError::Malformed { .. })
    ));
}

#[test]
fn a_parse_error_is_a_bad_file() {
    assert!(matches!(
        parse("[mcp\n"),
        Err(OwnerPolicyError::Malformed { .. })
    ));
}

#[test]
fn a_wildcard_is_a_bad_file() {
    for body in [
        "[mcp]\ndeny = [\"*\"]\n",
        "[mcp]\nallow = [\"duetto-*\"]\n",
        "[mcp]\ndeny = [\"mcp__?\"]\n",
    ] {
        let err = parse(body).unwrap_err();
        assert!(err.to_string().contains("wildcard"), "{body:?}: {err}");
    }
}

/// Q2: a floor server in `deny` is a bad file — builtins and chrome alike.
#[test]
fn a_floor_server_in_deny_is_a_bad_file() {
    for name in ["trusty-search", "claude-in-chrome"] {
        let err = parse(&format!("[mcp]\ndeny = [\"{name}\"]\n")).unwrap_err();
        assert!(err.to_string().contains("never-deny floor"), "{err}");
    }
}

#[test]
fn a_floor_server_in_allow_is_harmless() {
    assert!(parse("[mcp]\nallow = [\"trusty-search\"]\n").is_ok());
}

#[test]
fn an_extends_value_that_is_not_an_owner_name_is_a_bad_file() {
    for parent in ["", "../etc", "a/b", ".hidden"] {
        let err = parse(&format!("extends = \"{parent}\"\n")).unwrap_err();
        assert!(err.to_string().contains("extends"), "{parent:?}: {err}");
    }
}

#[test]
fn a_non_utf8_file_is_unreadable() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join(OWNER_FILE);
    std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
    assert!(matches!(
        parse_owner_file(&path),
        Err(OwnerPolicyError::Unreadable { .. })
    ));
}

/// An EACCES read is a bad file, not an absent one. Skipped as root, which
/// reads through mode 000.
#[cfg(unix)]
#[test]
fn an_unreadable_file_is_a_bad_file() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join(OWNER_FILE);
    std::fs::write(&path, "").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&path).is_ok() {
        return; // running as root
    }
    assert!(matches!(
        parse_owner_file(&path),
        Err(OwnerPolicyError::Unreadable { .. })
    ));
}
