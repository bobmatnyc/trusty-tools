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

#[test]
fn evaluate_worktree_add_command_denies_direct_tmp_targets() {
    let cwd = Path::new("/Users/x/proj");
    for cmd in [
        "git worktree add /tmp/wt-x",
        "git worktree add /private/tmp/wt-x",
        "git worktree add /var/folders/x1/abc/T/wt-x",
        "git worktree add -b feat-x /tmp/wt-x",
        "git worktree add --lock --reason busy /tmp/wt-x",
    ] {
        assert_eq!(
            evaluate_worktree_add_command(cmd, cwd),
            Some(WORKTREE_TMP_REASON),
            "expected deny for: {cmd}"
        );
    }
}

#[test]
fn evaluate_worktree_add_command_allows_project_targets() {
    let cwd = Path::new("/Users/x/proj");
    for cmd in [
        "git worktree add .claude/worktrees/wt-x",
        "git worktree add /Users/x/proj/.claude/worktrees/wt-x",
        "git worktree add ../sibling-wt",
        "git worktree add -b feat-x .claude/worktrees/wt-x",
    ] {
        assert_eq!(
            evaluate_worktree_add_command(cmd, cwd),
            None,
            "expected allow for: {cmd}"
        );
    }
}

#[test]
fn evaluate_worktree_add_command_only_matches_add_subcommand() {
    let cwd = Path::new("/Users/x/proj");
    for cmd in [
        "git worktree list",
        "git worktree remove /tmp/wt-x",
        "git worktree prune",
        "git worktree lock /tmp/wt-x",
    ] {
        assert_eq!(
            evaluate_worktree_add_command(cmd, cwd),
            None,
            "non-add worktree subcommand must never be blocked: {cmd}"
        );
    }
}

#[test]
fn evaluate_worktree_add_command_ignores_ordinary_temp_usage() {
    let cwd = Path::new("/Users/x/proj");
    for cmd in [
        "mktemp -d",
        "echo x > /tmp/scratch.txt",
        "cargo build --target-dir /tmp/build",
        "git status",
        "git commit -m 'add worktree support'",
    ] {
        assert_eq!(
            evaluate_worktree_add_command(cmd, cwd),
            None,
            "ordinary temp usage must not be blocked: {cmd}"
        );
    }
}

#[test]
fn evaluate_worktree_add_command_resolves_relative_target_against_cwd() {
    // A relative target under a cwd that is ITSELF under /tmp must resolve
    // (and deny) even though the argument text never spells "/tmp".
    let cwd = Path::new("/tmp/some-repo");
    assert_eq!(
        evaluate_worktree_add_command("git worktree add wt-x", cwd),
        Some(WORKTREE_TMP_REASON)
    );
    // The same relative target from a project cwd is fine.
    let ok_cwd = Path::new("/Users/x/proj");
    assert_eq!(
        evaluate_worktree_add_command("git worktree add wt-x", ok_cwd),
        None
    );
}

#[test]
fn evaluate_worktree_add_command_follows_cd_prefix() {
    // `cd /tmp && git worktree add wt-foo` — the cwd change must be tracked
    // across composition segments (documented partial mitigation).
    let cwd = Path::new("/Users/x/proj");
    assert_eq!(
        evaluate_worktree_add_command("cd /tmp && git worktree add wt-foo", cwd),
        Some(WORKTREE_TMP_REASON)
    );
    assert_eq!(
        evaluate_worktree_add_command("cd .claude/worktrees && git worktree add wt-foo", cwd),
        None
    );
}

#[test]
fn evaluate_worktree_add_command_follows_git_dash_c_override() {
    let cwd = Path::new("/Users/x/proj");
    assert_eq!(
        evaluate_worktree_add_command("git -C /tmp worktree add wt-foo", cwd),
        Some(WORKTREE_TMP_REASON)
    );
    assert_eq!(
        evaluate_worktree_add_command("git -C /tmp worktree add /Users/x/proj/wt-foo", cwd),
        None,
        "an absolute in-project target overrides the -C base"
    );
}

