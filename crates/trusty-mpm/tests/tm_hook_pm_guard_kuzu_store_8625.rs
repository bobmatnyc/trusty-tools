//! End-to-end proof of the kuzu-store deny (#8625 PR-2).
//!
//! Why: Architect ruling B10 — the deny holds under `TRUSTY_MPM_UNRESTRICTED`
//! and `DISABLE_HOOKS`, and for the Architect. Only the built binary proves
//! the rule sits on both the guarded path and the bypass floor.
//! What: the #8878 harness — `tm hook --pm-guard` with a scratch `$HOME`,
//! run as a PM, as a subagent, under each bypass, and as the recorded
//! Architect's main thread. The scratch home carries an import venv shaped
//! like the host one, so the sanctioned `--python` passes and a planted
//! symlink to the same CPython does not.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_kuzu_store_8625::`.

use std::os::unix::fs::symlink;
use std::path::PathBuf;

use crate::tm_hook_pm_guard_trust_anchor_8878::{
    Fixture, bash_payload, run, run_under_claude, run_under_claude_with,
};

/// The bypass variables, and `None` for no bypass.
const BYPASSES: [Option<(&str, &str)>; 3] = [
    None,
    Some(("TRUSTY_MPM_PM_UNRESTRICTED", "1")),
    Some(("TRUSTY_MPM_DISABLE_HOOKS", "1")),
];

/// The fixture plus a store in the project, the import venv in the scratch
/// home (`python3`/`python3.13` → `python` → a CPython outside it), and a
/// CPython symlink planted elsewhere.
fn fixture() -> (Fixture, PathBuf) {
    let fx = Fixture::new();
    let root = fx.home.parent().expect("scratch root").to_path_buf();
    std::fs::create_dir_all(fx.project.join(".kuzu-memory")).expect("mkdir store");
    std::fs::write(fx.project.join(".kuzu-memory/memories.db"), "kuzu").expect("store");
    let cpython = root.join("uv/cpython/bin");
    std::fs::create_dir_all(&cpython).expect("mkdir cpython");
    std::fs::write(cpython.join("python3.13"), "#!").expect("cpython");
    let bin = fx.home.join(".trusty-tools/kuzu-import-venv/bin");
    std::fs::create_dir_all(&bin).expect("mkdir venv");
    symlink(cpython.join("python3.13"), bin.join("python")).expect("python");
    symlink("python", bin.join("python3")).expect("python3");
    symlink("python", bin.join("python3.13")).expect("python3.13");
    std::fs::create_dir_all(root.join("elsewhere")).expect("mkdir elsewhere");
    symlink(cpython.join("python3.13"), root.join("elsewhere/python")).expect("planted");
    (fx, root)
}

/// A main-thread `Read` payload for `path`.
fn read_payload(fx: &Fixture, path: &std::path::Path) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s-1",
        "cwd": fx.project.display().to_string(),
        "tool_name": "Read",
        "tool_input": { "file_path": path.display().to_string() },
    })
    .to_string()
}

/// `payload` as a dispatched subagent's call.
fn as_subagent(payload: &str) -> String {
    let mut value: serde_json::Value = serde_json::from_str(payload).expect("json");
    value["agent_id"] = serde_json::json!("agent-7");
    value.to_string()
}

/// Every call shape the deny must refuse, each with the slug it must carry.
fn denied_calls(fx: &Fixture, root: &std::path::Path) -> Vec<(String, &'static str)> {
    let store = fx.project.join(".kuzu-memory/memories.db");
    let s = store.display();
    let planted = root.join("elsewhere/python");
    vec![
        (read_payload(fx, &store), "kuzu-store-read"),
        (bash_payload(fx, &format!("cat {s}")), "kuzu-store-read"),
        (bash_payload(fx, "cat *.db"), "kuzu-store-read"),
        (
            bash_payload(fx, "kuzu-memory memory export"),
            "kuzu-store-read",
        ),
        (
            bash_payload(fx, "trusty-memory migrate kuzu-data --from . --palace x"),
            "kuzu-store-read",
        ),
        (
            bash_payload(fx, "trusty-memory import kuzu --discover --python /bin/cat"),
            "kuzu-import-python",
        ),
        (
            bash_payload(
                fx,
                &format!(
                    "trusty-memory import kuzu --discover --python {}",
                    planted.display()
                ),
            ),
            "kuzu-import-python",
        ),
    ]
}

#[test]
fn the_kuzu_store_deny_holds_under_each_bypass_and_for_a_subagent() {
    let (fx, root) = fixture();
    for (payload, slug) in denied_calls(&fx, &root) {
        for bypass in BYPASSES {
            let env: Vec<(&str, &str)> = bypass.into_iter().collect();
            for stdin in [payload.clone(), as_subagent(&payload)] {
                let out = run(&fx, &stdin, &env, None);
                assert!(out.contains("\"deny\""), "{bypass:?} {stdin}: {out}");
                assert!(out.contains(slug), "{bypass:?} {stdin}: {out}");
            }
        }
    }
}

#[test]
fn the_architect_is_bound_by_the_kuzu_store_deny() {
    let (fx, root) = fixture();
    for (payload, slug) in denied_calls(&fx, &root) {
        let out = run_under_claude(&fx, &payload, true);
        assert!(
            out.contains("\"deny\"") && out.contains(slug),
            "{payload}: {out}"
        );
        for bypass in BYPASSES.into_iter().flatten() {
            let out = run_under_claude_with(&fx, &payload, true, &[bypass]);
            assert!(
                out.contains("\"deny\"") && out.contains(slug),
                "{bypass:?}: {out}"
            );
        }
    }
}

#[test]
fn the_sanctioned_import_passes_end_to_end() {
    let (fx, _root) = fixture();
    let store = fx.project.join(".kuzu-memory/memories.db");
    let venv = fx.home.join(".trusty-tools/kuzu-import-venv/bin");
    let mut commands = vec![
        "trusty-memory import kuzu --discover --dry-run".to_string(),
        format!(
            "env -u TRUSTY_MEMORY_PALACE trusty-memory import kuzu --from {} --palace x",
            store.display()
        ),
    ];
    for name in ["python", "python3", "python3.13"] {
        commands.push(format!(
            "trusty-memory import kuzu --from {} --dry-run --python {}",
            store.display(),
            venv.join(name).display()
        ));
    }
    for command in commands {
        let payload = bash_payload(&fx, &command);
        for bypass in BYPASSES {
            let env: Vec<(&str, &str)> = bypass.into_iter().collect();
            let out = run(&fx, &payload, &env, None);
            assert_eq!(out.trim(), "", "{bypass:?} {command}");
        }
    }
}
