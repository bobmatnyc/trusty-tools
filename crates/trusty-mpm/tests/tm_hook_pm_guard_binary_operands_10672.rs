//! End-to-end proof that `tm hook --pm-guard` judges the program a command
//! runs and not the data handed to it (#10672).
//!
//! Why: a data argument was taken for the script when the script word did not
//! resolve, so a Word or PDF path refused as a non-UTF-8 "script", and a text
//! data file could stand in for a missing script and allow.
//! What: spawns the built `tm` with an agent payload over commands whose data
//! files are synthetic bytes built here. Nothing is executed.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_binary_operands_10672::`.

use crate::common;

use std::io::Write;
use std::path::Path;
use std::process::Stdio;

/// Nothing listens on port 1, so a deny's best-effort audit POST fails fast.
const UNREACHABLE_DAEMON: &str = "http://127.0.0.1:1";

/// The first bytes of a zip container, then bytes that are not UTF-8.
const BINARY: &[u8] = &[b'P', b'K', 3, 4, 0, 0xff, 0xfe, 0x80, 0, 1];

/// A script taking data paths from argv through a Word library.
const BUILD_SHAPE: &str = "import sys\nfrom docx import Document\n\nfrom_path, to_path = sys.argv[1], sys.argv[2]\ndoc = Document(from_path)\nprint(\"count\", len(doc.paragraphs), len(doc.tables))\nnames = sorted({q.style.name for q in doc.paragraphs})\nprint(\"names\", names)\ngrid = doc.tables[0]\nprint(\"grid\", grid.style.name if grid.style else None, len(grid.rows))\nfor row in grid.rows:\n    print(\" |\", row.cells[0].text[:9], \"|\", row.cells[-1].text[:9].replace(\"\\n\", \" / \"))\ndoc.save(to_path)\nprint(\"wrote\", to_path)\n";

/// A read-only walk of a Word file. `ps` is the local name that read as a
/// process dump to the shell judges (#8756) before the Python reader.
const INSPECT_SHAPE: &str = "import sys\nfrom docx import Document\n\ndoc = Document(sys.argv[1])\nprint(\"count:\", len(doc.tables))\nfor idx in [0, 1]:\n    grid = doc.tables[idx]\n    print(\"GRID\", idx, \"name:\", grid.style.name if grid.style else None, len(grid.rows), len(grid.columns))\n    for row in grid.rows[:5]:\n        out = []\n        for cell in row.cells:\n            ps = cell.paragraphs\n            out.append([(q.style.name, q.text[:9], any(u.italic for u in q.runs), any(u.bold for u in q.runs)) for q in ps])\n        print(\"  LINE\", out)\n# order of blocks in the body\nroot = doc.element.body\nblocks = []\nfor node in root.iterchildren():\n    kind = node.tag.split('}')[1]\n    if kind == 'p':\n        words = ''.join(n.text or '' for n in node.iter() if n.tag.endswith('}t'))\n        blocks.append(('p', words[:9]))\n    elif kind == 'tbl':\n        blocks.append(('TBL', ''))\nfor item in blocks[2:6]:\n    print(item)\n";

/// A literal Keychain read, as Python.
const KEYCHAIN_PY: &str = "import subprocess\nsubprocess.run([\"security\", \"find-generic-password\", \"-s\", \"x\", \"-w\"])\n";

/// The refusal text of a body that is not text.
const NOT_TEXT: &str = "its body is not UTF-8 text";

/// The guard's refusal reason for `command`, or `None` for an allow.
fn verdict(command: &str, cwd: &Path) -> Option<String> {
    let home = tempfile::tempdir().expect("home");
    common::write_disk_threshold(home.path(), 100);
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "agent_id": "agent-binary-operands",
        "agent_type": "local-ops",
        "cwd": cwd.display().to_string(),
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut child = common::tm_command_in(home.path())
        .args(["--url", UNREACHABLE_DAEMON, "hook", "--pm-guard"])
        .current_dir(cwd)
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT")
        .env_remove("TM_MANAGED_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `tm hook --pm-guard`");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success(), "`{command}` must exit 0");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let line = stdout.lines().next()?;
    let parsed: serde_json::Value = serde_json::from_str(line).expect("deny is JSON");
    assert_eq!(
        parsed["hookSpecificOutput"]["permissionDecision"], "deny",
        "`{command}`: {stdout}"
    );
    let reason = parsed["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("a deny carries a reason");
    Some(reason.to_string())
}

