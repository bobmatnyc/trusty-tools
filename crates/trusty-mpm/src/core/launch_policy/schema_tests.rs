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

/// Q7: a table no PR has shipped is an unknown key, so the file is bad.
/// #8992: `[skills]` and `[tools]` shipped; `[agents]` and `[launch]` have not.
#[test]
fn a_pr2_table_is_an_unknown_key() {
    for body in ["[agents]\n", "[launch]\n"] {
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

/// #8992: exact skill names, plugin-namespaced names, bare tool names and
/// `mcp__server[__tool]` names make a good file.
#[test]
fn good_skills_and_tools_tables_parse() {
    let body = "[skills]\noff = [\"xlsx\", \"breeze:voice\", \"a.b_c-d\"]\n\
                [tools]\ndeny = [\"NotebookEdit\", \"mcp__heygen\", \"mcp__slack-mcp__post\"]\n";
    assert!(parse(body).is_ok(), "{:?}", parse(body).err());
    assert!(parse("[skills]\noff = []\n[tools]\ndeny = []\n").is_ok());
    assert!(parse("[skills]\n[tools]\n").is_ok());
}

/// #8992: every forbidden `[skills] off` / `[tools] deny` value is a bad file
/// that names its key — a floor name, a wildcard, an empty name, a
/// parenthesised rule (Q-C: bare names only, so `Agent(engineer)` too).
#[test]
fn a_forbidden_skills_or_tools_value_is_a_bad_file_naming_its_key() {
    let cases: &[(&str, &str)] = &[
        ("tools.deny", "Agent"),
        ("tools.deny", "Skill"),
        ("tools.deny", "Bash"),
        ("tools.deny", "bash"),
        ("tools.deny", "mcp__trusty-search"),
        ("tools.deny", "mcp__claude-in-chrome__x"),
        ("tools.deny", "*"),
        ("tools.deny", "Web*"),
        ("tools.deny", ""),
        ("tools.deny", " "),
        ("tools.deny", "mcp__"),
        ("tools.deny", "Bash(rm:*)"),
        ("tools.deny", "Agent(engineer)"),
        ("tools.deny", "Task(engineer)"),
        ("tools.deny", "WebFetch(domain:example.com)"),
        ("skills.off", "tm-workflow"),
        ("skills.off", "tm-anything-new"),
        ("skills.off", "systematic-debugging"),
        ("skills.off", "*"),
        ("skills.off", "xl?x"),
        ("skills.off", ""),
        ("skills.off", "two words"),
        ("skills.off", "a/b"),
    ];
    for (key, value) in cases {
        let (table, field) = key.split_once('.').expect("table.field");
        let body = format!("[{table}]\n{field} = [{value:?}]\n");
        let err = parse(&body).expect_err(&format!("{body:?} must be a bad file"));
        assert!(
            matches!(&err, OwnerPolicyError::Invalid { key: k, .. } if k == key),
            "{body:?}: {err}"
        );
        assert!(err.to_string().contains(key), "{body:?}: {err}");
    }
}

/// #8992: a wrong type, or a key the new tables do not define (`on`, `allow`,
/// `ask`), is a malformed file whose message names the problem.
#[test]
fn a_wrong_type_or_unknown_key_in_the_new_tables_is_malformed() {
    for (body, needle) in [
        ("[skills]\noff = \"xlsx\"\n", "invalid type"),
        ("[tools]\ndeny = 3\n", "invalid type"),
        ("[skills]\non = [\"xlsx\"]\n", "unknown field `on`"),
        ("[tools]\nallow = [\"Read\"]\n", "unknown field `allow`"),
        ("[tools]\nask = [\"Read\"]\n", "unknown field `ask`"),
    ] {
        let err = parse(body).expect_err(body);
        assert!(
            matches!(err, OwnerPolicyError::Malformed { .. }),
            "{body:?}: {err}"
        );
        assert!(err.to_string().contains(needle), "{body:?}: {err}");
    }
}
