//! #10566: a script run inside a command substitution is judged like one run
//! directly. Every fixture is a fake placeholder; nothing is executed.

use super::*;

use crate::commands::pm_guard_bash::{MAX_SUBSTITUTION_DEPTH, blank_shell_comments};
use crate::commands::pm_guard_secret_script_read::MAX_SCRIPT_BYTES;

/// A Keychain read that prints its value — refused inline (#8596).
const KEYCHAIN_READ: &str = "security find-generic-password -s fake-svc -w\n";

/// Write `body` to `dir/name`.
fn script(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write script");
}

/// 🔴 REGRESSION (#10566): a credential-reading script run from a `$( … )`
/// or backtick body — assigned, quoted, nested, or inside a script — refuses
/// with the reason the plain `bash probe.sh` gets, naming the same script.
#[test]
fn a_script_run_in_a_substitution_is_judged_10566() {
    let dir = tempfile::tempdir().expect("dir");
    script(dir.path(), "probe.sh", KEYCHAIN_READ);
    script(
        dir.path(),
        "outer.sh",
        "T=\"$(bash ./probe.sh)\"\necho \"$T\"\n",
    );
    script(dir.path(), "tick.sh", "T=`bash probe.sh`\necho \"$T\"\n");
    let plain = evaluate_script_body_secret_read("bash probe.sh", dir.path())
        .expect("the plain run refuses");
    for (command, word) in [
        ("T=\"$(bash probe.sh)\"", "probe.sh"),
        ("T=$(bash probe.sh); echo \"$T\"", "probe.sh"),
        ("T=`bash probe.sh`", "probe.sh"),
        ("echo \"$(echo \"$(bash probe.sh)\")\"", "probe.sh"),
        ("T=\"$(sh ./probe.sh)\"", "./probe.sh"),
        ("bash outer.sh", "outer.sh"),
        ("bash tick.sh", "tick.sh"),
    ] {
        let reason = evaluate_script_body_secret_read(command, dir.path()).unwrap_or_default();
        assert!(reason.contains("#8879"), "`{command}` must deny: {reason}");
        assert!(reason.contains(&format!("`{word}`")), "{command}: {reason}");
        assert!(
            reason.contains("runs a credential-reading command"),
            "same rule class as the plain run ({plain}): {reason}"
        );
        assert!(!reason.contains("fake-svc"), "never quotes the body");
    }
}

/// 🔴 REGRESSION (#10566, fail closed): substitutions nested past
/// `MAX_SUBSTITUTION_DEPTH` refuse rather than go unread.
#[test]
fn substitutions_nested_past_the_bound_refuse_10566() {
    let dir = tempfile::tempdir().expect("dir");
    let deep = MAX_SUBSTITUTION_DEPTH + 2;
    let command = format!("echo {}true{}", "$(".repeat(deep), ")".repeat(deep));
    let reason = evaluate_script_body_secret_read(&command, dir.path()).unwrap_or_default();
    assert!(
        reason.contains("nests command substitutions past"),
        "must deny: {reason}"
    );
    let shallow = format!("echo {}true{}", "$(".repeat(4), ")".repeat(4));
    assert_eq!(evaluate_script_body_secret_read(&shallow, dir.path()), None);
}

/// No over-deny (#10566): a substitution running a clean script or no
/// script, a quoted `$(` mention, and a script an earlier stage writes allow.
#[test]
fn a_substitution_with_no_credential_read_allows_10566() {
    let dir = tempfile::tempdir().expect("dir");
    script(dir.path(), "clean.sh", "echo fake-value\n");
    script(dir.path(), "probe.sh", KEYCHAIN_READ);
    // Residual 10: in a body, a substitution whose literal script is missing.
    script(
        dir.path(),
        "prose.sh",
        "echo \"run `./never-written.sh` later\"\n",
    );
    for command in [
        "bash prose.sh",
        "T=\"$(bash clean.sh)\"; echo \"$T\"",
        "echo \"$(date +%s)\" `pwd`",
        "git commit -m \"$(cat msg.txt)\"",
        "echo 'run $(bash probe.sh) later'",
        "printf 'echo hi' > later.sh; T=\"$(bash later.sh)\"",
    ] {
        assert_eq!(
            evaluate_script_body_secret_read(command, dir.path()),
            None,
            "`{command}` must allow"
        );
    }
}