// The env values are INJECTED, never written to the process. An earlier
// revision set `TMPDIR`/`HOME` with `std::env::set_var` behind a restore-on-drop
// guard plus `#[serial]`; neither closes the window that matters, because
// `cargo test` runs tests as threads in ONE process and `#[serial]` only
// serialises against other `#[serial]` tests. Every non-serial sibling still saw
// the mutated `TMPDIR` — and `tempfile` honors it, so five `pm_guard_budget`
// tests panicked with `NotFound` on a Linux CI runner over a macOS-only literal
// path (PR #4914, run 31023632348). `PathEnv` is the seam that removes the
// global write; `#[serial]` is correspondingly gone because there is nothing
// left to serialise.
#[test]
fn evaluate_worktree_add_command_expands_tmpdir_and_home() {
    let cwd = Path::new("/Users/x/proj");
    let env = PathEnv {
        // A harness scratchpad under the `/private/tmp` denylist root — the
        // property under test. Deliberately not any real machine's path.
        tmpdir: Some("/private/tmp/agent-scratch".to_string()),
        tmp: None,
        home: Some("/Users/x".to_string()),
    };
    assert_eq!(
        evaluate_worktree_add_command_in("git worktree add $TMPDIR/wt-foo", cwd, &env),
        Some(WORKTREE_TMP_REASON),
        "$TMPDIR indirection must resolve to the harness scratchpad, still denylisted"
    );
    assert_eq!(
        evaluate_worktree_add_command_in(
            "git worktree add ~/proj/.claude/worktrees/wt-foo",
            cwd,
            &env
        ),
        None,
        "~ expansion to an in-project target must be allowed"
    );
}

/// The public entry point must still expand against the process environment.
///
/// Why: without this, the injected-env test above could stay green while a
/// refactor left [`evaluate_worktree_add_command`] passing an EMPTY `PathEnv`,
/// silently disabling `$TMPDIR`/`~` expansion for the real guard. Asserts
/// equivalence rather than a concrete verdict, so it depends on the delegation
/// and not on what this machine's `$TMPDIR`/`$HOME` happen to be.
#[test]
fn evaluate_worktree_add_command_delegates_to_the_process_environment() {
    let cwd = Path::new("/Users/x/proj");
    let process_env = PathEnv::from_process();
    for cmd in [
        "git worktree add $TMPDIR/wt-foo",
        "git worktree add ${TMPDIR}/wt-foo",
        "git worktree add ~/scratch/wt-foo",
        "git worktree add .claude/worktrees/wt-foo",
    ] {
        assert_eq!(
            evaluate_worktree_add_command(cmd, cwd),
            evaluate_worktree_add_command_in(cmd, cwd, &process_env),
            "the public entry point must expand against the process env: {cmd}"
        );
    }
}

#[test]
fn evaluate_worktree_add_command_normalizes_dot_dot_traversal() {
    // A purely textual `..` escape must be recognized without touching the fs.
    // cwd has exactly 2 name components (Users, proj), so 2 `..` pops back to
    // root before descending into `tmp`.
    let cwd = Path::new("/Users/proj");
    assert_eq!(
        evaluate_worktree_add_command("git worktree add ../../tmp/wt-x", cwd),
        Some(WORKTREE_TMP_REASON)
    );
}

// ── #4837 review BLOCK 1(b): the agent-cost stop's persistence escape hatch ──

#[test]
fn command_is_persistence_only_accepts_commit_and_push() {
    // Exactly the sequence a stopped agent needs to not lose its work.
    for cmd in [
        "git add -A",
        "git commit -m 'fix: something'",
        "git push origin HEAD",
        "git status --short",
        "git diff --stat",
        // Composed, but every segment is still persistence.
        "git add -A && git commit -m x && git push",
        "git status; git diff",
        // A trailing separator leaves an empty tail segment.
        "git status && ",
    ] {
        assert!(
            command_is_persistence_only(cmd),
            "expected {cmd:?} to count as persistence"
        );
    }
}

