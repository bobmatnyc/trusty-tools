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
fn both_addenda_name_the_extraction_heading() {
    assert!(
        PM_ADDENDUM.contains(FEEDBACK_HEADING),
        "the PM addendum must name the heading the hook extracts"
    );
    assert!(
        AGENT_ADDENDUM.contains(FEEDBACK_HEADING),
        "the agent addendum must name the heading the hook extracts"
    );
}

/// An agent never dispatches ("No Subagent Fan-Out"), so its addendum must not
/// tell it to pass the request down.
#[test]
fn agent_addendum_does_not_ask_an_agent_to_dispatch() {
    assert!(
        !AGENT_ADDENDUM.contains("dispatch brief you send"),
        "the agent addendum must not carry the PM's pass-it-down clause"
    );
    assert!(
        PM_ADDENDUM.contains("dispatch brief you send"),
        "the PM addendum must carry it"
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

#[test]
fn agent_addendum_is_separated_from_the_body_it_follows() {
    let addendum = agent_addendum();
    assert!(addendum.starts_with(crate::core::instruction_pipeline::SECTION_SEPARATOR));
    assert!(
        addendum.ends_with('\n'),
        "a composed agent file must end with a newline"
    );
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

/// 🔴 THE OFF CASE for the agent deploy. `None` is the byte-identical
/// pre-#7688 path through `deploy_agents_filtered_with_suffix`.
#[test]
fn agent_suffix_is_none_when_the_flag_is_off() {
    let tmp = project_with("prompt_self_improvement = false\n");
    assert_eq!(agent_deploy_suffix(tmp.path()), None);
}

#[test]
fn agent_suffix_is_the_addendum_when_the_flag_is_on() {
    let tmp = project_with("prompt_self_improvement = true\n");
    let suffix = agent_deploy_suffix(tmp.path()).expect("a suffix");
    assert!(suffix.contains(FEEDBACK_HEADING));
    assert_eq!(suffix, agent_addendum());
}

/// Both addenda stay short. The feature costs every response five lines of
/// output; it must not also cost the prompt a page of input.
#[test]
fn both_addenda_are_short() {
    for (name, text) in [("pm", PM_ADDENDUM), ("agent", AGENT_ADDENDUM)] {
        assert!(
            text.lines().count() <= 12,
            "the {name} addendum is {} lines; keep it under 12",
            text.lines().count()
        );
    }
}