/// 🔴 REGRESSION (#10566): a backtick pair in a trailing `#` comment is not
/// run, so it is not judged; a `#` inside quotes or a word opens no comment.
#[test]
fn a_substitution_in_a_comment_is_not_run_10566() {
    let dir = tempfile::tempdir().expect("dir");
    script(dir.path(), "probe.sh", KEYCHAIN_READ);
    let commented = "echo hi  # see `bash probe.sh` and `//`";
    assert_eq!(
        evaluate_script_body_secret_read(commented, dir.path()),
        None
    );
    for command in [
        "echo \"a # b\" $(bash probe.sh)",
        "echo a#b $(bash probe.sh)",
    ] {
        let reason = evaluate_script_body_secret_read(command, dir.path()).unwrap_or_default();
        assert!(
            reason.contains("`probe.sh`"),
            "`{command}` must deny: {reason}"
        );
    }
}

/// Every row refuses through the script-body rule from `dir`, naming `word`.
fn all_name(dir: &Path, rows: &[&str], word: &str) {
    for command in rows {
        let reason = evaluate_script_body_secret_read(command, dir).unwrap_or_default();
        assert!(
            reason.contains(&format!("`{word}`")),
            "`{command}` must deny naming `{word}`: {reason}"
        );
    }
}

/// 🔴 REGRESSION (#10566 round 2, critic 3): a `#` inside `${ }` opens no
/// comment, so the substitution after it is judged.
#[test]
fn a_hash_inside_a_parameter_expansion_opens_no_comment_10566() {
    let dir = tempfile::tempdir().expect("dir");
    script(dir.path(), "probe.sh", KEYCHAIN_READ);
    all_name(dir.path(), &["echo ${x:- #} $(bash probe.sh)"], "probe.sh");
}

/// 🔴 REGRESSION (#10566 round 2, critic 4): a `#` line inside a multi-line
/// double-quoted string runs its backticks, so it is judged.
#[test]
fn a_backtick_hash_line_in_a_string_is_judged_10566() {
    let dir = tempfile::tempdir().expect("dir");
    script(dir.path(), "probe.sh", KEYCHAIN_READ);
    script(
        dir.path(),
        "ml.sh",
        "echo \"start\n# `bash probe.sh`\nend\"\n",
    );
    all_name(dir.path(), &["bash ml.sh"], "ml.sh");
}

/// 🔴 REGRESSION (#10566 round 2, critic 5): inside a script body, a
/// substitution's script word that is computed, a variable the guard
/// resolved, a `/dev/fd/` pipe or a glob refuses as unresolvable.
#[test]
fn a_computed_script_in_a_body_substitution_refuses_10566() {
    let dir = tempfile::tempdir().expect("dir");
    for (name, body) in [
        ("a.sh", "B=./b.sh\nT=$(bash \"$B\")\n"),
        ("c.sh", "T=$(bash \"$1\")\n"),
        ("d.sh", "T=$(bash /dev/fd/63)\n"),
        ("e.sh", "T=$(bash ./gen-*.sh)\n"),
    ] {
        script(dir.path(), name, body);
        let reason = evaluate_script_body_secret_read(&format!("bash {name}"), dir.path())
            .unwrap_or_default();
        assert!(
            reason.contains("computed at run time"),
            "`bash {name}` must deny as unresolvable: {reason}"
        );
    }
}

/// No over-deny (#10566 round 2): `$(git rev-parse --show-toplevel)`, bare
/// or with a literal `-C`, resolves to the checkout root above the script,
/// so a gate script's `$(bash "$REPO_ROOT/scripts/x.sh")` names a file.
#[test]
fn a_git_toplevel_idiom_resolves_10566() {
    let dir = tempfile::tempdir().expect("dir");
    std::fs::create_dir_all(dir.path().join(".git")).expect("git dir");
    let scripts = dir.path().join("scripts");
    std::fs::create_dir_all(&scripts).expect("scripts dir");
    let root = dir.path().display().to_string();
    let path = scripts.join("gate.sh");
    for body in [
        "R=\"$(git rev-parse --show-toplevel)\"\nT=$(bash \"$R/scripts/x.sh\")\n".to_string(),
        format!("T=$(bash \"$(git -C {root}/scripts rev-parse --show-toplevel)/scripts/x.sh\")\n"),
    ] {
        let resolved = resolve_self_paths(&body, &path);
        assert!(
            resolved.contains(&format!("{root}/scripts/x.sh")),
            "{resolved}"
        );
    }
}

/// 🔴 REGRESSION (#10566 round 2, critic 6): an unquoted here-document
/// expands its substitutions; a quoted one does not.
#[test]
fn an_unquoted_heredoc_substitution_is_judged_10566() {
    let dir = tempfile::tempdir().expect("dir");
    script(dir.path(), "probe.sh", KEYCHAIN_READ);
    all_name(
        dir.path(),
        &["cat <<EOF\n$(bash probe.sh)\nEOF"],
        "probe.sh",
    );
    let quoted = "cat <<'EOF'\n$(bash probe.sh)\nEOF";
    assert_eq!(evaluate_script_body_secret_read(quoted, dir.path()), None);
}

