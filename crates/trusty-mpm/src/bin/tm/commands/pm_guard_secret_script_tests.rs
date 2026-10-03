//! Unit rows for `pm_guard_secret_script` (#8879). Every fixture is a fake
//! placeholder; nothing is executed.

use super::*;

use std::os::unix::fs::PermissionsExt;

use crate::commands::pm_guard_secret_script_read::MAX_SCRIPT_BYTES;

/// A Keychain read that prints its value — refused inline (#8596).
const KEYCHAIN_READ: &str = "security find-generic-password -s fake-svc -w\n";

/// Write `body` to `dir/name` and return the path.
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&path, body).expect("write script");
    path
}

fn eval(command: &str, cwd: &Path) -> Option<String> {
    evaluate_script_body_secret_read(command, cwd)
}

/// 🔴 REGRESSION (#8879): the refused inline Keychain read, moved into a
/// script run by an interpreter, is refused.
#[test]
fn refuses_a_keychain_read_in_a_bash_script() {
    let dir = tempfile::tempdir().expect("dir");
    let path = script(
        dir.path(),
        "probe.sh",
        &format!("#!/bin/bash\n{KEYCHAIN_READ}"),
    );
    for command in [
        format!("bash {}", path.display()),
        "bash probe.sh".to_string(),
        "sh -x ./probe.sh".to_string(),
        "zsh -- probe.sh".to_string(),
        "source probe.sh".to_string(),
        ". ./probe.sh".to_string(),
        "bash < probe.sh".to_string(),
        "timeout 30 bash probe.sh 2>&1".to_string(),
        "echo start && bash probe.sh; echo done".to_string(),
        format!("cd {} && bash probe.sh", dir.path().display()),
    ] {
        let cwd = if command.starts_with("cd ") {
            Path::new("/")
        } else {
            dir.path()
        };
        let reason = eval(&command, cwd).unwrap_or_default();
        assert!(reason.contains("#8879"), "`{command}` must deny: {reason}");
        assert!(reason.contains("Architect"), "{reason}");
        assert!(
            !reason.contains("fake-svc"),
            "never quotes the body: {reason}"
        );
    }
}

/// 🔴 REGRESSION (#8879): a script reading a secret-class file is refused.
#[test]
fn refuses_a_secret_file_read_in_a_python_script() {
    let dir = tempfile::tempdir().expect("dir");
    script(
        dir.path(),
        "sign.py",
        "import subprocess\nkey = open('keys/app.pem').read()\nprint(len(key))\n",
    );
    script(dir.path(), "env.py", "print(open('.env').read())\n");
    script(dir.path(), "cat.sh", "cat keys/app.pem\n");
    script(dir.path(), "glob.sh", "cat .env*\n");
    script(dir.path(), "cont.sh", "cat \\\n  ~/.ssh/id_rsa\n");
    for command in [
        "python3 sign.py",
        "python3 -u env.py --flag",
        "python3 -X dev sign.py",
        "python sign.py",
        "bash cat.sh",
        "bash glob.sh",
        "bash cont.sh",
        "node -r ./cat.sh app.js",
    ] {
        assert!(
            eval(command, dir.path()).is_some_and(|r| r.contains("#7266")),
            "`{command}` must deny"
        );
    }
}

/// 🔴 REGRESSION (#8879): a script run by its own path, by shebang or none.
#[test]
fn refuses_a_script_run_by_its_path() {
    let dir = tempfile::tempdir().expect("dir");
    script(
        dir.path(),
        "probe",
        &format!("#!/usr/bin/env bash\n{KEYCHAIN_READ}"),
    );
    script(dir.path(), "bin/plain", KEYCHAIN_READ);
    script(
        dir.path(),
        "read.py",
        "#!/usr/bin/env python3\nopen('.env').read()\n",
    );
    for command in ["./probe", "./bin/plain arg", "./read.py", "FOO=1 ./probe"] {
        assert!(eval(command, dir.path()).is_some(), "`{command}` must deny");
    }
}

