//! Hook-registration and byte-identity tests for #7688.
//!
//! The pair that matters: with the flag OFF every byte this writer produces is
//! what it produced before #7688 existed, and with it ON exactly two groups
//! appear. Anything in between is a regression neither half would catch alone.

use super::project_hooks::{
    is_project_managed_hook_command, project_managed_hook_additions,
    project_managed_hook_additions_with_prompt_feedback, project_managed_hook_events,
};
use super::prompt_feedback_hooks::PROMPT_FEEDBACK_SUFFIX;

/// A stable installed-looking path, so the assertions do not depend on whether
/// the host running them has `tm` installed (#7244).
const STABLE_EXE: &str = "/usr/local/bin/tm";

fn additions(prompt_feedback: bool) -> serde_json::Value {
    project_managed_hook_additions_with_prompt_feedback(
        Some(std::path::Path::new(STABLE_EXE)),
        true,
        false,
        prompt_feedback,
    )
    .expect("a stable exe resolves")
}

/// Every command string registered under `event`.
fn commands_for(value: &serde_json::Value, event: &str) -> Vec<String> {
    value["hooks"][event]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .filter_map(|g| g["hooks"].as_array())
                .flatten()
                .filter_map(|h| h["command"].as_str())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// 🔴 THE OFF CASE. Fails on any implementation that registers the capture
/// unconditionally.
#[test]
fn project_managed_hook_additions_omits_prompt_feedback_when_disabled() {
    let off = additions(false);
    for event in ["Stop", "SubagentStop"] {
        assert!(
            !commands_for(&off, event)
                .iter()
                .any(|c| c.ends_with(PROMPT_FEEDBACK_SUFFIX)),
            "{event} must carry no capture group when the flag is off"
        );
    }
}

/// 🔴 THE BYTE-IDENTITY PIN. The flag-off block must equal what the pre-#7688
/// three-argument function produces — not merely "look similar".
#[test]
fn the_disabled_block_is_byte_identical_to_the_pre_7688_writer() {
    let legacy =
        project_managed_hook_additions(Some(std::path::Path::new(STABLE_EXE)), true, false)
            .expect("a stable exe resolves");
    assert_eq!(
        legacy,
        additions(false),
        "turning the flag off must reproduce the pre-#7688 block exactly"
    );
}

/// THE ON CASE.
#[test]
fn project_managed_hook_additions_includes_prompt_feedback_when_enabled() {
    let on = additions(true);
    for event in ["Stop", "SubagentStop"] {
        let captures: Vec<String> = commands_for(&on, event)
            .into_iter()
            .filter(|c| c.ends_with(PROMPT_FEEDBACK_SUFFIX))
            .collect();
        assert_eq!(
            captures.len(),
            1,
            "{event} must carry exactly one capture group, got {captures:?}"
        );
        // 🔴 The pinned exe, not merely "some absolute path". `starts_with('/')`
        // alone passed on any machine with `tm` installed and failed only on a
        // runner without one, because the builder resolved its own binary and
        // ignored the override entirely — a red CI shard nobody could reproduce
        // locally. Comparing against STABLE_EXE fails on the pre-fix builder
        // wherever it runs, and still pins the #1914 absolute-path contract.
        assert_eq!(
            captures[0],
            format!("{STABLE_EXE}{PROMPT_FEEDBACK_SUFFIX}"),
            "the capture must invoke the resolved absolute exe (#1914)"
        );
    }
}

/// The capture is ADDITIVE — the lifecycle triad's own `Stop` group survives.
#[test]
fn the_capture_does_not_displace_the_lifecycle_triad() {
    let off = commands_for(&additions(false), "Stop");
    let on = commands_for(&additions(true), "Stop");
    for command in &off {
        assert!(
            on.contains(command),
            "enabling the capture dropped {command:?} from Stop"
        );
    }
    assert_eq!(on.len(), off.len() + 1, "exactly one group is added");
}

/// 🔴 The #5034 lesson: without this the strip never removes a stale group and
/// turning the flag back off would leave the hook firing forever.
#[test]
fn is_project_managed_hook_command_recognises_prompt_feedback() {
    assert!(is_project_managed_hook_command(&format!(
        "{STABLE_EXE}{PROMPT_FEEDBACK_SUFFIX}"
    )));
    assert!(!is_project_managed_hook_command("/usr/local/bin/tm wait"));
}

/// The strip domain must be the full OWNED set, or a stale capture survives the
/// flag being turned off.
#[test]
fn project_managed_hook_events_covers_the_capture_events() {
    let events = project_managed_hook_events();
    for event in ["Stop", "SubagentStop"] {
        assert!(
            events.iter().any(|e| e == event),
            "the strip domain must include {event}; got {events:?}"
        );
    }
}

/// The writer's command and the build-tree classifier's tail list must agree,
/// or a stale capture pointing at a Cargo build tree escapes repair (#7262).
#[test]
fn the_capture_command_ends_in_a_known_argv_tail() {
    let command = format!("{STABLE_EXE}{PROMPT_FEEDBACK_SUFFIX}");
    assert!(
        crate::core::standalone::hooks::is_mpm_hook_command(&command)
            || is_project_managed_hook_command(&command),
        "the classifier must recognise {command:?}"
    );
}