/// 🔴 REGRESSION (#10622): an ANSI-C `$'…'` string holding `\'` ends where
/// the shell ends it, so a `#` inside a later quoted word opens no comment
/// and the substitution after it is judged.
#[test]
fn an_ansi_c_string_keeps_the_comment_scanner_in_step_10622() {
    let line = "echo $'\\'' 'x #y' $(bash probe.sh)";
    assert_eq!(blank_shell_comments(line), line);
    let dir = tempfile::tempdir().expect("dir");
    script(dir.path(), "probe.sh", KEYCHAIN_READ);
    all_name(dir.path(), &[line], "probe.sh");
    // A real comment after an ANSI-C string is still blanked.
    assert_eq!(
        blank_shell_comments("echo $'a\\'b' # `x`"),
        "echo $'a\\'b'      "
    );
}

/// 🔴 REGRESSION (#10622, fail closed): when the comment scan ends inside an
/// open quote, the text after the last closed line start falls back, and the
/// fallback keeps every `#` line that carries command-substitution syntax and
/// blanks a plain one. A comment before that point is still blanked.
#[test]
fn the_comment_fallback_keeps_a_line_that_runs_code_10622() {
    let text = "# head `a`\necho 'open\n# `x`\n# <(y) >(z)\n# plain\n";
    assert_eq!(
        blank_shell_comments(text),
        "          \necho 'open\n# `x`\n# <(y) >(z)\n\n"
    );
    let dir = tempfile::tempdir().expect("dir");
    script(dir.path(), "probe.sh", KEYCHAIN_READ);
    script(
        dir.path(),
        "open.sh",
        "echo \"start\n# `bash probe.sh`\nend\"\necho 'open\n",
    );
    all_name(dir.path(), &["bash open.sh"], "open.sh");
}

/// 🔴 REGRESSION (#10622): a bare `$(git rev-parse --show-toplevel)` after a
/// directory change, and any toplevel idiom after a `GIT_*` override or a
/// redefinition of `git`, stays unresolved; a script running through it
/// refuses as computed. A literal `-C` after a `cd` still resolves.
#[test]
fn a_toplevel_after_a_cwd_or_git_change_stays_unresolved_10622() {
    let dir = tempfile::tempdir().expect("dir");
    std::fs::create_dir_all(dir.path().join(".git")).expect("git dir");
    let scripts = dir.path().join("scripts");
    std::fs::create_dir_all(&scripts).expect("scripts dir");
    let root = dir.path().display().to_string();
    let path = scripts.join("gate.sh");
    let bare = "R=\"$(git rev-parse --show-toplevel)\"\nT=$(bash \"$R/scripts/x.sh\")\n";
    let with_c = format!("T=$(bash \"$(git -C {root} rev-parse --show-toplevel)/scripts/x.sh\")\n");
    let target = format!("{root}/scripts/x.sh");
    for prefix in [
        "cd /fake-elsewhere\n",
        "pushd /fake-elsewhere\n",
        "export GIT_DIR=/fake-elsewhere/.git\n",
        "GIT_WORK_TREE=/fake-elsewhere\n",
        "git() { echo /fake-elsewhere; }\n",
        "function git { echo /fake-elsewhere; }\n",
        "alias git=fake-git\n",
        "PATH=/fake-bin:$PATH\n",
    ] {
        let resolved = resolve_self_paths(&format!("{prefix}{bare}"), &path);
        assert!(!resolved.contains(&target), "{prefix:?}: {resolved}");
    }
    for prefix in [
        "export GIT_DIR=/fake-elsewhere/.git\n",
        "alias git=fake-git\n",
    ] {
        let resolved = resolve_self_paths(&format!("{prefix}{with_c}"), &path);
        assert!(!resolved.contains(&target), "{prefix:?}: {resolved}");
    }
    // No over-deny: a comment naming `cd`, and an absolute `-C` after a `cd`.
    for body in [
        format!("# cd first\n{bare}"),
        format!("cd /fake-elsewhere\n{with_c}"),
    ] {
        assert!(resolve_self_paths(&body, &path).contains(&target), "{body}");
    }
    std::fs::write(scripts.join("x.sh"), "echo fake-value\n").expect("write x.sh");
    std::fs::write(&path, format!("cd /fake-elsewhere\n{bare}")).expect("write gate.sh");
    let reason =
        evaluate_script_body_secret_read("bash scripts/gate.sh", dir.path()).unwrap_or_default();
    assert!(
        reason.contains("computed at run time"),
        "must deny: {reason}"
    );
}

