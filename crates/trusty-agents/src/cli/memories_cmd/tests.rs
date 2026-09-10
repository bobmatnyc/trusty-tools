//! Compatibility parsing and explicit retirement of local memory import/export.
use super::{Command, parse_args};
use std::path::PathBuf;
#[test]
fn parses_export_with_session_and_output() {
    let cmd = parse_args(&["export", "--session", "sess-1", "--output", "/tmp/x.jsonl"]).unwrap();
    assert_eq!(
        cmd,
        Command::Export {
            session: Some("sess-1".to_string()),
            output: Some(PathBuf::from("/tmp/x.jsonl")),
            segment: None,
        }
    );
}

#[test]
fn parses_export_with_no_args() {
    let cmd = parse_args(&["export"]).unwrap();
    assert_eq!(
        cmd,
        Command::Export {
            session: None,
            output: None,
            segment: None,
        }
    );
}

#[test]
fn parses_export_with_segment_filter() {
    let cmd = parse_args(&["export", "--segment", "brief"]).unwrap();
    assert_eq!(
        cmd,
        Command::Export {
            session: None,
            output: None,
            segment: Some("brief".to_string()),
        }
    );
}

#[test]
fn rejects_invalid_export_segment() {
    // Unknown segment names must error before we open any stores.
    assert!(parse_args(&["export", "--segment", "bogus"]).is_err());
}

#[test]
fn parses_import_from_committed() {
    let cmd = parse_args(&["import", "--from-committed"]).unwrap();
    assert_eq!(
        cmd,
        Command::Import {
            input: None,
            from_committed: true,
        }
    );
}

#[test]
fn parses_list_with_scope() {
    let cmd = parse_args(&["list", "--scope", "imported"]).unwrap();
    assert_eq!(
        cmd,
        Command::List {
            scope: "imported".to_string()
        }
    );
}

#[test]
fn parses_list_default_scope_is_session() {
    let cmd = parse_args(&["list"]).unwrap();
    assert_eq!(
        cmd,
        Command::List {
            scope: "session".to_string()
        }
    );
}

#[test]
fn rejects_invalid_scope() {
    assert!(parse_args(&["list", "--scope", "bogus"]).is_err());
}

#[test]
fn rejects_unknown_action() {
    assert!(parse_args(&["foo"]).is_err());
}

#[tokio::test]
async fn legacy_memories_dispatch_never_opens_local_stores() {
    for command in ["export", "import", "list"] {
        let error = super::run_memories_command(&[command.to_string()])
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Local memories import/export is retired")
        );
    }
}
