//! Tests for the Focus key's plan and its effects (#10476).

use super::*;

const SESSION: &str = "tm-trusty-tools-27";

fn env(in_tmux: bool, in_iterm2: bool) -> FocusEnv {
    FocusEnv { in_tmux, in_iterm2 }
}

fn iterm_found(_: &str) -> Result<bool, String> {
    Ok(true)
}

fn iterm_missing(_: &str) -> Result<bool, String> {
    Ok(false)
}

fn iterm_must_not_run(_: &str) -> Result<bool, String> {
    panic!("the iTerm2 effect must not run for this plan")
}

fn hook_switched(_: &str) -> FocusResult {
    FocusResult::Switched
}

fn hook_unresolved(_: &str) -> FocusResult {
    FocusResult::Unresolved
}

fn hook_failed(_: &str) -> FocusResult {
    FocusResult::Failed("can't find session".to_string())
}

#[test]
fn plan_table_tmux_iterm_neither() {
    let session = SESSION.to_string();
    assert_eq!(
        FocusPlan::decide(env(true, false), SESSION),
        FocusPlan::SwitchClient {
            session: session.clone()
        }
    );
    assert_eq!(
        FocusPlan::decide(env(true, true), SESSION),
        FocusPlan::SwitchClient {
            session: session.clone()
        },
        "tmux inside iTerm2 switches through tmux"
    );
    assert_eq!(
        FocusPlan::decide(env(false, true), SESSION),
        FocusPlan::ItermSelect { session }
    );
    assert!(matches!(
        FocusPlan::decide(env(false, false), SESSION),
        FocusPlan::Notice(_)
    ));
}

#[test]
fn no_plan_contains_spawn_class_argv() {
    const SPAWN_CLASS: &[&str] = &[
        "new-session",
        "new-window",
        "attach-session",
        "split-window",
        "create tab",
        "create window",
    ];
    for (tmux, iterm) in [(true, false), (true, true), (false, true), (false, false)] {
        let plan = FocusPlan::decide(env(tmux, iterm), SESSION);
        let argv = plan.argv();
        for arg in &argv {
            for bad in SPAWN_CLASS {
                assert!(
                    !arg.to_lowercase().contains(bad),
                    "{plan:?} carries spawn-class {bad:?} in {arg:?}"
                );
            }
        }
        if let FocusPlan::SwitchClient { .. } = plan {
            assert_eq!(argv[1], "switch-client");
            assert_eq!(argv.last().map(String::as_str), Some("=tm-trusty-tools-27"));
        }
    }
}

#[test]
fn focus_outside_tmux_and_iterm2_is_a_notice_only() {
    let env = FocusEnv::from_vars(
        None,
        Some(String::new()),
        Some("Apple_Terminal".into()),
        None,
    );
    assert_eq!(env, FocusEnv::default());
    let plan = FocusPlan::decide(env, SESSION);
    assert!(plan.argv().is_empty(), "a notice runs nothing");
    let notice = run_plan(&plan, Some(hook_switched), iterm_must_not_run);
    assert!(!notice.contains('\n'), "one line: {notice:?}");
    assert!(notice.contains("nothing opened"), "{notice}");
}

#[test]
fn env_detection_reads_tmux_and_iterm_vars() {
    assert!(
        FocusEnv::from_vars(Some("/tmp/tmux-501/default,1,0".into()), None, None, None).in_tmux
    );
    assert!(FocusEnv::from_vars(None, Some("%3".into()), None, None).in_tmux);
    assert!(FocusEnv::from_vars(None, None, Some("iTerm.app".into()), None).in_iterm2);
    assert!(FocusEnv::from_vars(None, None, None, Some("w0t0p0:abc".into())).in_iterm2);
}

#[test]
fn unset_hook_is_a_notice_not_a_panic() {
    let plan = FocusPlan::decide(env(true, false), SESSION);
    let notice = run_plan(&plan, None, iterm_must_not_run);
    assert!(notice.contains("unavailable"), "{notice}");
}

#[test]
fn hook_results_map_to_notices() {
    let plan = FocusPlan::decide(env(true, false), SESSION);
    assert_eq!(
        run_plan(&plan, Some(hook_switched), iterm_must_not_run),
        format!("focused '{SESSION}'")
    );
    assert!(run_plan(&plan, Some(hook_unresolved), iterm_must_not_run).contains("nothing moved"));
    assert!(run_plan(&plan, Some(hook_failed), iterm_must_not_run).contains("can't find session"));
}

#[test]
fn iterm_tab_not_found_is_a_notice() {
    let plan = FocusPlan::decide(env(false, true), SESSION);
    assert!(run_plan(&plan, None, iterm_found).contains("focused the iTerm2 tab"));
    let missing = run_plan(&plan, None, iterm_missing);
    assert!(missing.contains("nothing opened"), "{missing}");
}
