//! Shared helpers for the Intent Ledger integration tests.
//!
//! Why: the golden, ops, reliability, ilio and parity tests all copy a ledger to a
//! temp dir, run verbs against it and compare files, so the plumbing lives once.
//! What: fixture paths, a recursive directory copy, an in-process `run`, a
//! subprocess `run_bin`, and a reader for the golden JSON-lines files.
//! Test: used by `il_golden.rs`, `il_ops.rs`, `il_reliability.rs`, `il_ilio.rs`,
//! `il_parity.rs`, `il_latency.rs`.
#![allow(dead_code)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;
use trusty_architect::FakeClock;
use trusty_architect::il::{Env, Out, run};

/// `tests/fixtures/il/<rel>`.
pub fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/il")
        .join(rel)
}

/// Copies the files of `src` (one level, no `.lock`) into `dst`, creating it.
pub fn copy_files(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for e in fs::read_dir(src).unwrap().flatten() {
        let name = e.file_name();
        if e.path().is_file() && name != ".lock" {
            fs::copy(e.path(), dst.join(name)).unwrap();
        }
    }
}

/// A project directory under `root` holding a copy of the ledger files of `datg_src`.
pub fn project_with(root: &Path, datg_src: &Path) -> PathBuf {
    let p = root.join("p");
    copy_files(datg_src, &p.join(".trusty-mpm/datg"));
    p
}

/// The ledger directory of a project.
pub fn datg_dir(project: &Path) -> PathBuf {
    project.join(".trusty-mpm/datg")
}

/// Runs `args` in process with the clock frozen at `epoch`.
pub fn run_at(args: &[&str], stdin: &str, env: &Env, epoch: i64) -> Out {
    let a: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    let input = stdin.to_owned();
    run(
        &a,
        &move || input.clone(),
        env,
        &FakeClock::new(epoch as f64),
    )
}

/// Runs the real binary; `epoch` freezes its clock through `TRUSTY_IL_NOW`.
pub fn run_bin(args: &[&str], stdin: &str, env: &[(&str, &str)], epoch: Option<i64>) -> Out {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_trusty-architect-il"));
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for k in [
        "DATG_SESSION",
        "DATG_STATE_DIR",
        "TMUX_PANE",
        "TRUSTY_IL_NOW",
        "ILIO_PROJECT",
        "ILIO_DIR",
    ] {
        cmd.env_remove(k);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    if let Some(e) = epoch {
        cmd.env("TRUSTY_IL_NOW", e.to_string());
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    Out {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    }
}

/// The lines of a JSON-lines file as values.
pub fn read_jsonl(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// The file names (no `.lock`) of a directory, sorted.
pub fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n != ".lock")
        .collect();
    v.sort();
    v
}

/// The text of `dir/name`.
pub fn read(dir: &Path, name: &str) -> String {
    fs::read_to_string(dir.join(name)).unwrap()
}
