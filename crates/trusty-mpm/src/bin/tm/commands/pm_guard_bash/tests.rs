//! Unit tests for the PM Bash-command classifier (`super`), relocated out of
//! `mod.rs` (issue #2734) so the production file stays under the 500-SLOC cap.
//! `tests.rs` is classified as a test file (1500-SLOC cap).

use super::*;

#[test]
fn evaluate_bash_command_denies_shell_edit_verbs() {
    for cmd in [
        "sed -i s/a/b/ src/lib.rs",
        "sed -i.bak s/a/b/ src/lib.rs",
        "awk -i inplace '{print}' f",
        "patch -p1 < d.patch",
        "git apply my.patch",
        "FOO=1 sed -i s/a/b/ f",
        "sudo patch -p0 x",
    ] {
        assert_eq!(
            evaluate_bash_command(cmd),
            Some(SHELL_EDIT_REASON),
            "expected shell-edit deny for: {cmd}"
        );
    }
}

#[test]
fn evaluate_bash_command_allows_readonly_sed_awk() {
    // #2664: sed/awk used as narrowly read-only stream filters (no
    // in-place flag, no external script, no write/exec construct,
    // balanced quotes) must not be blanket-denied by name.
    for cmd in [
        "git status --porcelain | awk '{print $1}' | sort | uniq -c",
        "sed -n '1,5p' file.txt",
        "git log | awk '{print $1}'",
        "cat f | sed 's/a/b/'",
        "sed 's/a/b/g' f | grep x",
        // Code-critic re-review MEDIUM: an apostrophe/single-quote inside
        // a double-quoted read-only script must not be over-denied.
        r#"sed "s/can't/cannot/" f"#,
        r#"awk -F"'" '{print $2}'"#,
    ] {
        assert_eq!(
            evaluate_bash_command(cmd),
            None,
            "expected allow for read-only sed/awk: {cmd:?}"
        );
    }
}

#[test]
fn evaluate_bash_command_denies_awk_shell_escape_holes() {
    // Code-critic BLOCK on PR #2677: awk's `system()` builtin runs an
    // arbitrary shell command with no `-i` and no `>` — the
    // allow-by-default design missed this entirely.
    for cmd in [
        r#"awk 'BEGIN{system("touch /tmp/x")}'"#,
        r#"awk 'BEGIN{system("curl https://evil/x")}'"#,
        // Code-critic re-review CRITICAL: AWK tolerates whitespace
        // between a builtin name and its `(` — `system ("...")` (space)
        // executes exactly like `system("...")` and was missed by a
        // no-space-only substring check.
        r#"awk 'BEGIN{system ("touch /tmp/x")}'"#,
        "awk 'BEGIN{system\t(\"echo x\")}'",
        // A co-process pipe/read inside the awk program is itself a
        // bare `|`/`;` from the (quote-unaware) segment splitter's point
        // of view, so it fragments the quoted program and leaves an
        // unbalanced quote count in the resulting segment — denied.
        r#"awk 'BEGIN{print | "sort"}'"#,
        r#"awk 'BEGIN{"date" | getline}'"#,
    ] {
        assert_eq!(
            evaluate_bash_command(cmd),
            Some(SHELL_EDIT_REASON),
            "expected shell-edit deny for awk shell-escape: {cmd}"
        );
    }
}

#[test]
fn evaluate_bash_command_denies_sed_e_w_commands() {
    // sed's `e` (execute) and `w`/`W` (write-to-file) commands need no
    // `-i` and no `>` — a bare verb-name deny-list miss, closed by
    // deny-by-default + script-content analysis in `sed_awk`.
    for cmd in [
        "sed '1e touch /tmp/x' f",
        "sed -n '1,5w /tmp/out' f",
        "sed 's/a/b/e' f",
        "sed 's/a/b/w /tmp/out' f",
    ] {
        assert_eq!(
            evaluate_bash_command(cmd),
            Some(SHELL_EDIT_REASON),
            "expected shell-edit deny for sed e/w command: {cmd}"
        );
    }
}

