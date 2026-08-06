//! Tests for `tm shell-init` — the emitted shell wrapper.
//!
//! Why: the output is text a user pipes into `eval` in an interactive shell. A
//! malformed snippet breaks their shell session, not a test run. Two things
//! therefore need pinning:
//! - the exact emitted text per dialect, so any edit to the snippet is a
//!   deliberate, reviewed change rather than a silent drift;
//! - the actual runtime behavior, exercised by running the emitted snippet
//!   under a real `bash` against a stub `tm` on `PATH`. The failure case that
//!   matters most is `tm path` returning nothing or failing: the wrapper must
//!   not `cd`, must not error, and must still return the real exit status.
//!
//! What: golden-text assertions for zsh/bash/fish, plus a behavioral suite
//! driven by `bash` (present on every platform this crate builds on) and a
//! `bash -n` parse of the POSIX body. zsh and fish are parsed with `zsh -n` /
//! `fish --no-execute` outside the suite — neither is guaranteed on a Linux CI
//! runner, so gating a test on their presence would silently skip.
//! Test: this file.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use super::shell_init_snippet;
use crate::cli::ShellArg;

/// The POSIX function body, verbatim. Shared by the zsh and bash goldens.
const EXPECTED_POSIX_BODY: &str = r#"tm() {
    command tm "$@"
    local _tm_status=$?
    if [ "${1:-}" != "run" ]; then
        return $_tm_status
    fi
    shift
    local _tm_alias='' _tm_root='' _tm_pending='' _tm_arg='' _tm_target=''
    for _tm_arg in "$@"; do
        if [ -n "$_tm_pending" ]; then
            if [ "$_tm_pending" = "--root" ]; then
                _tm_root="$_tm_arg"
            fi
            _tm_pending=''
            continue
        fi
        case "$_tm_arg" in
            --root=*) _tm_root="${_tm_arg#--root=}" ;;
            --root|--task) _tm_pending="$_tm_arg" ;;
            -*) ;;
            *) if [ -z "$_tm_alias" ]; then _tm_alias="$_tm_arg"; fi ;;
        esac
    done
    if [ -z "$_tm_alias" ]; then
        return $_tm_status
    fi
    if [ -n "$_tm_root" ]; then
        _tm_target=$(command tm path "$_tm_alias" --root "$_tm_root" 2>/dev/null) || _tm_target=''
    else
        _tm_target=$(command tm path "$_tm_alias" 2>/dev/null) || _tm_target=''
    fi
    if [ -n "$_tm_target" ] && [ -d "$_tm_target" ]; then
        cd "$_tm_target" || return $_tm_status
    fi
    return $_tm_status
}
"#;

// ---------------------------------------------------------------------------
// Golden text
// ---------------------------------------------------------------------------

#[test]
fn zsh_snippet_matches_golden() {
    let expected = format!(
        "{}{}",
        r#"# trusty-mpm shell integration (zsh).
# Add to ~/.zshrc:  eval "$(tm shell-init zsh)"
# After `tm run <alias>` exits, your shell is left in that alias's repo.
"#,
        EXPECTED_POSIX_BODY
    );
    assert_eq!(shell_init_snippet(ShellArg::Zsh), expected);
}

#[test]
fn bash_snippet_matches_golden() {
    let expected = format!(
        "{}{}",
        r#"# trusty-mpm shell integration (bash).
# Add to ~/.bashrc:  eval "$(tm shell-init bash)"
# After `tm run <alias>` exits, your shell is left in that alias's repo.
"#,
        EXPECTED_POSIX_BODY
    );
    assert_eq!(shell_init_snippet(ShellArg::Bash), expected);
}

#[test]
fn fish_snippet_matches_golden() {
    let expected = r#"# trusty-mpm shell integration (fish).
# Add to ~/.config/fish/config.fish:  tm shell-init fish | source
# After `tm run <alias>` exits, your shell is left in that alias's repo.
function tm
    command tm $argv
    set -l _tm_status $status
    if test (count $argv) -eq 0; or test "$argv[1]" != "run"
        return $_tm_status
    end
    set -l _tm_rest $argv
    set -e _tm_rest[1]
    set -l _tm_alias ""
    set -l _tm_root ""
    set -l _tm_pending ""
    for _tm_arg in $_tm_rest
        if test -n "$_tm_pending"
            if test "$_tm_pending" = "--root"
                set _tm_root $_tm_arg
            end
            set _tm_pending ""
            continue
        end
        switch $_tm_arg
            case '--root=*'
                set _tm_root (string replace -r -- '^--root=' '' $_tm_arg)
            case '--root' '--task'
                set _tm_pending $_tm_arg
            case '-*'
            case '*'
                if test -z "$_tm_alias"
                    set _tm_alias $_tm_arg
                end
        end
    end
    if test -z "$_tm_alias"
        return $_tm_status
    end
    set -l _tm_target ""
    if test -n "$_tm_root"
        set _tm_target (command tm path $_tm_alias --root $_tm_root 2>/dev/null)
    else
        set _tm_target (command tm path $_tm_alias 2>/dev/null)
    end
    if test -n "$_tm_target"; and test -d "$_tm_target"
        cd "$_tm_target"
    end
    return $_tm_status
end
"#;
    assert_eq!(shell_init_snippet(ShellArg::Fish), expected);
}

