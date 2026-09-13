//! Flag-resolution and addendum-shape tests for #7688.

use super::*;
use tempfile::TempDir;

/// Write a project config into a fresh temp project.
fn project_with(toml: &str) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    std::fs::write(
        tmp.path()
            .join(crate::core::project_config::PROJECT_CONFIG_FILE),
        toml,
    )
    .expect("write project config");
    tmp
}

#[test]
fn disabled_by_default() {
    let tmp = TempDir::new().expect("tempdir");
    assert!(
        !enabled_for_with_host(tmp.path(), None),
        "no project key and no host key must resolve to OFF"
    );
}

#[test]
fn host_config_enables_it() {
    let tmp = TempDir::new().expect("tempdir");
    assert!(enabled_for_with_host(tmp.path(), Some(true)));
    assert!(!enabled_for_with_host(tmp.path(), Some(false)));
}

/// 🔴 The precedence the owner ruling asks for: the committed project file wins
/// over the operator's host default, in BOTH directions.
#[test]
fn project_config_true_overrides_host_false() {
    let tmp = project_with("prompt_self_improvement = true\n");
    assert!(enabled_for_with_host(tmp.path(), Some(false)));
    assert!(enabled_for_with_host(tmp.path(), None));
}

#[test]
fn project_config_false_overrides_host_true() {
    let tmp = project_with("prompt_self_improvement = false\n");
    assert!(!enabled_for_with_host(tmp.path(), Some(true)));
}

/// A project file that declines to decide must fall through, not deny.
#[test]
fn an_absent_project_key_falls_through_to_the_host() {
    let tmp = project_with("worktree = true\n");
    assert!(enabled_for_with_host(tmp.path(), Some(true)));
    assert!(!enabled_for_with_host(tmp.path(), Some(false)));
}

/// A malformed committed config contributes nothing rather than aborting —
/// `load_or_report`'s contract, restated for this key.
#[test]
fn a_malformed_project_config_falls_through_to_the_host() {
    let tmp = project_with("prompt_self_improvement = \"yes\"\n");
    assert!(enabled_for_with_host(tmp.path(), Some(true)));
}

#[test]
fn the_addendum_names_the_extraction_heading() {
    assert!(
        PM_ADDENDUM.contains(FEEDBACK_HEADING),
        "the PM addendum must name the heading the hook extracts"
    );
}

/// 🔴 A dispatched agent is asked through the PM's own brief, never through a
/// second injection at agent deploy. The pass-it-down clause is therefore the
/// only thing that carries the request past the PM, and dropping it would
/// silently reduce the feature to PM-only.
#[test]
fn the_addendum_asks_the_pm_to_pass_the_request_down() {
    assert!(
        PM_ADDENDUM.contains("dispatch brief you send"),
        "the PM addendum must tell the PM to append the request to every brief"
    );
}

#[test]
fn pm_addendum_is_separated_from_the_prompt_it_follows() {
    let addendum = pm_addendum();
    assert!(
        addendum.starts_with(crate::core::instruction_pipeline::SECTION_SEPARATOR),
        "the addendum must open with the section separator, got {addendum:?}"
    );
    assert!(addendum.contains(FEEDBACK_HEADING));
}

/// 🔴 THE OFF CASE for the PM prompt. Fails on any implementation that appends
/// unconditionally.
#[test]
fn pm_prompt_is_unchanged_when_the_flag_is_off() {
    let tmp = project_with("prompt_self_improvement = false\n");
    let composed = "COMPOSED PROMPT".to_string();
    assert_eq!(
        append_to_pm_prompt(tmp.path(), composed.clone()),
        composed,
        "the flag off must leave the composed prompt byte-identical"
    );
}

#[test]
fn pm_prompt_carries_the_addendum_when_the_flag_is_on() {
    let tmp = project_with("prompt_self_improvement = true\n");
    let got = append_to_pm_prompt(tmp.path(), "COMPOSED PROMPT".to_string());
    assert!(
        got.starts_with("COMPOSED PROMPT"),
        "the prompt is preserved"
    );
    assert!(got.contains(FEEDBACK_HEADING), "the addendum is appended");
}

/// The addendum stays short. The feature costs every response five lines of
/// output; it must not also cost the prompt a page of input. The cap tightened
/// from 12 to 6 with the #7702 review trim: the delivered text is the heading
/// and one paragraph, and the rationale that used to follow it now lives in
/// `PM_ADDENDUM`'s own doc comment, where it is not resident in every prompt.
#[test]
fn the_addendum_is_short() {
    assert!(
        PM_ADDENDUM.lines().count() <= 6,
        "the PM addendum is {} lines; keep it under 6",
        PM_ADDENDUM.lines().count()
    );
}

/// 🔴 The trim is the point, not an accident of wording. A justification
/// paragraph reads as author-facing prose and costs tokens on every turn; the
/// delivered text carries the instruction alone.
#[test]
fn the_addendum_carries_no_justification_paragraph() {
    assert!(
        !PM_ADDENDUM.contains("#6935"),
        "the framework-reporting distinction belongs in the module doc, not in \
         the prompt every turn pays for"
    );
}