/// 🔴 REGRESSION (#8879): a script that only RUNS another script carrying the
/// read is refused too, to the depth bound.
#[test]
fn refuses_a_read_one_script_deeper() {
    let dir = tempfile::tempdir().expect("dir");
    script(dir.path(), "inner.sh", KEYCHAIN_READ);
    script(dir.path(), "outer.sh", "echo hi\nbash inner.sh\n");
    let reason = eval("bash outer.sh", dir.path()).unwrap_or_default();
    assert!(reason.contains("`outer.sh`"), "{reason}");
}

/// No over-deny: scripts that read no credential, and interpreters running
/// inline code (the inline rules own that), allow.
#[test]
fn allows_a_script_that_reads_no_credential() {
    let dir = tempfile::tempdir().expect("dir");
    script(
        dir.path(),
        "build.sh",
        "#!/usr/bin/env bash\nset -euo pipefail\nfor f in src/*.rs; do\n  echo \"$f\"\ndone\n\
         if [ -n \"${OUT:-}\" ]; then mkdir -p \"$OUT\"; fi\nfn() { echo \"${1:-x}\"; }\n",
    );
    script(
        dir.path(),
        "report.py",
        "import json\nd = {\"a\": [1, 2]}\nprint(json.dumps(d))\nwith open('notes.md') as f:\n    print(f.read())\n",
    );
    script(dir.path(), "probe.sh", KEYCHAIN_READ);
    for command in [
        "bash build.sh",
        "./build.sh",
        "python3 report.py notes.md",
        "python3 -c 'print(1)' probe.sh",
        "bash -c 'echo hi' probe.sh",
        "python3 -m pytest probe.sh",
        "cat probe.sh",
        "bash <<'EOF'\necho hi\nEOF",
        "python3 - <<'PY'\nprint(1)\nPY",
        "bash <<< 'echo hi'",
        "bash -l",
        "python3 --version",
        "cat build.sh | bash",
        "sudo -u nobody bash -c 'echo hi'",
    ] {
        assert_eq!(eval(command, dir.path()), None, "`{command}` must allow");
    }
}

/// A benign body of `len` bytes: no credential, so only fail-closed can deny.
fn benign(len: usize) -> String {
    let mut body = "echo hello\n".repeat(len / 11 + 1);
    body.truncate(len);
    body
}

/// 🔴 REGRESSION (#8879, fail-open check): every arm that cannot judge an
/// existing script's body in full REFUSES, though the body reads no
/// credential — read error, a symlink loop, over the bound, non-UTF-8, a NUL
/// byte, and not a regular file.
#[test]
fn every_unread_class_refuses_8879() {
    let dir = tempfile::tempdir().expect("dir");
    let locked = script(dir.path(), "locked.sh", &benign(40));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    // #9037: a symlink loop cannot be opened, so it is unreadable and refuses.
    std::os::unix::fs::symlink(dir.path().join("loop-b.sh"), dir.path().join("loop-a.sh"))
        .expect("symlink");
    std::os::unix::fs::symlink(dir.path().join("loop-a.sh"), dir.path().join("loop-b.sh"))
        .expect("symlink");
    script(dir.path(), "big.sh", &benign(MAX_SCRIPT_BYTES as usize + 1));
    std::fs::write(dir.path().join("latin1.sh"), b"echo caf\xe9\n").expect("write");
    script(dir.path(), "nul.sh", "echo a\0b\n");
    std::fs::create_dir(dir.path().join("pkg")).expect("mkdir");
    let mut rows = vec![
        ("bash loop-a.sh", "could not read"),
        ("./loop-a.sh", "could not read"),
        ("bash big.sh", "256 KiB"),
        ("python3 latin1.sh", "not UTF-8"),
        ("bash nul.sh", "not UTF-8"),
        ("python3 pkg", "not a regular file"),
    ];
    // Running as root reads a 0o000 file; the read-error arm exists only when it cannot.
    if std::fs::read(&locked).is_err() {
        rows.push(("bash locked.sh", "could not read"));
    }
    for (command, why) in rows {
        let reason = eval(command, dir.path()).unwrap_or_default();
        assert!(
            reason.contains("fails closed"),
            "`{command}` must deny: {reason}"
        );
        assert!(
            reason.contains(why),
            "`{command}` must name `{why}`: {reason}"
        );
    }
}