#[test]
fn command_is_persistence_only_sees_past_git_global_flags() {
    // Agents commit from a worktree with `git -C <path> …` constantly; the
    // escape hatch is useless if it cannot see the subcommand behind that.
    for cmd in [
        "git -C /repo/wt commit -m x",
        "git --git-dir=/repo/.git --work-tree=/repo add -A",
        "git --work-tree /repo -C /repo status",
        "git --no-pager diff --stat",
    ] {
        assert!(
            command_is_persistence_only(cmd),
            "expected {cmd:?} to resolve past its global flags"
        );
    }
}

#[test]
fn command_is_persistence_only_rejects_config_injection() {
    // #4850 review HIGH, shape 1: `-c` sits in `GIT_GLOBAL_OPTS_WITH_ARG`, so
    // the first cut consumed it with its value and resolved a clean `diff` —
    // while `diff.external` runs whatever it names. The earlier suite asserted
    // `git -c user.name=bot commit -m x` WAS persistence, which pinned the hole
    // open by construction: any `-c <anything>` was accepted. `-c` is now
    // rejected outright, so no value has to be judged.
    for cmd in [
        "git -c diff.external='cargo test' diff",
        "git -c core.gitProxy=cargo fetch",
        "git -c credential.helper='!cargo test' push",
        "git -c alias.p='!cargo test' push",
        // The benign spelling goes too — an agent persisting work does not
        // need `-c`, and allow-listing values is the losing side of the bet.
        "git -c user.name=bot commit -m x",
        // Same class, other spellings: config from the environment, and the
        // path git resolves its subcommand binaries from.
        "git --config-env=user.name=EVIL commit -m x",
        "git --exec-path=/tmp/evil status",
        // Default-deny means an option nobody thought about is rejected too.
        "git --totally-made-up-global status",
    ] {
        assert!(
            !command_is_persistence_only(cmd),
            "expected {cmd:?} to be rejected as config injection"
        );
    }
}

#[test]
fn command_is_persistence_only_rejects_exec_options() {
    // #4850 review HIGH, shape 3: these sit AFTER the subcommand, which the
    // first cut never examined — `git_subcommand` had already answered "push"
    // and stopped reading.
    for cmd in [
        "git push --receive-pack='cargo test' /tmp/repo HEAD",
        "git push --receive-pack cargo /tmp/repo HEAD",
        "git push --exec='cargo test' /tmp/repo HEAD",
        "git push --upload-pack=cargo origin HEAD",
        "git diff --ext-diff",
        "git diff --textconv",
        "git diff --output=/tmp/out.txt",
        // Default-deny, not a `-pack` suffix rule: a name nobody has listed is
        // denied because it is absent from SAFE_LONG_OPTS, not because it
        // matched a pattern someone thought of.
        "git push --future-pack=cargo origin HEAD",
        "git push --brand-new-exec-thing=cargo origin HEAD",
    ] {
        assert!(
            !command_is_persistence_only(cmd),
            "expected {cmd:?} to be rejected as an exec-capable option"
        );
    }
}

#[test]
fn command_is_persistence_only_rejects_abbreviated_exec_options() {
    // #4850 second review, HIGH: git's parse-options accepts any unambiguous
    // PREFIX of a long option, so `--exe` IS `--exec` and `--rece` IS
    // `--receive-pack`. The deny list matched names exactly, so all three of
    // these classified as persistence and all three executed the named program
    // against a real bare remote. `--receive-pack` was not even on the list —
    // it was riding the `-pack` SUFFIX rule, which an abbreviation strips off
    // entirely, so no suffix rule could ever have covered it.
    //
    // These pass now because the long-option surface is default-deny: an
    // abbreviation of a dangerous name is not in SAFE_LONG_OPTS. That is the
    // property under test — not the three strings.
    for cmd in [
        "git push --rece=cargo /tmp/r HEAD",
        "git push --exe=cargo /tmp/r HEAD",
        "git push --exe cargo /tmp/r HEAD",
        // The rest of the prefix chain, both families, both spellings.
        "git push --r=cargo /tmp/r HEAD",
        "git push --receiv=cargo /tmp/r HEAD",
        "git push --receive-pac=cargo /tmp/r HEAD",
        "git push --e cargo /tmp/r HEAD",
        "git push --exec cargo /tmp/r HEAD",
        "git push --uploa=cargo origin HEAD",
        "git push --upl cargo origin HEAD",
        // `--exec-path` and the diff-side exec filters abbreviate too.
        "git status --exec-pat=/tmp/evil",
        "git diff --ext",
        "git diff --textc",
        "git diff --outp=/tmp/out.txt",
    ] {
        assert!(
            !command_is_persistence_only(cmd),
            "expected {cmd:?} to be rejected: an abbreviation IS the option"
        );
    }
}

