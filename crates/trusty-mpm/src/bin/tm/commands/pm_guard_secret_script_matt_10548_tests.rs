//! #10548 / #10672: the two refusals a user hit, rebuilt from the verbatim
//! commands with synthetic tempdir fixtures, judged by the gated secret-read
//! decision both deny sites call (`evaluate_secret_file_read_gated`).
//!
//! The two `*_allows_*` tests are RED on main: they pin the false positives.
//! The `*_refuses_*` controls are green on main and must stay green after the
//! fix. No fixture is a real document or credential; nothing is executed.

use std::path::{Path, PathBuf};

use serde_json::json;
use trusty_mpm::core::config::MpmConfig;

use crate::commands::pm_guard_architect_envfile::evaluate_secret_file_read_gated;
use crate::commands::pm_guard_floor::ArchitectGate;
use crate::commands::pm_guard_trust_anchor::HookEnv;

/// An AWS credentials read — refused inline (#7266).
const AWS_READ_SH: &str = "cat ~/.aws/credentials\n";

/// The same read written in Python.
const AWS_READ_PY: &str =
    "import os\nprint(open(os.path.expanduser('~/.aws/credentials')).read())\n";

/// A benign python-docx build script: names no credential.
const BUILD_PY: &str =
    "import sys\nfrom docx import Document\nDocument(sys.argv[1]).save(sys.argv[2])\n";

/// A zip local-file-header magic and one non-UTF-8 byte: a fake `.docx`.
const FAKE_DOCX: [u8; 5] = [0x50, 0x4b, 0x03, 0x04, 0xff];

/// Write `body` to `dir/name`, creating parents, and return the path.
fn file(dir: &Path, name: &str, body: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, body).expect("write");
    path
}

/// The gated secret-read verdict for a Bash `command` run from `hook_cwd` by a
/// session that is not the Architect.
fn gated(command: &str, hook_cwd: &Path, home: &Path) -> Option<String> {
    let call = json!({ "tool_name": "Bash", "tool_input": { "command": command } });
    let env = HookEnv {
        home: Some(home.to_path_buf()),
        ..HookEnv::default()
    };
    let gate = ArchitectGate::new(&call, env, MpmConfig::default);
    evaluate_secret_file_read_gated("Bash", call.get("tool_input"), hook_cwd, &gate)
        .map(|d| d.reason)
}

/// The shared tempdir layout: `hook/` (the session's cwd), `home/`, and
/// `proj/` with `proj/sandboxes/` and a harmless review file.
struct Layout {
    _dir: tempfile::TempDir,
    root: PathBuf,
    hook: PathBuf,
    home: PathBuf,
    proj: PathBuf,
}

fn layout() -> Layout {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    let (hook, home, proj) = (root.join("hook"), root.join("home"), root.join("proj"));
    for d in [&hook, &home, &proj.join("sandboxes")] {
        std::fs::create_dir_all(d).expect("mkdir");
    }
    file(
        &proj,
        "sandboxes/r4/review/x.md",
        b"synthetic review note\n",
    );
    Layout {
        _dir: dir,
        root,
        hook,
        home,
        proj,
    }
}

/// Matt's find command with `program` in place of `find . sandboxes ../`.
fn find_shape(proj: &Path, program: &str) -> String {
    format!(
        "cd {}; f=$({program} -maxdepth 6 -path '*r4/review/x.md' 2>/dev/null | head -1); \
         echo \"$f\"; wc -c \"$f\"; cat \"$f\"",
        proj.display()
    )
}

/// 🔴 RED on main (#10548): `find . sandboxes ../` inside `$( … )` runs no
/// script. Main reads the `.` operand as the `source` builtin and judges the
/// next existing operand as its script, refusing "not a regular file".
#[test]
fn a_find_dot_operand_in_a_substitution_allows_matt_10548() {
    let l = layout();
    let command = find_shape(&l.proj, "find . sandboxes ../");
    let reason = gated(&command, &l.hook, &l.home);
    assert!(reason.is_none(), "must allow: {reason:?}");
}

/// 🔴 RED on main (#10672): a venv python reached through `$S`, running an
/// existing benign `$S/build_test.py` on a binary `.docx`. Main skips the
/// computed script word and judges the first literal existing operand — the
/// `.docx` — refusing "not UTF-8 text".
#[test]
fn a_venv_python_script_through_a_variable_allows_matt_10672() {
    let l = layout();
    let s = l.root.join("scratchpad");
    file(&s, "docxenv/bin/python", b"fake interpreter\n");
    file(&s, "build_test.py", BUILD_PY.as_bytes());
    let docx = file(&l.root, "Desk/Some Name-r4.docx", &FAKE_DOCX);
    let command = format!(
        "S={}; $S/docxenv/bin/python $S/build_test.py \"{}\" $S/test-out.docx",
        s.display(),
        docx.display()
    );
    let reason = gated(&command, &l.hook, &l.home);
    assert!(reason.is_none(), "must allow: {reason:?}");
}

/// CONTROL (#10548): the find shape with a real `.`/`bash` script run in the
/// `find` position, the script reading a credential, refuses — the fix must
/// keep `.` as the `source` builtin in program position.
#[test]
fn a_sourced_credential_script_in_the_find_shape_refuses_10548() {
    let l = layout();
    file(&l.proj, "reader.sh", AWS_READ_SH.as_bytes());
    for program in [". ./reader.sh", "bash ./reader.sh", "source ./reader.sh"] {
        let command = find_shape(&l.proj, program);
        let reason = gated(&command, &l.hook, &l.home).unwrap_or_default();
        assert!(
            reason.contains("reader.sh"),
            "{program}: must refuse: {reason}"
        );
    }
}