/// 🔴 REGRESSION (#8879, fail-open check): a script the command runs but
/// whose path the guard cannot resolve REFUSES — a computed interpreter
/// operand or loaded file, and a missing script nothing in the command writes.
/// Each row was allowed at 95893d74cb.
#[test]
fn an_unresolvable_or_missing_script_refuses_8879() {
    let dir = tempfile::tempdir().expect("dir");
    for (command, why) in [
        ("bash \"$SCRIPT\"", "computed at run time"),
        ("source \"$VENV/bin/activate\"", "computed at run time"),
        ("python3 $(mktemp)", "computed at run time"),
        ("bash <(cat probe.sh)", "fails closed"),
        ("node -r \"$HOOK\" app.js", "computed at run time"),
        ("bash ./probe-*.sh", "computed at run time"),
        ("bash missing.sh", "does not exist"),
        ("timeout 30 bash missing.sh 2>&1", "does not exist"),
        ("./missing-probe", "does not exist"),
        ("python3 -W ignore gone.py", "does not exist"),
    ] {
        let reason = eval(command, dir.path()).unwrap_or_default();
        assert!(
            reason.contains("cannot judge its body and fails closed"),
            "`{command}` must deny: {reason}"
        );
        assert!(
            reason.contains(why),
            "`{command}` must name `{why}`: {reason}"
        );
    }
}

/// A `$HOME`/`$PWD` prefix resolves, so the script behind it is judged.
#[test]
fn a_home_or_pwd_prefixed_script_is_judged_8879() {
    let dir = tempfile::tempdir().expect("dir");
    script(dir.path(), "probe.sh", KEYCHAIN_READ);
    script(dir.path(), "ok.sh", "echo hi\n");
    assert!(eval("bash \"$PWD/probe.sh\"", dir.path()).is_some_and(|r| r.contains("#8596")));
    assert_eq!(eval("source ${PWD}/ok.sh", dir.path()), None);
}

/// Ruling 268's accepted trade, pinned: a script an earlier stage of the same
/// command writes (that stage's text is judged inline), a computed path run
/// directly (how a built binary runs), and a compiled executable.
/// A row that starts denying is a residual that closed — update the doc.
#[test]
fn the_documented_residuals_allow() {
    let dir = tempfile::tempdir().expect("dir");
    let mut macho = vec![0xcf, 0xfa, 0xed, 0xfe];
    macho.extend(b"\0\0security find-generic-password -w\xff");
    std::fs::write(dir.path().join("tool"), &macho).expect("write");
    std::os::unix::fs::symlink(dir.path().join("tool"), dir.path().join("tool-link"))
        .expect("symlink");
    script(dir.path(), "bracket.sh", "cat .en[v]\n");
    for command in [
        "printf 'echo hi' > later.sh; bash later.sh",
        "\"$CARGO_TARGET_DIR\"/debug/tm --version",
        "bash bracket.sh",
        "./tool --version",
        "./tool-link --version",
    ] {
        assert_eq!(eval(command, dir.path()), None, "`{command}` is a residual");
    }
}

/// 🔴 REGRESSION (#9037): a symlinked script is judged by its target — a
/// `node_modules/.bin` shim to a clean script allows, a link to a credential
/// read refuses. Before the fix every symlinked script refused.
#[test]
fn a_symlinked_script_is_judged_by_its_target_9037() {
    let dir = tempfile::tempdir().expect("dir");
    let cli = script(
        dir.path(),
        "node_modules/pkg/cli.js",
        "#!/bin/sh\necho hello\n",
    );
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let bin = dir.path().join("node_modules/.bin");
    std::fs::create_dir_all(&bin).expect("mkdir");
    std::os::unix::fs::symlink("../pkg/cli.js", bin.join("tool")).expect("symlink");
    let probe = script(dir.path(), "probe.sh", KEYCHAIN_READ);
    std::os::unix::fs::symlink(&probe, dir.path().join("probe-link.sh")).expect("symlink");
    for command in [
        "./node_modules/.bin/tool --version",
        "sh node_modules/.bin/tool",
    ] {
        assert_eq!(eval(command, dir.path()), None, "`{command}` must allow");
    }
    for command in ["bash probe-link.sh", "./probe-link.sh"] {
        let reason = eval(command, dir.path()).unwrap_or_default();
        assert!(reason.contains("#8596"), "`{command}` must deny: {reason}");
    }
}