#[test]
fn command_is_persistence_only_accepts_the_flags_agents_actually_use() {
    // The other half of the default-deny flip: it must not strand the work the
    // hatch exists to save. Everything a stopped agent needs to stage, commit,
    // and push still classifies as persistence.
    for cmd in [
        "git add -A --force",
        "git add -- src/lib.rs",
        "git commit -m 'fix: thing' --amend --no-verify",
        "git commit --message='fix: thing' --signoff --allow-empty",
        "git commit -m x --no-gpg-sign --author='Bot <b@example.com>'",
        "git push -u origin HEAD --force-with-lease",
        "git push origin HEAD --tags --follow-tags --atomic",
        "git push origin HEAD --dry-run --porcelain",
        "git status --short --branch --untracked-files=all",
        "git diff --stat --cached --name-only",
        "git diff --unified=5 --ignore-all-space -- crates/",
        "git diff HEAD~1 --no-ext-diff --no-textconv",
    ] {
        assert!(
            command_is_persistence_only(cmd),
            "expected {cmd:?} to survive the default-deny option surface"
        );
    }
}

#[test]
fn command_is_persistence_only_rejects_remote_helper_transport() {
    // #4850 review HIGH, shape 2: `ext::` runs its argument as a shell command
    // by design, and the form generalises to any `<transport>::<address>`
    // remote helper. Rejecting the `::` token closes all of them, and the `-c`
    // that enabled it is independently rejected.
    for cmd in [
        "git -c protocol.ext.allow=always push ext::sh -c 'cargo test' HEAD",
        "git push ext::sh -c 'cargo test' HEAD",
        "git push ext::cargo origin HEAD",
    ] {
        assert!(
            !command_is_persistence_only(cmd),
            "expected {cmd:?} to be rejected as a remote-helper transport"
        );
    }
    // An ordinary remote with a single colon is untouched.
    assert!(command_is_persistence_only("git push origin HEAD:main"));
}

#[test]
fn command_is_persistence_only_admits_the_attacker_chosen_repo_residual() {
    // #4850 second review, LOW 1. The module's residual note used to scope the
    // on-disk exec route to "the repository's own config", which is wrong: `-C`,
    // `--git-dir`, and `--work-tree` are ALLOWED globals, so the repo whose
    // config git reads is attacker-choosable inside the same command. The
    // critic ran `git -C /tmp/evil diff` and it executed a `diff.external` from
    // an attacker-chosen path.
    //
    // This asserts the residual EXISTS, deliberately. Closing it means reading
    // and judging repo config from inside a PreToolUse hook, which is neither
    // fast nor side-effect-free; the note now says so. If someone later closes
    // it, this test fails and the note goes with it.
    assert!(command_is_persistence_only("git -C /tmp/evil diff"));
    assert!(command_is_persistence_only(
        "git --git-dir=/tmp/evil/.git diff"
    ));
}

#[test]
fn command_is_persistence_only_rejects_process_substitution() {
    // #4850 review HIGH, shape 4: `<(` is neither `$(` nor a backtick, and the
    // redirection scan only ever looked at `>`, so this ran `cargo test` inside
    // an "allowed" git call. Any unquoted metacharacter now disqualifies, so
    // there is no spelling left to miss.
    for cmd in [
        "git diff <(cargo test)",
        "git diff >(cargo test)",
        "git add -A < /tmp/list",
        "git commit -m x -F <(cargo test)",
        "git status (cargo test)",
    ] {
        assert!(
            !command_is_persistence_only(cmd),
            "expected {cmd:?} to be rejected as process substitution"
        );
    }
}