#[test]
fn evaluate_bash_command_denies_external_script_load() {
    // `-f`/`--file` loads a script the guard cannot see — it may contain
    // any of the w/e/system holes above, so deny unconditionally.
    for cmd in ["sed -f script.sed f", "awk -f script.awk f"] {
        assert_eq!(
            evaluate_bash_command(cmd),
            Some(SHELL_EDIT_REASON),
            "expected shell-edit deny for external script load: {cmd}"
        );
    }
}

#[test]
fn evaluate_bash_command_denies_sed_in_place_abbreviations() {
    // GNU getopt long-option abbreviations of `--in-place` must still
    // deny, not just the exact spelling.
    for cmd in [
        "sed --in s/a/b/ f",
        "sed --in-p s/a/b/ f",
        "sed --in=bak s/a/b/ f",
    ] {
        assert_eq!(
            evaluate_bash_command(cmd),
            Some(SHELL_EDIT_REASON),
            "expected shell-edit deny for --in-place abbreviation: {cmd}"
        );
    }
}

#[test]
fn evaluate_bash_command_denies_awk_family_verbs() {
    // gawk/nawk/mawk are the same in-place-edit-capable interpreter
    // family as awk and must be classified identically.
    assert_eq!(
        evaluate_bash_command("gawk -i inplace '{print}' f"),
        Some(SHELL_EDIT_REASON)
    );
    assert_eq!(
        evaluate_bash_command(r#"nawk 'BEGIN{system("id")}'"#),
        Some(SHELL_EDIT_REASON)
    );
    assert_eq!(
        evaluate_bash_command("gawk '{print $1}'"),
        None,
        "read-only gawk must still allow"
    );
}

#[test]
fn evaluate_bash_command_denies_redirection_write() {
    assert_eq!(
        evaluate_bash_command("echo 'code' > src/lib.rs"),
        Some(SHELL_EDIT_REASON)
    );
    assert_eq!(
        evaluate_bash_command("printf x >> notes.txt"),
        Some(SHELL_EDIT_REASON)
    );
}

#[test]
fn evaluate_bash_command_denies_build_and_test() {
    for cmd in ["make build", "pytest -q", "npm test"] {
        assert_eq!(
            evaluate_bash_command(cmd),
            Some(BUILD_TEST_REASON),
            "expected build/test deny for: {cmd}"
        );
    }
}

#[test]
fn evaluate_bash_command_denies_network() {
    for cmd in ["curl https://example.com", "wget https://example.com/x"] {
        assert_eq!(
            evaluate_bash_command(cmd),
            Some(NETWORK_REASON),
            "expected network deny for: {cmd}"
        );
    }
}

#[test]
fn evaluate_bash_command_allows_normal_shell() {
    for cmd in [
        "",
        "   ",
        "git status",
        "git add -A",
        "git commit -m x",
        "git log --oneline",
        "git diff",
        "git push",
        "ls -la",
        "grep -rn foo src",
        "cat README.md",
        "cargo tree 2>&1", // fd-dup, not a file write
    ] {
        assert_eq!(
            evaluate_bash_command(cmd),
            None,
            "expected allow for: {cmd:?}"
        );
    }
}

#[test]
fn evaluate_bash_command_denies_composition_hidden_verbs() {
    // A benign leading verb must NOT hide a forbidden verb in a later
    // composed segment (the shell-composition bypass, PR #1985).
    assert_eq!(
        evaluate_bash_command("cd repo && sed -i s/a/b/ f"),
        Some(SHELL_EDIT_REASON)
    );
    assert_eq!(
        evaluate_bash_command("true; make build"),
        Some(BUILD_TEST_REASON)
    );
    assert_eq!(
        evaluate_bash_command("x || pytest"),
        Some(BUILD_TEST_REASON)
    );
    assert_eq!(
        evaluate_bash_command("ls && git apply p.diff"),
        Some(SHELL_EDIT_REASON)
    );
    // Redirection hidden after a benign first segment still denies.
    assert_eq!(
        evaluate_bash_command("cd repo && echo x > out.txt"),
        Some(SHELL_EDIT_REASON)
    );
}

#[test]
fn evaluate_bash_command_denies_bare_ampersand_composition() {
    // A bare `&` (background separator) must NOT hide a forbidden trailing
    // verb — the bare-`&` composition bypass (second adversarial review).
    assert_eq!(
        evaluate_bash_command("true & sed -i s/a/b/ f"),
        Some(SHELL_EDIT_REASON)
    );
    assert_eq!(
        evaluate_bash_command("cd x & make build"),
        Some(BUILD_TEST_REASON)
    );
    assert_eq!(evaluate_bash_command("x & pytest"), Some(BUILD_TEST_REASON));
    // Newline is a separator too.
    assert_eq!(
        evaluate_bash_command("cd repo\nsed -i s/a/b/ f"),
        Some(SHELL_EDIT_REASON)
    );
}

#[test]
fn evaluate_bash_command_allows_trailing_background() {
    // Trailing background `&` with nothing after must not become a spurious
    // forbidden segment, and fd-dups must not be treated as separators.
    for cmd in ["sleep 1 &", "cargo tree 2>&1", "foo >&2"] {
        assert_eq!(
            evaluate_bash_command(cmd),
            None,
            "expected allow for: {cmd:?}"
        );
    }
}

#[test]
fn evaluate_bash_command_allows_benign_pipes() {
    // Composition where NO segment is a forbidden verb must still allow —
    // we do not blanket-deny all composition.
    for cmd in [
        "git log | head",
        "cat f | grep x",
        "ls | wc -l",
        "git diff && git status",
        "cd repo; ls -la",
    ] {
        assert_eq!(
            evaluate_bash_command(cmd),
            None,
            "expected allow for benign composition: {cmd:?}"
        );
    }
}

#[test]
fn evaluate_bash_command_allows_dev_null_redirect() {
    // `/dev/null` is an output-discard sink, not a file write.
    for cmd in ["which cargo 2>/dev/null", "command -v foo >/dev/null"] {
        assert_eq!(
            evaluate_bash_command(cmd),
            None,
            "expected allow for /dev/null discard: {cmd:?}"
        );
    }
}

// ---- #2745 false-positive instance: 2>/dev/null on a read-only multi-line,
// multi-segment command (Bob's live hotstats-worktree repro, 2026-07-17) ----

#[test]
fn evaluate_bash_command_allows_bob_hotstats_dev_null_repro() {
    // Bob's live repro: a `cd` + `echo` + `ls … 2>/dev/null` composed over a
    // newline and `&&` was denied by an older deployed pm_guard binary. The
    // current classifier already treats `2>/dev/null` as a discard sink, not
    // a file write, across the whole (multi-segment, multi-line) command —
    // this locks that behaviour in as a permanent regression test.
    let cmd = "cd /Users/masa/trusty-mpm-projects/bobmatnyc/trusty-tools/.base/.claude/worktrees/tm-hotstats-product-poc-01\necho \"=== docs tree ===\" && ls docs docs/provenance 2>/dev/null";
    assert_eq!(
        evaluate_bash_command(cmd),
        None,
        "read-only cd/echo/ls with 2>/dev/null must be allowed"
    );
}

#[test]
fn evaluate_bash_command_allows_more_dev_null_and_fd_dup_shapes() {
    // The task's explicit list of read-only redirection shapes that must
    // never trip the edit heuristic.
    for cmd in [
        "ls docs 2>/dev/null",
        "cargo check 2>&1",
        "which rustc >/dev/null",
        "which rustc &>/dev/null",
        "ls a b 2>/dev/null && echo done",
    ] {
        assert_eq!(
            evaluate_bash_command(cmd),
            None,
            "expected allow for read-only redirection shape: {cmd:?}"
        );
    }
}

// ---- #2918: content-aware delegation routing hint extraction -------------

#[test]
fn extract_shell_edit_target_from_redirection() {
    assert_eq!(
        extract_shell_edit_target("echo 'code' > src/lib.rs"),
        Some("src/lib.rs".to_string())
    );
    assert_eq!(
        extract_shell_edit_target("printf x >> docs/notes.md"),
        Some("docs/notes.md".to_string())
    );
}

#[test]
fn extract_shell_edit_target_from_trailing_sed_awk_patch_arg() {
    assert_eq!(
        extract_shell_edit_target("sed -i s/a/b/ app/main.py"),
        Some("app/main.py".to_string())
    );
    assert_eq!(
        extract_shell_edit_target("patch -p1 web/App.tsx"),
        Some("web/App.tsx".to_string())
    );
    assert_eq!(
        extract_shell_edit_target("git apply my.patch"),
        Some("my.patch".to_string())
    );
}

#[test]
fn extract_shell_edit_target_none_when_unresolvable() {
    // A verb with no plausible trailing target (not sed/awk/patch/git-apply)
    // yields no hint — caller falls back to generic.
    assert_eq!(extract_shell_edit_target("git status"), None);
    // A trailing flag with nothing after it yields no hint either.
    assert_eq!(extract_shell_edit_target("sed -i"), None);
}

#[test]
fn extract_shell_edit_target_best_effort_on_script_only_sed() {
    // `sed -n '1,5p'` has no target file — the trailing-token heuristic is
    // best-effort only (it never affects allow/deny, only the suggested
    // delegate name) and may pick up the script text itself here.
    assert_eq!(
        extract_shell_edit_target("sed -n '1,5p'"),
        Some("'1,5p'".to_string())
    );
}

#[test]
fn extract_shell_edit_target_ignores_dev_null_and_fd_dup() {
    // These are not file-write redirects, so no target — falls through to
    // the trailing-token heuristic (which also yields nothing here).
    assert_eq!(extract_shell_edit_target("ls docs 2>/dev/null"), None);
    assert_eq!(extract_shell_edit_target("cargo check 2>&1"), None);
}

#[test]
fn evaluate_bash_command_denies_hidden_substitution_verb() {
    // A forbidden verb inside a command substitution must be caught.
    assert_eq!(
        evaluate_bash_command("echo \"$(sed -i s/a/b/ f)\""),
        Some(SHELL_EDIT_REASON)
    );
    assert_eq!(
        evaluate_bash_command("x=`make build`"),
        Some(BUILD_TEST_REASON)
    );
}

#[test]
fn evaluate_bash_command_allows_benign_substitution() {
    // Trivial substitutions with benign bodies must not be over-blocked.
    for cmd in [
        "echo \"$(date)\"",
        "cd \"$(git rev-parse --show-toplevel)\"",
    ] {
        assert_eq!(
            evaluate_bash_command(cmd),
            None,
            "expected allow for benign substitution: {cmd:?}"
        );
    }
}

#[test]
fn evaluate_bash_command_denies_unbalanced_substitution() {
    // An unbalanced substitution we cannot decompose denies conservatively.
    assert_eq!(
        evaluate_bash_command("echo $(sed -i"),
        Some(SHELL_EDIT_REASON)
    );
    assert_eq!(evaluate_bash_command("echo `foo"), Some(SHELL_EDIT_REASON));
}

#[test]
fn evaluate_bash_command_bounds_deep_substitution_nesting() {
    // Adversarial deep `$(` nesting must return a decision without panicking
    // (stack-exhaustion guard). A deny is the acceptable safe outcome.
    let deep = format!("echo {}", "$(".repeat(200));
    let decision = evaluate_bash_command(&deep);
    assert!(decision.is_some(), "deep nesting must deny, got allow");
    // Balanced-but-deep nesting is also bounded (no panic, returns a value).
    let balanced = format!("echo {}date{}", "$(".repeat(200), ")".repeat(200));
    let _ = evaluate_bash_command(&balanced);
}

#[test]
fn split_shell_segments_splits_operators() {
    assert_eq!(
        split_shell_segments("a && b || c ; d | e"),
        vec!["a ", " b ", " c ", " d ", " e"]
    );
}

#[test]
fn split_shell_segments_splits_bare_ampersand() {
    // A bare `&` with a following command splits; fd-dups and trailing
    // background do not.
    assert_eq!(split_shell_segments("a & b"), vec!["a ", " b"]);
    assert_eq!(split_shell_segments("foo &"), vec!["foo &"]);
    assert_eq!(
        split_shell_segments("cargo test 2>&1"),
        vec!["cargo test 2>&1"]
    );
    assert_eq!(split_shell_segments("foo >&2"), vec!["foo >&2"]);
    // Newline splits.
    assert_eq!(split_shell_segments("a\nb"), vec!["a", "b"]);
}

#[test]
fn split_shell_segments_single_command() {
    assert_eq!(split_shell_segments("git status"), vec!["git status"]);
    // Backgrounding `&` (nothing after) is not a split point.
    assert_eq!(split_shell_segments("foo &"), vec!["foo &"]);
}

#[test]
fn has_file_write_redirection_detects_write() {
    assert!(has_file_write_redirection("echo x > f"));
    assert!(has_file_write_redirection("echo x >f.rs"));
}

#[test]
fn has_file_write_redirection_detects_append() {
    assert!(has_file_write_redirection("echo x >> f"));
}

#[test]
fn has_file_write_redirection_ignores_fd_dup() {
    assert!(!has_file_write_redirection("cargo test 2>&1"));
    assert!(!has_file_write_redirection("foo >&2"));
}

#[test]
fn has_file_write_redirection_ignores_dev_null() {
    // Discarding output to /dev/null is not a file write.
    assert!(!has_file_write_redirection("which cargo 2>/dev/null"));
    assert!(!has_file_write_redirection("command -v foo >/dev/null"));
    assert!(!has_file_write_redirection("foo &>/dev/null"));
    // A real file write with the same shape still denies.
    assert!(has_file_write_redirection("echo x > /dev/null.txt"));
    assert!(has_file_write_redirection("echo x > out.txt"));
}

#[test]
fn has_file_write_redirection_false_for_plain_command() {
    assert!(!has_file_write_redirection("git status"));
}

// ---- #2734: git global flags + quoted-content false positives -----------

#[test]
fn evaluate_bash_command_allows_bob_git_commit_repro() {
    // Bob's live repro: an allowlisted `git commit` denied as a shell edit
    // because a `>` / `->` inside the single-quoted `-m` body tripped the
    // (then quote-unaware) file-write-redirection scan, compounded by the
    // leading `-C <path>` global flag hiding the `commit` subcommand.
    let cmd = r#"git -C /Users/masa/trusty-mpm-projects/bobmatnyc/writing commit -q -m 'feat(hyperdev): draft "SDD: When The Spec Becomes Part Of The Code"' -m 'HyperDev deep-dive: the spec IS the code — SDD flips the pipeline so spec > impl'"#;
    assert_eq!(
        evaluate_bash_command(cmd),
        None,
        "Bob's git commit with quoted prose must be allowed"
    );
}

#[test]
fn evaluate_bash_command_allows_git_commit_with_quoted_operators() {
    // A `-m` message may legitimately contain any shell metacharacter as prose;
    // none of it is shell syntax, so the commit stays allowed.
    for cmd in [
        r#"git commit -m 'spec -> code'"#,
        r#"git commit -m 'a > b'"#,
        r#"git commit -m 'pipe a | b then c'"#,
        r#"git commit -m 'run make && test'"#,
        r#"git commit -m 'costs $(x) or `y`'"#,
        r#"git commit -m 'use sed -i to fix; then awk'"#,
        r#"git commit -m "double > quoted -> ok""#,
    ] {
        assert_eq!(
            evaluate_bash_command(cmd),
            None,
            "quoted prose must not trip a deny: {cmd}"
        );
    }
}

#[test]
fn evaluate_bash_command_allows_git_global_flags() {
    // Allowlisted git subcommands must be recognised through leading global
    // flags (`-C <path>`, `-c <kv>`, `--git-dir=…`).
    for cmd in [
        "git -C /some/path status",
        "git -C /some/path log --oneline",
        "git -C /some/path diff",
        "git -C /some/path commit -m x",
        "git -c user.name=x -c user.email=y@z commit -m x",
        "git --git-dir=/p/.git --work-tree=/p status",
        "git -C /some/path push --force",
    ] {
        assert_eq!(
            evaluate_bash_command(cmd),
            None,
            "git through global flags must be allowed: {cmd}"
        );
    }
}

#[test]
fn evaluate_bash_command_denies_git_apply_through_global_flags() {
    // The one forbidden git subcommand must still be caught through global
    // flags — closing the `git -C <path> apply` under-deny hole.
    for cmd in [
        "git apply my.patch",
        "git -C /some/path apply my.patch",
        "git -c core.pager=x apply my.patch",
        "git --git-dir=/p/.git apply my.patch",
    ] {
        assert_eq!(
            evaluate_bash_command(cmd),
            Some(SHELL_EDIT_REASON),
            "git apply must deny through global flags: {cmd}"
        );
    }
}

#[test]
fn evaluate_bash_command_preserves_true_positives_after_quote_awareness() {
    // Quote-awareness must NOT weaken real shell-level edits/pipes/redirects.
    let shell_edit = [
        "sed -i s/a/b/ src/lib.rs",
        "echo 'code' > src/lib.rs",
        "printf x >> src/lib.rs",
        r#"awk '{print}' > out.rs"#,      // unquoted redirect
        r#"awk 'BEGIN{print | "sort"}'"#, // quoted co-process
        r#"awk '{print > "out.rs"}'"#,    // in-program redirect
        "git apply p.diff",
        "cat f | sed -i s/a/b/ g", // forbidden verb in a real pipe
    ];
    for cmd in shell_edit {
        assert_eq!(
            evaluate_bash_command(cmd),
            Some(SHELL_EDIT_REASON),
            "must still deny shell edit: {cmd}"
        );
    }
    // #2664 read-only sed/awk carve-out still ALLOWS.
    for cmd in [
        "git status --porcelain | awk '{print $1}' | sort | uniq -c",
        "sed -n '1,5p' file.txt",
        "cat f | sed 's/a/b/'",
        r#"sed "s/can't/cannot/" f"#,
    ] {
        assert_eq!(
            evaluate_bash_command(cmd),
            None,
            "read-only sed/awk must still allow: {cmd}"
        );
    }
}

#[test]
fn split_shell_segments_ignores_quoted_operators() {
    // Operators inside quotes are literal data, not separators.
    assert_eq!(
        split_shell_segments("git commit -m 'a | b'"),
        vec!["git commit -m 'a | b'"]
    );
    assert_eq!(
        split_shell_segments("echo 'x && y ; z'"),
        vec!["echo 'x && y ; z'"]
    );
    // Unquoted operators still split.
    assert_eq!(split_shell_segments("a | b"), vec!["a ", " b"]);
    // Unbalanced quotes fall back to the quote-unaware split.
    assert_eq!(
        split_shell_segments("echo 'unterminated | b"),
        vec!["echo 'unterminated ", " b"]
    );
}

#[test]
fn has_file_write_redirection_ignores_quoted_gt() {
    // A `>` inside quotes is literal; an unquoted one is a real redirect.
    assert!(!has_file_write_redirection("git commit -m 'spec > code'"));
    assert!(!has_file_write_redirection(r#"echo "a -> b""#));
    assert!(has_file_write_redirection("echo x > f.rs"));
    // Unbalanced quotes fall back to scanning all bytes.
    assert!(has_file_write_redirection("echo 'oops > f.rs"));
}

#[test]
fn evaluate_bash_command_allows_quoted_substitution_prose() {
    // `$(`/backtick inside SINGLE quotes is literal; inside DOUBLE quotes it is
    // still live and a forbidden body still denies.
    assert_eq!(
        evaluate_bash_command(r#"git commit -m 'run $(sed -i x) maybe'"#),
        None,
        "single-quoted substitution is literal prose"
    );
    assert_eq!(
        evaluate_bash_command(r#"echo "$(sed -i s/a/b/ f)""#),
        Some(SHELL_EDIT_REASON),
        "double-quoted substitution is still live"
    );
}
