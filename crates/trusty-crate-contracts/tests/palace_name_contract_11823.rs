//! `tctl ensure` and the trusty-memory daemon agree on a project's palace
//! name (#11823).
//!
//! Why: `tctl ensure` names the palace on the client side and sends
//! `palace_create {name, cwd}`; the daemon validates that name against the
//! same `cwd`. When the two used different rules, every create from `tctl`
//! was refused. This pins that the name tctl sends is the one the daemon
//! accepts, for the three project shapes the resolver distinguishes.
//! What: for an unpinned git project with a remote, a git project with no
//! remote, and a pinned project — with `TRUSTY_MEMORY_PALACE` unset — takes
//! tctl's `palace_name_for`, checks it against trusty-memory's real
//! `validate_palace_name`, and drives the real MCP `palace_create` with
//! `{name, cwd}` (no `force`, no enforcement bypass) to `status: created`.
//! Test: `tctl_palace_name_is_accepted_by_the_daemon`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;
use serial_test::serial;
use trusty_installer::commands::ensure::identity::palace_name_for;
use trusty_memory::AppState;
use trusty_memory::project_root::validate_palace_name;
use trusty_memory::tools::dispatch_tool;

/// `<tmp>/work/<dir>` as a git repo with an optional origin remote.
fn repo(tmp: &Path, dir: &str, remote: Option<&str>) -> PathBuf {
    let root = tmp.join("work").join(dir);
    std::fs::create_dir_all(&root).expect("repo dir");
    let mut steps = vec![vec!["init", "-q"]];
    if let Some(url) = remote {
        steps.push(vec!["remote", "add", "origin", url]);
    }
    for args in steps {
        let out = Command::new("git")
            .args(&args)
            .current_dir(&root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }
    root
}

/// Why: see the module docs — the client/daemon naming contract.
#[tokio::test]
#[serial]
async fn tctl_palace_name_is_accepted_by_the_daemon() {
    // SAFETY: the only test in this binary, and `#[serial]`; no other thread
    // reads or writes the environment meanwhile.
    unsafe { std::env::remove_var("TRUSTY_MEMORY_PALACE") };
    let tmp = tempfile::tempdir().expect("tempdir");
    let pinned = repo(
        tmp.path(),
        "pinned-dir",
        Some("git@example.com:acme/widget.git"),
    );
    std::fs::create_dir_all(pinned.join(".trusty-tools")).expect("pin dir");
    std::fs::write(
        pinned.join(".trusty-tools/trusty-memory.yaml"),
        "schema_version: 1\npalace: pinned-name\n",
    )
    .expect("pin");
    let cases = [
        (
            repo(
                tmp.path(),
                "My_Repo",
                Some("git@example.com:Acme/My_Repo.git"),
            ),
            "acme-my-repo",
        ),
        (repo(tmp.path(), "no-remote", None), "work-no-remote"),
        (pinned, "pinned-name"),
    ];

    let data = tempfile::tempdir().expect("data root");
    let state = AppState::new(data.path().to_path_buf());
    state.set_ready();
    for (root, expected) in cases {
        let name = palace_name_for(&root).expect("tctl names the palace");
        assert_eq!(name, expected, "tctl's name for {}", root.display());
        validate_palace_name(&name, &root)
            .unwrap_or_else(|e| panic!("daemon refused tctl's name {name:?}: {e:#}"));
        let out = dispatch_tool(&state, "palace_create", json!({"name": name, "cwd": root}))
            .await
            .unwrap_or_else(|e| panic!("palace_create {name:?} refused: {e:#}"));
        assert_eq!(out["status"], "created", "{out}");
    }
}
