//! Tests for the PM tool trim (#8992).

use super::*;
use crate::core::session_profile::launch_env;

#[test]
fn a_pm_launch_disables_the_artifact_tools() {
    assert_eq!(
        trim_env(SessionProfile::Pm),
        [("CLAUDE_CODE_DISABLE_ARTIFACT".to_owned(), "1".to_owned())]
    );
}

#[test]
fn a_pm_launch_denies_the_optional_built_ins() {
    assert_eq!(
        trim_args(SessionProfile::Pm),
        ["--disallowedTools", "Workflow,DesignSync"]
    );
}

#[test]
fn the_cron_tools_are_never_denied() {
    // #8992: session resume schedules through CronCreate/CronDelete/CronList.
    let value = &trim_args(SessionProfile::Pm)[1];
    assert!(!value.contains("Cron"), "{value}");
}

#[test]
fn the_chrome_tools_are_never_denied() {
    // #8992: web-qa needs the chrome tools; deny rules reach subagents
    assert!(
        !DISALLOWED_TOOLS
            .iter()
            .any(|t| t.contains("claude-in-chrome")),
        "{DISALLOWED_TOOLS:?}"
    );
    assert!(!trim_args(SessionProfile::Pm)[1].contains("chrome"));
}

#[test]
fn a_supervisor_launch_keeps_every_tool() {
    assert!(trim_env(SessionProfile::Architect).is_empty());
    assert!(trim_args(SessionProfile::Architect).is_empty());
}

#[test]
fn the_stamp_decides_the_profile() {
    let pin = ("TRUSTY_INDEX".to_owned(), "repo".to_owned());
    assert_eq!(stamped_profile(&[]), SessionProfile::Pm);
    assert_eq!(
        stamped_profile(std::slice::from_ref(&pin)),
        SessionProfile::Pm
    );
    assert_eq!(
        stamped_profile(&[pin, launch_env(SessionProfile::Architect)]),
        SessionProfile::Architect
    );
    // The last stamp wins, as it does for the spawned process.
    assert_eq!(
        stamped_profile(&[
            launch_env(SessionProfile::Architect),
            launch_env(SessionProfile::Pm)
        ]),
        SessionProfile::Pm
    );
}