#[test]
fn zsh_and_bash_share_one_body() {
    // One implementation of the wrapper, two installation headers. If the
    // dialects ever have to diverge, this assertion is the place that says so.
    let zsh = shell_init_snippet(ShellArg::Zsh);
    let bash = shell_init_snippet(ShellArg::Bash);
    assert_ne!(zsh, bash, "headers must name the shell they install into");
    assert!(zsh.ends_with(EXPECTED_POSIX_BODY));
    assert!(bash.ends_with(EXPECTED_POSIX_BODY));
}

#[test]
fn every_dialect_calls_command_tm_and_never_recurses() {
    for shell in [ShellArg::Zsh, ShellArg::Bash, ShellArg::Fish] {
        let s = shell_init_snippet(shell);
        assert!(
            s.contains("command tm"),
            "{shell:?}: must invoke `command tm`, or the function recurses"
        );
        // Every `tm ` invocation inside the body is prefixed by `command`. Any
        // line that runs a bare `tm` would loop forever the first time a user
        // typed `tm`.
        for line in s.lines() {
            let t = line.trim();
            if t.starts_with('#') {
                continue;
            }
            assert!(
                !t.starts_with("tm ") && t != "tm",
                "{shell:?}: bare `tm` call would recurse: {line}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Behavior, under a real shell
// ---------------------------------------------------------------------------

/// Stub `tm` placed on `PATH` for the behavioral suite.
///
/// `run` and `status` exit with distinct non-zero codes so the wrapper's
/// pass-through can be told apart from a swallowed status. `path` resolves
/// only the aliases each case needs, so an unresolvable alias is a real
/// failure rather than a lucky empty string.
const STUB_TM: &str = r#"#!/bin/sh
case "$1" in
  run) exit 7 ;;
  status) exit 3 ;;
  path)
    shift
    _alias=''
    _root=''
    while [ $# -gt 0 ]; do
      case "$1" in
        --root) shift; _root="$1" ;;
        *) if [ -z "$_alias" ]; then _alias="$1"; fi ;;
      esac
      shift
    done
    if [ -n "$_root" ]; then
      if [ "$_alias" = "rooted" ]; then printf '%s\n' "$TM_TEST_ROOTED"; exit 0; fi
      exit 1
    fi
    case "$_alias" in
      "good alias") printf '%s\n' "$TM_TEST_TARGET"; exit 0 ;;
      "empty") exit 0 ;;
      "gone") printf '%s\n' "$TM_TEST_TARGET/does-not-exist"; exit 0 ;;
      *) exit 1 ;;
    esac ;;
esac
exit 0
"#;