/// A directory holding the data files and the scripts every row names.
fn fixtures() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("dir");
    let d = dir.path();
    for name in ["in.docx", "in.pdf", "out.docx", "blob.docx", "blob"] {
        std::fs::write(d.join(name), BINARY).expect("write binary fixture");
    }
    for (name, body) in [
        ("script.py", BUILD_SHAPE),
        ("inspect.py", INSPECT_SHAPE),
        ("creds.py", KEYCHAIN_PY),
        // A text script named like data is still the script.
        ("x.docx", KEYCHAIN_PY),
        ("notes.txt", "plain words\n"),
        ("ok.sh", "echo hi\n"),
        ("ok.js", "console.log(1)\n"),
        ("ok.rb", "puts 1\n"),
        ("ok.pl", "print 1;\n"),
    ] {
        std::fs::write(d.join(name), body).expect("write text fixture");
    }
    let creds_sh = "security find-generic-password -s x -w\n";
    std::fs::write(d.join("creds.sh"), creds_sh).expect("write text fixture");
    dir
}

/// 🔴 REGRESSION (#10672): data around a script that resolves is not read as
/// a body. The rows naming a data file before the script, or as a word an
/// option takes, refused before the fix.
#[test]
fn plain_script_with_binary_args_allows_10672() {
    let dir = fixtures();
    for command in [
        "python3 script.py in.docx out.docx",
        "python3 script.py --flag in.pdf",
        "python3 script.py -- in.docx",
        "python3 -u script.py in.docx out.docx",
        "python3 -W ignore script.py in.docx",
        "python3 -X dev script.py in.docx",
        "python3 -W in.docx script.py",
        "python3 -uX in.pdf script.py",
        "python3 --check-hash-based-pycs in.pdf script.py",
        "python3 < in.docx script.py",
        "python3 script.py < in.docx",
        "python3 script.py > out.docx 2>&1",
        "bash ok.sh in.docx",
        "bash -o pipefail ok.sh in.docx",
        "bash -euo pipefail ok.sh in.docx",
        "bash +o in.docx ok.sh",
        "bash +x ok.sh in.docx",
        "bash ok.sh --rcfile in.docx",
        "source ok.sh in.docx",
        "node ok.js in.docx",
        "node ok.js -r in.docx",
        "node -C in.docx ok.js",
        "node --conditions dev ok.js",
        "ruby -I in.docx ok.rb in.pdf",
        "ruby ok.rb -r in.docx",
        "perl -I in.docx ok.pl in.pdf",
        "perl -w ok.pl in.pdf",
    ] {
        assert_eq!(verdict(command, dir.path()), None, "`{command}` must allow");
    }
}

/// 🔴 REGRESSION (#10672): an unresolved or missing script keeps its fail-closed
/// refusal, naming the script. Before the fix the binary rows named the data,
/// and the text rows allowed because the data was judged as the script.
#[test]
fn an_unresolved_script_does_not_fall_through_to_data_10672() {
    let dir = fixtures();
    for (command, script, why) in [
        ("python3 \"$S\" in.docx", "$S", "computed at run time"),
        ("python3 \"$S\" notes.txt", "$S", "computed at run time"),
        ("python3 \"$S\" < in.docx", "$S", "computed at run time"),
        (
            "S=script.py; python3 \"$S\" in.docx",
            "$S",
            "computed at run time",
        ),
        ("python3 missing.py in.docx", "missing.py", "does not exist"),
        (
            "python3 missing.py notes.txt",
            "missing.py",
            "does not exist",
        ),
        (
            "python3 missing.py < in.docx",
            "missing.py",
            "does not exist",
        ),
        (
            "python3 missing.py < notes.txt",
            "missing.py",
            "does not exist",
        ),
        (
            "python3 --flag missing.py in.docx",
            "missing.py",
            "does not exist",
        ),
        (
            "python3 -W ignore missing.py in.docx",
            "missing.py",
            "does not exist",
        ),
        ("bash missing.sh in.docx", "missing.sh", "does not exist"),
        ("bash missing.sh notes.txt", "missing.sh", "does not exist"),
        ("node missing.js in.docx", "missing.js", "does not exist"),
    ] {
        let reason = verdict(command, dir.path()).unwrap_or_default();
        assert!(
            reason.contains(&format!("the script `{script}`")),
            "`{command}` must name `{script}`: {reason}"
        );
        assert!(
            reason.contains(why),
            "`{command}` must say `{why}`: {reason}"
        );
        assert!(
            !reason.contains("in.docx") && !reason.contains("notes.txt"),
            "`{command}` must not name the data: {reason}"
        );
    }
}