/// 🔴 REGRESSION (#9037): a script run inside a body is resolved as strictly
/// as the top level — a computed or missing nested script refuses. Both rows
/// were allowed (documented residual 3) before the fix.
#[test]
fn a_nested_unresolvable_script_refuses_9037() {
    let dir = tempfile::tempdir().expect("dir");
    script(dir.path(), "arg.sh", "echo start\nbash \"$1\"\n");
    script(dir.path(), "lib.sh", "source \"$LIB/x.sh\"\n");
    script(dir.path(), "gone.sh", "bash ./never-written.sh\n");
    for (command, why) in [
        ("bash arg.sh probe.sh", "computed at run time"),
        ("bash lib.sh", "computed at run time"),
        ("./gone.sh", "does not exist"),
    ] {
        let reason = eval(command, dir.path()).unwrap_or_default();
        assert!(reason.contains(why), "`{command}` must deny: {reason}");
    }
}

/// #9037: a case arm's pattern is not a script run (strict nested resolution
/// would read `docs/x.md)` as a missing script); the arm's command is judged.
#[test]
fn a_case_arm_pattern_is_not_a_script_9037() {
    let dir = tempfile::tempdir().expect("dir");
    script(dir.path(), "probe.sh", KEYCHAIN_READ);
    script(
        dir.path(),
        "arms.sh",
        "case \"$1\" in\n  docs/x.md) ;;\n  *) echo other ;;\nesac\n",
    );
    assert_eq!(eval("bash arms.sh", dir.path()), None);
    script(
        dir.path(),
        "run.sh",
        "case \"$1\" in\n  go) bash probe.sh ;;\nesac\n",
    );
    let reason = eval("bash run.sh", dir.path()).unwrap_or_default();
    assert!(
        reason.contains("#8596"),
        "the arm's command is judged: {reason}"
    );
}

/// 🔴 REGRESSION (#9037): a chain of scripts longer than the depth bound
/// refuses; one at the bound is judged in full and allows. A 5-script chain
/// was allowed before the fix.
#[test]
fn a_chain_past_the_depth_bound_refuses_9037() {
    let chain = |len: usize| {
        let dir = tempfile::tempdir().expect("dir");
        for i in 0..len {
            let next = if i + 1 < len {
                format!("bash s{}.sh\n", i + 1)
            } else {
                "echo end\n".to_string()
            };
            script(dir.path(), &format!("s{i}.sh"), &next);
        }
        eval("bash s0.sh", dir.path())
    };
    assert_eq!(chain(MAX_SCRIPT_DEPTH), None, "a chain at the bound allows");
    let reason = chain(MAX_SCRIPT_DEPTH + 1).unwrap_or_default();
    assert!(
        reason.contains("depth bound"),
        "past the bound must deny: {reason}"
    );
}

/// #9037: a script that sources a helper through its own path has that
/// helper judged, not refused as unresolvable.
#[test]
fn self_location_idioms_resolve_9037() {
    let dir = tempfile::tempdir().expect("dir");
    let run = dir.path().join("scripts/run.sh");
    let body = "if [ -z \"${BASH_VERSION:-}\" ]; then exec bash \"$0\" \"$@\"; fi\n\
                SCRIPT_DIR=\"$(cd \"$(dirname \"${BASH_SOURCE[0]}\")\" && pwd)\"\n\
                ROOT=\"$(cd \"$SCRIPT_DIR/..\" && pwd)\"\n\
                . \"$SCRIPT_DIR/lib/a.sh\"\n\
                . \"$(cd \"$(dirname \"$0\")\" && pwd)/lib/b.sh\"\n\
                bash \"$ROOT/tools/c.sh\"\n";
    let scripts = dir.path().join("scripts");
    let resolved = resolve_self_paths(body, &run);
    assert!(
        resolved.contains(&format!(". \"{}/lib/a.sh\"", scripts.display())),
        "{resolved}"
    );
    assert!(
        resolved.contains(&format!(". \"{}/lib/b.sh\"", scripts.display())),
        "{resolved}"
    );
    assert!(
        resolved.contains(&format!("bash \"{}/tools/c.sh\"", dir.path().display())),
        "{resolved}"
    );
    script(dir.path(), "scripts/run.sh", body);
    script(dir.path(), "scripts/lib/a.sh", "echo a\n");
    script(dir.path(), "scripts/lib/b.sh", "echo b\n");
    script(dir.path(), "tools/c.sh", "echo c\n");
    assert_eq!(eval("bash scripts/run.sh", dir.path()), None);
    script(dir.path(), "scripts/lib/b.sh", KEYCHAIN_READ);
    let reason = eval("bash scripts/run.sh", dir.path()).unwrap_or_default();
    assert!(
        reason.contains("#8596"),
        "the sourced helper is judged: {reason}"
    );
    // A variable bound twice is not pinned to either value; it stays computed.
    let twice = "D=/a\nD=/b\nbash \"$D/x.sh\"\n";
    assert!(resolve_self_paths(twice, &run).contains("$D/x.sh"));
}