/// CONTROL (#10672): the venv shape whose `$S/...` script reads a credential
/// refuses, with and without the `.docx` operand; a `.docx` given as the only
/// operand refuses too.
#[test]
fn a_venv_python_credential_script_or_docx_script_refuses_10672() {
    let l = layout();
    let s = l.root.join("scratchpad");
    file(&s, "docxenv/bin/python", b"fake interpreter\n");
    file(&s, "build_test.py", AWS_READ_PY.as_bytes());
    file(&s, "payload.docx", &FAKE_DOCX);
    let docx = file(&l.root, "Desk/Some Name-r4.docx", &FAKE_DOCX);
    let sd = s.display();
    let commands = [
        format!(
            "S={sd}; $S/docxenv/bin/python $S/build_test.py \"{}\" $S/test-out.docx",
            docx.display()
        ),
        format!("S={sd}; $S/docxenv/bin/python $S/build_test.py $S/test-out.docx"),
        format!("S={sd}; $S/docxenv/bin/python $S/payload.docx"),
    ];
    for command in &commands {
        let reason = gated(command, &l.hook, &l.home);
        assert!(reason.is_some(), "must refuse: {command}");
    }
}

/// Every row whose verdict misses `expect`, so one run names every flip.
fn misses(rows: &[(String, &str)], hook: &Path, home: &Path) -> Vec<String> {
    rows.iter()
        .filter_map(|(command, expect)| {
            let reason = gated(command, hook, home).unwrap_or_default();
            (!reason.contains(expect)).then(|| format!("{command} => {reason:?}"))
        })
        .collect()
}

/// CONTROL (#10548): masking a substitution's words keeps every script it
/// runs judged — nested, backtick, unclosed, and a top-level `.` beside it.
#[test]
fn substitution_runs_and_a_sourced_script_still_refuse_10548() {
    let l = layout();
    file(&l.proj, "reader.sh", AWS_READ_SH.as_bytes());
    let cd = format!("cd {};", l.proj.display());
    let rows: Vec<(String, &str)> = [
        "f=$(echo $(. ./reader.sh) | head -1); echo \"$f\"",
        "f=$(echo $(bash ./reader.sh)); echo \"$f\"",
        "f=`. ./reader.sh | head -1`; echo \"$f\"",
        ". ./reader.sh",
        "f=$(find . sandboxes ../ -maxdepth 2 | head -1); . ./reader.sh",
        // Fail-open arm: an unclosed `$(` still has its body judged.
        "f=$(. ./reader.sh | head -1",
    ]
    .iter()
    .map(|tail| (format!("{cd} {tail}"), "reader.sh"))
    .collect();
    let missed = misses(&rows, &l.hook, &l.home);
    assert!(missed.is_empty(), "must refuse: {missed:#?}");
}

/// CONTROL (#10672): a `$NAME` script word resolves only from a plain literal
/// assigned unconditionally just before it; every other shape stays computed
/// at run time and refuses, though the script it would name is benign.
#[test]
fn an_untracked_variable_script_refuses_10672() {
    let l = layout();
    let s = l.root.join("scratchpad");
    file(&s, "build_test.py", BUILD_PY.as_bytes());
    file(&s, "payload.docx", &FAKE_DOCX);
    file(&s, "notes.txt", b"plain notes\n");
    // #10548: `$S` holding a spaced value word-splits, so the shell runs a
    // different path than the spaced one that exists here.
    let spaced = l.root.join("a b");
    file(&spaced, "build_test.py", BUILD_PY.as_bytes());
    let sd = s.display();
    let computed = "computed at run time";
    let rows: Vec<(String, &str)> = vec![
        ("python3 $S/build_test.py".into(), computed),
        (
            format!("S={sd}; S=$(evil); python3 $S/build_test.py"),
            computed,
        ),
        (format!("S={sd} python3 $S/build_test.py"), computed),
        (
            format!("false && S={sd}; python3 $S/build_test.py"),
            computed,
        ),
        (format!("S={sd} | python3 $S/build_test.py"), computed),
        (
            format!("S={sd}; cd {sd}; python3 $S/build_test.py"),
            computed,
        ),
        (
            format!("for i in 1; do S={sd}; done; python3 $S/build_test.py"),
            computed,
        ),
        (format!("S={sd}; python3 '$S'/build_test.py"), computed),
        (format!("S={sd}; python3 '$S/build_test.py'"), computed),
        (format!("S={sd}; python3 \\$S/build_test.py"), computed),
        (
            format!("S={sd}; python3 $S/build_test.py; bash -c 'python3 $S/build_test.py'"),
            computed,
        ),
        (
            format!("printf x > {sd}/later.py; S={sd}; python3 $S/later.py {sd}/notes.txt"),
            computed,
        ),
        (
            format!("S={sd}; python3 ${{S:-/x}}/build_test.py"),
            computed,
        ),
        (
            format!("S=\"{}\"; python3 $S/build_test.py", spaced.display()),
            computed,
        ),
        (format!("python3 -X dev {sd}/payload.docx"), "not UTF-8"),
    ];
    let missed = misses(&rows, &l.hook, &l.home);
    assert!(missed.is_empty(), "must refuse: {missed:#?}");
}

/// #10672: the braced form after `&&` resolves too, and the operand after the
/// script is data, never read as the script.
#[test]
fn a_braced_variable_script_after_and_and_allows_10672() {
    let l = layout();
    let s = l.root.join("scratchpad");
    file(&s, "build_test.py", BUILD_PY.as_bytes());
    file(&s, "payload.docx", &FAKE_DOCX);
    let command = format!(
        "S={} && python3 \"${{S}}/build_test.py\" $S/payload.docx",
        s.display()
    );
    let reason = gated(&command, &l.hook, &l.home);
    assert!(reason.is_none(), "must allow: {reason:?}");
}
