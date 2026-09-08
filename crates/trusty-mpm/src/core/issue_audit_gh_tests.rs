//! Argv tests for the `tm issue audit` gh reads (#7097).
//!
//! No test here spawns `gh`. What matters at this seam is that the field list
//! and the limit reach gh intact — a dropped field silently stops checking a
//! requirement, and a dropped limit silently truncates the audited window.

use super::*;

#[test]
fn view_argv_requests_every_audited_field() {
    let argv = view_argv(7093);
    assert_eq!(argv[..3], ["issue", "view", "7093"]);
    let json = argv
        .iter()
        .position(|a| a == "--json")
        .expect("the view argv carries --json");
    assert_eq!(
        argv.get(json + 1).map(String::as_str),
        Some(AUDIT_JSON_FIELDS)
    );
}

#[test]
fn recent_window_argv_bounds_the_limit() {
    let argv = list_argv(&AuditWindow::Recent(20));
    let limit = argv
        .iter()
        .position(|a| a == "--limit")
        .expect("the list argv carries --limit");
    assert_eq!(argv.get(limit + 1).map(String::as_str), Some("20"));
    assert!(
        argv.windows(2).any(|w| w == ["--state", "open"]),
        "a batch audit reads OPEN issues only: {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "--search"),
        "a --recent window needs no search term: {argv:?}"
    );
}

#[test]
fn since_window_argv_carries_an_explicit_limit() {
    // #7097: gh applies a silent default of 30 when --limit is omitted, which
    // would make a wide --since window report on a fraction of its issues.
    let argv = list_argv(&AuditWindow::Since("2026-09-01".to_string()));
    let limit = argv
        .iter()
        .position(|a| a == "--limit")
        .expect("the since argv carries --limit");
    assert_eq!(
        argv.get(limit + 1).map(String::as_str),
        Some(SINCE_LIMIT.to_string().as_str())
    );
    const {
        assert!(
            SINCE_LIMIT > 30,
            "the limit must exceed gh's silent default of 30"
        );
    }
    let search = argv
        .iter()
        .position(|a| a == "--search")
        .expect("the since argv carries --search");
    assert_eq!(
        argv.get(search + 1).map(String::as_str),
        Some("created:>=2026-09-01")
    );
}

#[test]
fn an_ambient_binding_touches_no_environment() {
    // A default `GhEnv` is the unbound case — `bind` must be a no-op there, or
    // every unbound call would carry a scrubbed environment it never asked for.
    let ambient = GhEnv::default();
    assert!(ambient.is_empty());
    assert!(ambient.vars().is_empty());
    assert!(ambient.unset_vars().is_empty());
    let cmd = bind(GhCommand::new(view_argv(1)), &ambient);
    assert_eq!(cmd.argv_display(), view_argv(1).join(" "));
}