/// No over-deny on this repository's own gate scripts, run by path.
#[test]
fn the_repo_gate_scripts_allow_8879() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    // #8879: bare names, joined below, so no `scripts/<name>.sh` literal sits in
    // crate source for scripts/select-test-crates.sh to read as a reference.
    for gate in [
        "check_line_cap.sh",
        "check_test_pointers.sh",
        "check_changelog_fragment.sh",
        "check_sld.sh",
    ] {
        let command = format!("./scripts/{gate} --staged");
        assert_eq!(eval(&command, &root), None, "`{command}` must allow");
    }
}

/// Each unread class is named, and a FIFO never blocks the read.
#[test]
fn read_script_reports_each_unread_class() {
    let dir = tempfile::tempdir().expect("dir");
    let missing = read_script(&dir.path().join("nope"));
    assert_eq!(missing, Err(Unread::Unreadable));
    assert_eq!(read_script(dir.path()), Err(Unread::NotRegular));
    let big = script(dir.path(), "big", &benign(MAX_SCRIPT_BYTES as usize + 1));
    assert_eq!(read_script(&big), Err(Unread::TooLarge));
    let at_bound = script(dir.path(), "at", &benign(MAX_SCRIPT_BYTES as usize));
    assert!(matches!(read_script(&at_bound), Ok(Body::Script(_))));
    let bin = script(dir.path(), "bin", "a\0b");
    assert_eq!(read_script(&bin), Err(Unread::NotText));
    let elf = dir.path().join("elf");
    std::fs::write(&elf, b"\x7fELF\x02\x01\x01\0").expect("write");
    assert_eq!(read_script(&elf), Ok(Body::Executable));
    let fifo = dir.path().join("fifo");
    let c = std::ffi::CString::new(fifo.display().to_string()).expect("cstr");
    // SAFETY: `c` is a valid NUL-terminated path for the duration of the call.
    if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } == 0 {
        assert_eq!(read_script(&fifo), Err(Unread::NotRegular));
    }
}

/// A full-line comment is dropped before judging, so an apostrophe in prose
/// cannot unbalance the quoting; a comment line carrying a substitution stays,
/// and a backslash-newline continuation joins.
#[test]
fn without_comment_lines_keeps_a_substitution_8879() {
    let body = "#!/bin/sh\n# the crate's gate \\\n  # x\necho hi \\\n  there\n#$(cat .env)\n";
    assert_eq!(
        without_comment_lines(body),
        "\n\n\necho hi    there\n#$(cat .env)\n"
    );
}

/// The inline equivalent never lets a body line close the here-document; a
/// shell body is its own inline command text.
#[test]
fn inline_equivalent_picks_a_delimiter_the_body_does_not_carry() {
    assert_eq!(inline_equivalent("bash", "echo a | wc"), "echo a | wc");
    let text = inline_equivalent("python3", "a = 1\nTM_PM_GUARD_SCRIPT_BODY\nb = 2");
    assert!(
        text.starts_with("python3 <<'TM_PM_GUARD_SCRIPT_BODY_'\n"),
        "{text}"
    );
    assert!(text.ends_with("\nTM_PM_GUARD_SCRIPT_BODY_\n"), "{text}");
}
