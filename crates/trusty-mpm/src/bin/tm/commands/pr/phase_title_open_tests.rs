//! `tm pr open` tests for the `[EPIC_N PHASE_M]` tag on a phase PR's title
//! (#9571). A child of `tests.rs`, so it reuses that file's `gh` and
//! preflight fakes.

use super::*;
use crate::commands::pr::{EXIT_OK, EXIT_PARTIAL};

/// A `gh issue view --json` payload for a phase issue of epic 12.
const PHASE_ISSUE_JSON: &str = r#"{"number":9572,"title":"[EPIC_12 PHASE_2] wire the thing",
    "milestone":{"title":"mpm 1.6"},"projectItems":[{"title":"Harness"}],
    "labels":[],"comments":[],"state":"OPEN"}"#;

/// The same issue payload carrying `title` instead of the phase title.
fn issue_json_titled(title: &str) -> String {
    PHASE_ISSUE_JSON.replace("[EPIC_12 PHASE_2] wire the thing", title)
}

/// The `--title` value of the first `gh pr edit` that carried one.
fn edited_title(gh: &FakeGh) -> Option<String> {
    gh.calls()
        .into_iter()
        .filter(|c| c.join(" ").starts_with("pr edit"))
        .find_map(|c| {
            let at = c.iter().position(|a| a == "--title")?;
            c.get(at + 1).cloned()
        })
}

/// REGRESSION (#9571): a PR whose link line names a phase issue is retitled
/// with the tag after the conventional prefix, in the post-create edit.
/// Red before the fix: the edit carries no `--title`.
#[test]
fn pr_9571_open_tags_a_phase_pr_title() {
    let (_d, path) = scratch_body(&body_linking("Refs #9572"));
    let mut args = open_args(&path.to_string_lossy());
    args.title = "feat(trusty-mpm): add X".to_string();
    let gh = FakeGh::new()
        .on("pr create", "https://github.com/o/r/pull/4242\n")
        .on("issue view 9572", PHASE_ISSUE_JSON)
        .on("pr edit 4242", "");
    let pre = FakePreflight::ok().with_diff(&["crates/trusty-mpm/src/lib.rs"]);

    assert_eq!(open::run(&gh, &args, &pre).expect("the PR opens"), EXIT_OK);
    assert_eq!(
        edited_title(&gh).as_deref(),
        Some("feat(trusty-mpm): [EPIC_12 PHASE_2] add X")
    );
    let edits = gh
        .calls()
        .iter()
        .filter(|c| c.join(" ").starts_with("pr edit"))
        .count();
    assert_eq!(edits, 1, "the tag rides the one metadata edit");
}

/// #9571: each case that leaves the title alone, through the real wiring.
/// The two warning cases print to stderr; `phase_title_tests` asserts them.
#[test]
fn pr_9571_title_left_unchanged() {
    let long = format!("feat(x): {}", "a".repeat(240));
    let cases: [(&str, Option<&str>, &str); 6] = [
        ("no link line", None, "feat(x): add X"),
        (
            "not a phase issue",
            Some("fix: something"),
            "feat(x): add X",
        ),
        ("a tracker", Some("[EPIC 12] the outcome"), "feat(x): add X"),
        (
            "already tagged",
            Some("[EPIC_12 PHASE_2] wire"),
            "feat(x): [EPIC_12 PHASE_2] add X",
        ),
        ("not conventional", Some("[EPIC_12 PHASE_2] wire"), "Add X"),
        ("too long", Some("[EPIC_12 PHASE_2] wire"), long.as_str()),
    ];
    for (case, issue_title, pr_title) in cases {
        let body = match issue_title {
            Some(_) => body_linking("Refs #9572"),
            None => full_body(),
        };
        let (_d, path) = scratch_body(&body);
        let mut args = open_args(&path.to_string_lossy());
        args.title = pr_title.to_string();
        let json = issue_json_titled(issue_title.unwrap_or(""));
        let gh = FakeGh::new()
            .on("pr create", "https://github.com/o/r/pull/4242\n")
            .on("issue view 9572", &json)
            .on("pr edit 4242", "");
        let pre = FakePreflight::ok().with_diff(&["crates/trusty-mpm/src/lib.rs"]);
        assert_eq!(
            open::run(&gh, &args, &pre).expect("the PR opens"),
            EXIT_OK,
            "{case}"
        );
        assert_eq!(edited_title(&gh), None, "{case}");
    }
}

/// REGRESSION (#9571, Fail-Open Check): a linked issue that cannot be read
/// leaves the phase tag unknown, so the run names `title` missing and exits
/// partial, naming the PR. Red before the fix: the run exits 0.
#[test]
fn pr_9571_an_unreadable_issue_reports_the_title_missing() {
    let (_d, path) = scratch_body(&body_linking("Refs #9572"));
    let mut args = open_args(&path.to_string_lossy());
    args.title = "feat(trusty-mpm): add X".to_string();
    let gh = FakeGh::new()
        .on("pr create", "https://github.com/o/r/pull/4242\n")
        .on_fail("issue view 9572", "HTTP 502 (api.github.com/graphql)")
        .on("pr edit 4242", "");
    let pre = FakePreflight::ok().with_diff(&["crates/trusty-mpm/src/lib.rs"]);

    assert_eq!(
        open::run(&gh, &args, &pre).expect("the PR exists"),
        EXIT_PARTIAL,
        "an unknown phase tag is a partial apply, never a silent success"
    );
    let missing =
        metadata_apply::apply(&gh, &args, &pre, "4242", &body_linking("Refs #9572")).missing();
    assert!(
        missing.iter().any(|m| m.starts_with("title")),
        "`title` is named missing: {missing:?}"
    );
}

/// #9571 Fail-Open Check: a title edit that fails twice is reported missing.
/// Red before the fix: no title step exists, so nothing names `title`.
#[test]
fn pr_9571_a_failed_title_edit_reports_the_title_missing() {
    let (_d, path) = scratch_body(&body_linking("Refs #9572"));
    let mut args = open_args(&path.to_string_lossy());
    args.title = "feat(trusty-mpm): add X".to_string();
    let gh = FakeGh::new()
        .on("issue view 9572", PHASE_ISSUE_JSON)
        .on_fail("pr edit 4242", "GraphQL: something went wrong");
    let pre = FakePreflight::ok().with_diff(&["crates/trusty-mpm/src/lib.rs"]);

    let missing =
        metadata_apply::apply(&gh, &args, &pre, "4242", &body_linking("Refs #9572")).missing();
    assert!(
        missing
            .iter()
            .any(|m| m.starts_with("title \"feat(trusty-mpm): [EPIC_12 PHASE_2] add X\"")),
        "{missing:?}"
    );
}

/// #9571: the tag needs the issue read, which happens only after the create —
/// so a dry run with a phase link still spawns no `gh`.
#[test]
fn pr_9571_dry_run_with_a_phase_link_calls_no_gh() {
    let (_d, path) = scratch_body(&body_linking("Refs #9572"));
    let mut args = open_args(&path.to_string_lossy());
    args.title = "feat(trusty-mpm): add X".to_string();
    args.dry_run = true;
    let gh = FakeGh::new();
    assert_eq!(
        open::run(&gh, &args, &FakePreflight::ok()).expect("dry run"),
        EXIT_OK
    );
    assert!(gh.calls().is_empty(), "a dry run must not spawn gh");
}