#[test]
fn command_is_persistence_only_allows_metacharacters_inside_quotes() {
    // The metacharacter rule has to stay quote-aware or it strands exactly the
    // work the hatch exists to save — commit messages are full of these.
    for cmd in [
        "git commit -m 'fix: handle (n>1) and $HOME'",
        "git commit -m \"refactor: spec -> code\"",
        "git commit -m 'closes #4850 (review)'",
    ] {
        assert!(
            command_is_persistence_only(cmd),
            "expected {cmd:?} to survive its quoted metacharacters"
        );
    }
}

#[test]
fn command_is_persistence_only_requires_a_bare_git_program() {
    // An env-assignment prefix is an exec vector of its own
    // (`GIT_SSH_COMMAND`, `GIT_EXTERNAL_DIFF`, `GIT_PAGER`, `LD_PRELOAD`), and
    // `sudo`/`env` re-exec. None is needed to persist, so the program token
    // must be git itself.
    for cmd in [
        "GIT_SSH_COMMAND='cargo test' git push",
        "GIT_EXTERNAL_DIFF=cargo git diff",
        "env GIT_PAGER=cargo git diff",
        "sudo git commit -m x",
        // #4850 second review, MEDIUM: the check was `program.rsplit('/')`, a
        // BASENAME test, so every one of these answered "git" and the critic
        // executed a planted script by that name. Exposure needs a pre-existing
        // write + chmod +x, but the module doc claimed the program was
        // verified, and a basename verifies the file's name, not the program.
        "./git commit -m x",
        "/tmp/evil/git push origin HEAD",
        "../../tmp/evil/git status",
        "$HOME/evil/git add -A",
        // The whole token must be `git`, so a path-qualified real git goes too.
        // That is the trade: no fs resolution in a PreToolUse hook, and a bare
        // `git` is always available as the fallback spelling.
        "/usr/bin/git commit -m x",
    ] {
        assert!(
            !command_is_persistence_only(cmd),
            "expected {cmd:?} to be rejected: the program must be git itself"
        );
    }
    // The one spelling that survives.
    assert!(command_is_persistence_only("git commit -m x"));
}

#[test]
fn command_is_persistence_only_rejects_smuggled_work() {
    // The whole risk of an allowlist: one allowed verb dragging real work in
    // behind it. Every one of these must fail the WHOLE command.
    for cmd in [
        "git commit -m x && cargo test",
        "cargo test && git commit -m x",
        "git commit -m x | tee log",
        "git commit -m x; rm -rf /",
        // Not persistence: these mutate the tree or fetch work.
        "git checkout main",
        "git reset --hard",
        "git worktree add /tmp/x",
        "git rebase -i HEAD~3",
        // Not git at all.
        "echo hi",
        "",
        "   ",
        // `git` reached through sudo: the program token is not git, and a
        // program that re-execs is not persistence.
        "sudo -u bob git commit -m x",
    ] {
        assert!(
            !command_is_persistence_only(cmd),
            "expected {cmd:?} to be rejected as non-persistence"
        );
    }
}

#[test]
fn command_is_persistence_only_rejects_substitution_and_redirection() {
    // A substitution runs arbitrary code inside an "allowed" segment, and a
    // redirection writes arbitrary files. Neither is needed to commit.
    for cmd in [
        "git commit -m \"$(cargo build 2>&1)\"",
        "git commit -m `date`",
        "git status > /tmp/out.txt",
        "git diff >> notes.md",
        // #4850: a discard sink used to be allowed here, on the strength of
        // `has_file_write_redirection` telling a `/dev/null` write from a real
        // one. That classifier is no longer in the trust path — keeping it
        // meant keeping a hand-rolled redirection parser that had already
        // missed `<`. Every unquoted redirection metacharacter now
        // disqualifies, and an agent saving its work never needs one.
        "git status 2>/dev/null",
    ] {
        assert!(
            !command_is_persistence_only(cmd),
            "expected {cmd:?} to be rejected"
        );
    }
}
