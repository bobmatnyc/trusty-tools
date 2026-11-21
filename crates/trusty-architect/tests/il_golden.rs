//! Golden tests: the Rust ledger writes the bytes the Python wrote.
//!
//! Why: `tests/fixtures/il/golden/` holds what `datg.py` and `ilio.py` printed and
//! wrote for a fixed ledger, a fixed list of steps and a frozen clock
//! (`tests/fixtures/il/generate.py`). Replaying the steps with the Rust port and
//! comparing stdout, stderr, exit codes and every file byte for byte proves parity
//! without python3 or the live ledgers, and fails the default `cargo test` when it
//! drifts.
//! What: replays `steps.jsonl` on a copy of `base/`, compares each step with
//! `golden/steps.jsonl`, then the final files with `golden/datg/` and `epoch.json`.
//! Python-only stderr noise (`task_steer:`) is ignored; `@P@` and `@I@` stand for
//! the project and `ILIO_DIR` paths.
//! Test: `golden_steps_match_python_byte_for_byte`, `no_unexpected_files_in_the_ledger_dir`.

mod il_support;

use std::fs;

use il_support::*;
use trusty_architect::il::Env;

/// Characters in the project path of the golden run (`tests/fixtures/il/generate.py`).
const PROJECT_LEN: usize = 150;

fn normalise(s: &str, project: &str, ilio: &str) -> String {
    s.replace(project, "@P@").replace(ilio, "@I@")
}

fn python_noise(line: &str) -> bool {
    line.starts_with("task_steer:") || line.starts_with("ilio: steering log skipped")
}

fn keep_stderr(s: &str) -> String {
    s.lines()
        .filter(|l| !python_noise(l))
        .map(|l| format!("{l}\n"))
        .collect()
}

#[test]
fn golden_steps_match_python_byte_for_byte() {
    let tmp = tempfile::tempdir().unwrap();
    // The digest cuts by length and prints the project path, so the project path has the length the
    // golden was generated with (`generate.py` PROJECT_LEN).
    let pad = PROJECT_LEN - tmp.path().to_str().unwrap().len() - "/x/p".len();
    let root = tmp.path().join("x".repeat(pad + 1));
    let project = project_with(&root, &fixture("base_datg"));
    assert_eq!(project.to_str().unwrap().len(), PROJECT_LEN);
    let ilio_dir = tmp.path().join("ilio");
    fs::create_dir_all(&ilio_dir).unwrap();
    fs::copy(
        fixture("ilio/r-library.json"),
        ilio_dir.join("r-library.json"),
    )
    .unwrap();
    let (p, i) = (project.to_str().unwrap(), ilio_dir.to_str().unwrap());
    let env = Env::bare(tmp.path())
        .with("DATG_NOTIFY_CMD", "/usr/bin/true")
        .with("ILIO_PROJECT", p)
        .with("ILIO_DIR", i)
        .with("ILIO_WHO", "tm-architect");
    let steps = read_jsonl(&fixture("steps.jsonl"));
    let gold = read_jsonl(&fixture("golden/steps.jsonl"));
    assert_eq!(steps.len(), gold.len());
    for (n, (s, g)) in steps.iter().zip(&gold).enumerate() {
        let prog = s["prog"].as_str().unwrap();
        let mut args = vec![prog];
        if prog == "datg" {
            args.extend(["--project", p]);
        }
        let step_args: Vec<&str> = s["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        args.extend(step_args.iter().copied());
        let stdin = s["stdin"].as_str().unwrap_or("");
        let out = run_at(&args, stdin, &env, s["epoch"].as_i64().unwrap());
        let label = format!("step {n}: {prog} {}", step_args.join(" "));
        assert_eq!(
            normalise(&out.stdout, p, i),
            g["stdout"].as_str().unwrap(),
            "{label}: stdout"
        );
        assert_eq!(
            keep_stderr(&normalise(&out.stderr, p, i)),
            keep_stderr(g["stderr"].as_str().unwrap()),
            "{label}: stderr"
        );
        assert_eq!(
            i64::from(out.code),
            g["code"].as_i64().unwrap(),
            "{label}: exit code"
        );
    }
    let dir = datg_dir(&project);
    for name in names(&fixture("golden/datg")) {
        assert_eq!(
            read(&dir, &name),
            read(&fixture("golden/datg"), &name),
            "final {name}"
        );
    }
    assert_eq!(
        read(&ilio_dir, "epoch.json"),
        read(&fixture("golden"), "epoch.json"),
        "epoch.json"
    );
}

#[test]
fn no_unexpected_files_in_the_ledger_dir() {
    // Same file set as the Python leaves: nothing like an outbox or a journal, no temp file.
    let tmp = tempfile::tempdir().unwrap();
    let project = project_with(tmp.path(), &fixture("base_datg"));
    let env = Env::bare(tmp.path());
    let p = project.to_str().unwrap();
    for args in [
        vec![
            "datg",
            "--project",
            p,
            "add",
            "T",
            "--title",
            "x",
            "--goal",
            "fx-G01",
            "--done-when",
            "y",
        ],
        vec!["datg", "--project", p, "act", "--who", "w", "--what", "x"],
        vec![
            "datg",
            "--project",
            p,
            "update",
            "fx-T01",
            "--title",
            "renamed",
        ],
    ] {
        assert_eq!(run_at(&args, "", &env, 1_800_000_000).code, 0, "{args:?}");
    }
    let mut want = names(&fixture("base_datg"));
    want.sort();
    assert_eq!(names(&datg_dir(&project)), want);
}