struct Harness {
    _dir: TempDir,
    script: std::path::PathBuf,
    bin: std::path::PathBuf,
    start: std::path::PathBuf,
    target: std::path::PathBuf,
    rooted: std::path::PathBuf,
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Build a temp dir holding the stub `tm`, a start dir, two target dirs, and a
/// driver script that sources the emitted snippet and runs one named case.
fn harness(shell: ShellArg) -> Harness {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let bin = root.join("bin");
    let start = root.join("start");
    let target = root.join("target dir");
    let rooted = root.join("rooted");
    for d in [&bin, &start, &target, &rooted] {
        fs::create_dir_all(d).unwrap();
    }
    write_exec(&bin.join("tm"), STUB_TM);

    // The driver prints `rc=<status>` and `pwd=<cwd>` so each case can assert
    // on both halves of the contract at once.
    let script = root.join("driver.sh");
    let body = format!(
        "{snippet}\ncd \"$1\" || exit 99\ncase \"$2\" in\n{cases}\nesac\nprintf 'rc=%s\\n' \"$?\"\nprintf 'pwd=%s\\n' \"$(pwd)\"\n",
        snippet = shell_init_snippet(shell),
        cases = concat!(
            "  good) tm run \"good alias\" ;;\n",
            "  bad) tm run nosuchalias ;;\n",
            "  empty) tm run empty ;;\n",
            "  gone) tm run gone ;;\n",
            "  nonrun) tm status ;;\n",
            "  noalias) tm run ;;\n",
            "  task) tm run --task 'good alias is not the task' 'good alias' ;;\n",
            "  rooted) tm run --root /custom rooted ;;\n",
            "  rootedeq) tm run --root=/custom rooted ;;\n",
            "  rootless) tm run rooted ;;\n",
        ),
    );
    fs::write(&script, body).unwrap();

    Harness {
        _dir: dir,
        script,
        bin,
        start,
        target,
        rooted,
    }
}

/// Run one named case under `bash` and return `(rc, pwd)` as the driver saw them.
fn run_case(h: &Harness, case: &str) -> (i32, String) {
    let out = Command::new("bash")
        .arg(&h.script)
        .arg(&h.start)
        .arg(case)
        .env("PATH", format!("{}:/usr/bin:/bin", h.bin.display()))
        .env("TM_TEST_TARGET", &h.target)
        .env("TM_TEST_ROOTED", &h.rooted)
        .output()
        .expect("bash must be available");
    assert!(
        out.status.success(),
        "driver failed for case {case}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let mut rc = None;
    let mut pwd = None;
    for line in stdout.lines() {
        if let Some(v) = line.strip_prefix("rc=") {
            rc = Some(v.parse::<i32>().unwrap());
        } else if let Some(v) = line.strip_prefix("pwd=") {
            pwd = Some(v.to_string());
        }
    }
    // Nothing but the driver's own two lines may reach stdout — the wrapper is
    // silent on both the happy path and every failure path.
    assert_eq!(
        stdout.lines().count(),
        2,
        "case {case}: wrapper wrote extra output: {stdout:?}"
    );
    assert!(
        out.stderr.is_empty(),
        "case {case}: wrapper wrote to stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    (rc.unwrap(), pwd.unwrap())
}

#[test]
fn wrapper_cds_after_tm_run_and_passes_the_exit_status_through() {
    let h = harness(ShellArg::Bash);
    let (rc, pwd) = run_case(&h, "good");
    assert_eq!(rc, 7, "the real `tm run` status must survive the wrapper");
    assert_eq!(pwd, h.target.display().to_string());
}

#[test]
fn wrapper_does_not_cd_when_path_resolution_fails() {
    // The failure case that matters most: `tm path` exits non-zero. No `cd`,
    // no diagnostic, and the run's own status still comes back.
    let h = harness(ShellArg::Bash);
    let (rc, pwd) = run_case(&h, "bad");
    assert_eq!(rc, 7);
    assert_eq!(pwd, h.start.display().to_string());
}

#[test]
fn wrapper_does_not_cd_when_path_prints_nothing() {
    // `tm path` succeeding with empty stdout must never become `cd ""`, which
    // in an interactive shell would jump to $HOME.
    let h = harness(ShellArg::Bash);
    let (rc, pwd) = run_case(&h, "empty");
    assert_eq!(rc, 7);
    assert_eq!(pwd, h.start.display().to_string());
}

#[test]
fn wrapper_does_not_cd_when_the_resolved_directory_is_missing() {
    let h = harness(ShellArg::Bash);
    let (rc, pwd) = run_case(&h, "gone");
    assert_eq!(rc, 7);
    assert_eq!(pwd, h.start.display().to_string());
}

#[test]
fn wrapper_ignores_every_subcommand_that_is_not_run() {
    let h = harness(ShellArg::Bash);
    let (rc, pwd) = run_case(&h, "nonrun");
    assert_eq!(rc, 3, "`tm status`'s own status, not `tm run`'s");
    assert_eq!(pwd, h.start.display().to_string());
}

#[test]
fn wrapper_ignores_run_without_an_alias() {
    let h = harness(ShellArg::Bash);
    let (rc, pwd) = run_case(&h, "noalias");
    assert_eq!(rc, 7);
    assert_eq!(pwd, h.start.display().to_string());
}

#[test]
fn wrapper_does_not_mistake_a_task_value_for_the_alias() {
    // `--task` takes a value that can look exactly like an alias.
    let h = harness(ShellArg::Bash);
    let (rc, pwd) = run_case(&h, "task");
    assert_eq!(rc, 7);
    assert_eq!(pwd, h.target.display().to_string());
}

#[test]
fn wrapper_forwards_root_to_tm_path() {
    // The stub resolves `rooted` ONLY when `--root` arrives, so a passing
    // `rooted`/`rootedeq` next to a non-cd'ing `rootless` proves the flag is
    // really forwarded rather than the alias resolving by luck.
    let h = harness(ShellArg::Bash);
    for case in ["rooted", "rootedeq"] {
        let (rc, pwd) = run_case(&h, case);
        assert_eq!(rc, 7, "case {case}");
        assert_eq!(pwd, h.rooted.display().to_string(), "case {case}");
    }
    let (rc, pwd) = run_case(&h, "rootless");
    assert_eq!(rc, 7);
    assert_eq!(pwd, h.start.display().to_string());
}

#[test]
fn posix_snippets_parse_under_bash() {
    // A snippet that passes every Rust assertion above and fails to parse is
    // not shipped work. zsh and fish are parsed the same way outside the suite
    // (`zsh -n`, `fish --no-execute`); neither is guaranteed on a Linux runner.
    let dir = TempDir::new().unwrap();
    for (shell, name) in [(ShellArg::Zsh, "zsh.sh"), (ShellArg::Bash, "bash.sh")] {
        let p = dir.path().join(name);
        fs::write(&p, shell_init_snippet(shell)).unwrap();
        let out = Command::new("bash").arg("-n").arg(&p).output().unwrap();
        assert!(
            out.status.success(),
            "{shell:?} snippet failed `bash -n`: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