/// A binary file that IS the program stays refused, as non-UTF-8 (#8879). A
/// data-like name does not change that, nor does a real script after it.
#[test]
fn a_binary_program_stays_refused_10672() {
    let dir = fixtures();
    for (command, program) in [
        ("python3 blob.docx", "blob.docx"),
        ("python3 in.docx script.py", "in.docx"),
        ("./blob", "./blob"),
        ("bash blob", "blob"),
        ("bash -o pipefail blob ok.sh", "blob"),
        ("python3 < blob.docx", "blob.docx"),
    ] {
        let reason = verdict(command, dir.path()).unwrap_or_default();
        assert!(
            reason.contains(NOT_TEXT) && reason.contains(&format!("the script `{program}`")),
            "`{command}` must refuse `{program}` as not text: {reason}"
        );
    }
}

/// 🔴 REGRESSION (#10672): credential controls hold. The rows with an option
/// after the script allowed before the fix: the parser read `-c` or `-` there
/// as inline code and dropped the script.
#[test]
fn a_credential_script_is_denied_whatever_its_arguments_10672() {
    let dir = fixtures();
    for (command, cite) in [
        ("python3 creds.py", "#8596"),
        ("python3 creds.py in.docx", "#8596"),
        ("python3 creds.py -c x", "#8596"),
        ("python3 creds.py - x", "#8596"),
        ("python3 -W in.docx creds.py", "#8596"),
        ("bash creds.sh in.docx", "#8596"),
        ("bash creds.sh -c x", "#8596"),
        ("bash -o pipefail creds.sh", "#8596"),
        ("python3 x.docx", "#8596"),
        ("python3 x.docx in.docx", "#8596"),
        ("cat ~/.netrc", "#7266"),
        (
            "python3 -c 'import subprocess; subprocess.run([\"security\", \"find-generic-password\", \"-s\", \"x\", \"-w\"])'",
            "#8596",
        ),
        // A data argument naming a secret path keeps the verdict main gives it.
        ("python3 script.py ~/.netrc", "#7266"),
        ("python3 script.py .env", "#7266"),
        ("python3 missing.py .env", "#7266"),
    ] {
        let reason = verdict(command, dir.path()).unwrap_or_default();
        assert!(
            reason.contains(cite),
            "`{command}` must deny citing {cite}: {reason}"
        );
    }
}

/// The read-only inspect body plus a secret read or a dump the guard knows
/// refuses.
#[test]
fn a_read_only_inspect_shape_with_a_secret_read_refuses_10672() {
    // #10672: the allow half moved to the ps-as-identifier follow-up issue.
    let dir = fixtures();
    let d = dir.path();
    for (name, tail, cite) in [
        (
            "netrc.py",
            "import os\nprint(open(os.path.expanduser(\"~/.netrc\")).read())\n",
            "#7266",
        ),
        ("keychain.py", KEYCHAIN_PY, "#8596"),
        (
            "ps_eww.py",
            "import subprocess\nsubprocess.run([\"ps\", \"eww\"])\n",
            "#8756",
        ),
        (
            "proc.py",
            "print(open(\"/proc/1/environ\").read())\n",
            "#8756",
        ),
        (
            "launchctl.py",
            "import subprocess\nsubprocess.run([\"launchctl\", \"print\", \"gui/501/x\"])\n",
            "#8756",
        ),
    ] {
        std::fs::write(d.join(name), format!("{INSPECT_SHAPE}{tail}")).expect("write");
        let command = format!("python3 {name} in.docx");
        let reason = verdict(&command, d).unwrap_or_default();
        assert!(
            reason.contains(cite),
            "`{command}` must deny citing {cite}: {reason}"
        );
    }
}
