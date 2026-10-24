//! Tests for the #9025 remote-name rule.

use std::cell::Cell;
use std::path::Path;

use super::{RemoteNameDrift, named_after_session, remote_name_drifts};
use crate::runtime::launch_spec::LaunchSpec;

/// A spec whose argv is `args`.
fn spec_with(args: &[&str]) -> LaunchSpec {
    let mut spec =
        crate::runtime::cli_launch::inplace_spec(Path::new("/w"), Path::new("/p"), false);
    spec.args = args.iter().map(|a| (*a).to_owned()).collect();
    spec
}

/// `--name <session>` leads the argv; nothing else changes.
#[test]
fn a_spec_is_named_after_its_session() {
    let spec = spec_with(&["--append-system-prompt-file", "/p", "--remote-control"]);
    let named = named_after_session(&spec, "tm-dogfood");
    assert_eq!(
        named.args,
        [
            "--name",
            "tm-dogfood",
            "--append-system-prompt-file",
            "/p",
            "--remote-control"
        ]
    );
    assert_eq!(named.env_set, spec.env_set);
    assert_eq!(named.cwd, spec.cwd);
}

/// `claude attach <id>` takes no options, and an explicit name is kept.
#[test]
fn an_attach_or_an_already_named_spec_is_left_alone() {
    for args in [
        &["attach", "1905c26f"][..],
        &["--name", "mine"][..],
        &["-n", "mine"][..],
    ] {
        let spec = spec_with(args);
        assert_eq!(named_after_session(&spec, "tm-x"), spec, "{args:?}");
    }
}

/// A capture of the shape `claude agents --json` emits for live sessions.
const REGISTRY: &str = r#"[
  {"pid": 101, "kind": "interactive", "sessionId": "a", "name": "tm-dogfood", "status": "busy"},
  {"pid": 202, "kind": "interactive", "sessionId": "b", "name": "tm-dogfood", "status": "idle"},
  {"id": "1905c26f", "kind": "background", "sessionId": "c", "name": "Research", "state": "blocked"},
  {"pid": 303, "kind": "interactive", "sessionId": "d", "status": "idle"}
]"#;

/// The rename case: tmux session `tm-dogfood` was renamed `tm-dogfood-2`
/// after launch, so its `claude` (pid 202) still shows peers the old name.
/// Unchanged, unlisted and unnamed sessions are not drifts.
#[test]
fn a_renamed_tmux_session_is_a_remote_name_drift() {
    let sessions = [
        "tm-dogfood".to_owned(),
        "tm-dogfood-2".to_owned(),
        "tm-unlisted".to_owned(),
        "tm-unnamed".to_owned(),
        "tm-shell".to_owned(),
    ];
    let pid = |s: &str| match s {
        "tm-dogfood" => Some(101),
        "tm-dogfood-2" => Some(202),
        "tm-unlisted" => Some(999),
        "tm-unnamed" => Some(303),
        _ => None,
    };
    let drifts = remote_name_drifts(&sessions, pid, || Ok(REGISTRY.to_owned())).expect("parsed");
    assert_eq!(
        drifts,
        [RemoteNameDrift {
            session: "tm-dogfood-2".to_owned(),
            pid: 202,
            remote: "tm-dogfood".to_owned(),
        }]
    );
}

/// No `claude` in any session: the registry is never read.
#[test]
fn the_registry_is_not_read_without_a_claude() {
    let read = Cell::new(false);
    let drifts = remote_name_drifts(
        &["tm-shell".to_owned()],
        |_| None,
        || {
            read.set(true);
            Ok(REGISTRY.to_owned())
        },
    );
    assert_eq!(drifts, Ok(Vec::new()));
    assert!(!read.get());
}

/// A registry that cannot be read or parsed is an error, never "no drift".
#[test]
fn an_unreadable_registry_is_an_error() {
    let sessions = ["tm-a".to_owned()];
    let failed = remote_name_drifts(&sessions, |_| Some(1), || Err("timed out".to_owned()));
    assert_eq!(failed, Err("timed out".to_owned()));
    let garbled = remote_name_drifts(&sessions, |_| Some(1), || Ok("{not json".to_owned()));
    assert!(garbled.is_err(), "{garbled:?}");
}
