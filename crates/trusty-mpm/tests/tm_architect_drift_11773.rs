//! `tm architect drift` and `tm architect up --strict` through the built binary (#11773).
//!
//! Why: the exit code is the contract a script reads; only the binary proves
//! `drift` exits 1 on an edited file and `up --strict` refuses before it
//! writes, locks or starts anything.
//! What: each test runs `tm` under a scratch `$HOME` against a scratch
//! Architect directory passed with `--dir`; the seed comes from this checkout
//! (the child's cwd). No tmux, daemon or network is reached.
//! Test: `cargo test -p trusty-mpm --test integration tm_architect_drift_11773::`.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Output;

use crate::common;

/// `tm architect <args> --dir <dir>` under a scratch home, cwd in this checkout.
fn tm_architect(home: &Path, dir: &Path, args: &[&str]) -> Output {
    common::tm_command_in(home)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env_remove("ARCHITECT_PROJECT_DIR")
        .arg("architect")
        .args(args)
        .arg("--dir")
        .arg(dir)
        .output()
        .expect("run tm")
}

/// Every file below `dir` with its bytes.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(dir).unwrap().display().to_string();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    out
}

/// A directory with one edited managed script.
fn edited_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("architect dir");
    std::fs::create_dir_all(dir.path().join("scripts")).unwrap();
    std::fs::write(dir.path().join("scripts/datg.py"), "print('my edit')\n").unwrap();
    dir
}

/// No file edited exits 0; an edited script exits 1 and `--json` names it.
#[test]
fn drift_json_exits_1_on_an_edited_script() {
    let home = tempfile::tempdir().expect("scratch home");
    let empty = tempfile::tempdir().expect("empty dir");
    let out = tm_architect(home.path(), empty.path(), &["drift", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(v["summary"]["edited"], 0);
    assert!(v["summary"]["absent"].as_u64().unwrap() > 100, "{v}");

    let dir = edited_dir();
    let out = tm_architect(home.path(), dir.path(), &["drift", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let datg = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "scripts/datg.py")
        .expect("datg.py reported");
    assert_eq!(datg["state"], "edited");
    assert_eq!(v["exit"], 1);
}

/// `up --strict` over an edited script exits 1, names it, and writes nothing.
#[test]
fn up_strict_refuses_an_edited_script_and_writes_nothing() {
    let home = tempfile::tempdir().expect("scratch home");
    let dir = edited_dir();
    let before = snapshot(dir.path());
    let home_before = snapshot(home.path());
    let out = tm_architect(
        home.path(),
        dir.path(),
        &["up", "--strict", "--no-attach", "--json"],
    );
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("edited   scripts/datg.py"), "{stderr}");
    assert_eq!(snapshot(dir.path()), before);
    assert_eq!(snapshot(home.path()), home_before);
}