/// 🔴 REGRESSION (#10622 round 2): `$$` is the pid parameter, so a `'` after
/// it opens a plain single-quoted string; a later double-quoted region that
/// holds a `#` and a substitution is not a comment, and is judged.
#[test]
fn a_pid_expansion_before_a_plain_string_keeps_the_scanner_in_step_10622() {
    let line = "echo $$'a\\' \"b ' # $(bash probe.sh)\"";
    assert_eq!(blank_shell_comments(line), line);
    let dir = tempfile::tempdir().expect("dir");
    script(dir.path(), "probe.sh", KEYCHAIN_READ);
    all_name(dir.path(), &[line], "probe.sh");
}

/// A checkout root (a `.git` entry) holding `scripts/`, and the path
/// `{root}/scripts/x.sh` a resolved toplevel idiom names.
fn checkout() -> (tempfile::TempDir, PathBuf, String) {
    let dir = tempfile::tempdir().expect("dir");
    std::fs::create_dir_all(dir.path().join(".git")).expect("git dir");
    let scripts = dir.path().join("scripts");
    std::fs::create_dir_all(&scripts).expect("scripts dir");
    let target = format!("{}/scripts/x.sh", dir.path().display());
    (dir, scripts, target)
}

/// A line running `x.sh` through the bare toplevel idiom.
const RUN_X: &str = "T=\"$(bash \"$(git rev-parse --show-toplevel)/scripts/x.sh\")\"\n";

/// 🔴 REGRESSION (#10622 round 2): a bare toplevel idiom stays unresolved
/// when a directory change can run before it at run time — the idiom in a
/// function or loop with a `cd` anywhere — or when the directory change or
/// the git redefinition is spelled with quotes or backslashes. A `cd` after
/// a top-level idiom still resolves.
#[test]
fn a_toplevel_run_later_or_spelled_in_quotes_stays_unresolved_10622() {
    let (_dir, scripts, target) = checkout();
    let path = scripts.join("gate.sh");
    for body in [
        format!("top() {{\n  {RUN_X}}}\ncd /fake-elsewhere\ntop\n"),
        format!("for d in a b; do\n  {RUN_X}  cd /fake-elsewhere\ndone\n"),
        format!("\"cd\" /fake-elsewhere\n{RUN_X}"),
        format!("\\cd /fake-elsewhere\n{RUN_X}"),
        format!("'pushd' /fake-elsewhere\n{RUN_X}"),
        format!("alias g\\it=fake-git\n{RUN_X}"),
        format!("{RUN_X}alias \"git\"=fake-git\n"),
    ] {
        let resolved = resolve_self_paths(&body, &path);
        assert!(!resolved.contains(&target), "{body:?} must stay unresolved");
    }
    let top_level = format!("{RUN_X}cd /fake-elsewhere\n");
    assert!(resolve_self_paths(&top_level, &path).contains(&target));
}

/// 🔴 REGRESSION (#10622 round 2): a `GIT_*` assignment on the invoking
/// command, or exported by a parent script, reaches the script it runs, so
/// that script's toplevel idiom stays unresolved and the run refuses.
#[test]
fn a_toplevel_inherits_the_invoking_git_environment_10622() {
    let (dir, scripts, _) = checkout();
    std::fs::write(scripts.join("x.sh"), "echo fake-value\n").expect("write x.sh");
    std::fs::write(scripts.join("gate.sh"), RUN_X).expect("write gate.sh");
    script(
        dir.path(),
        "outer.sh",
        "export GIT_DIR=/fake-elsewhere/.git\nbash scripts/gate.sh\n",
    );
    assert_eq!(
        evaluate_script_body_secret_read("bash scripts/gate.sh", dir.path()),
        None
    );
    for command in [
        "GIT_DIR=/fake-elsewhere/.git bash scripts/gate.sh",
        "export GIT_WORK_TREE=/fake-elsewhere; bash scripts/gate.sh",
        "bash outer.sh",
    ] {
        let reason = evaluate_script_body_secret_read(command, dir.path()).unwrap_or_default();
        assert!(
            reason.contains("computed at run time"),
            "`{command}` must deny: {reason}"
        );
    }
}

/// 🔴 REGRESSION (#10622 round 2, bounded time): a body near the script size
/// bound made of toplevel idioms resolves in linear time.
#[test]
fn a_body_full_of_toplevel_idioms_resolves_in_linear_time_10622() {
    let (_dir, scripts, target) = checkout();
    let line = "echo \"$(git rev-parse --show-toplevel)\"\n";
    let lines = MAX_SCRIPT_BYTES as usize / line.len() - 1;
    let body = line.repeat(lines);
    let started = std::time::Instant::now();
    let resolved = resolve_self_paths(&body, &scripts.join("gate.sh"));
    let elapsed = started.elapsed();
    let root = target.trim_end_matches("/scripts/x.sh");
    assert_eq!(
        resolved.matches(root).count(),
        lines,
        "every idiom resolves"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "{lines} idioms took {elapsed:?}"
    );
}
