//! Per-issue regression rows for the shared Bash tokenizer (#7839, #7833,
//! #7479, #7744, #7743, #7738, #7728, #7863).
//!
//! Why: eight issues reported the same root cause in eight spellings — a guard
//! reading shell metacharacters as literal filenames or as executable code.
//! Four of them reproduce here and are fixed by [`super::bash_tokens`]; two
//! were already closed by an earlier round and are PINNED so the fix cannot
//! regress them; two turn out not to originate in this guard at all, and their
//! rows record that finding rather than a change. Keeping one row per issue,
//! each carrying the issue's own reproduction command, is what makes the next
//! reader able to tell which of those three things happened.
//! What: one test per issue named `guard_<issue>_<slug>`, plus the
//! still-refused controls that bound every relaxation and the tokenizer's own
//! fail-closed error arm. The here-document rows at the end name no issue:
//! they pin denials this guard already gives, as the bound on any future
//! attempt to stop reading a quoted body as code (#7190, still open).
//! Test: itself.
//!
//! This file is classified as a test file (3000-SLOC cap) by its
//! `_tests.rs` basename.

use std::path::Path;

use super::bash_tokens::{TokenizeError, tokenize};
use super::{
    evaluate_bash_command, evaluate_destructive_delete_command, evaluate_secret_file_copy_command,
    unclassifiable_command,
};
use crate::commands::pm_guard_secret_read::evaluate_secret_file_read_command;

/// A cwd outside any repository, so the deletion rule's repo-root arm is not
/// what any row below is measuring.
fn cwd() -> &'static Path {
    Path::new("/tmp/pm-guard-tokenizer-probe")
}

/// Every band a Bash command passes through, as one answer.
///
/// What: `None` when no rule refuses. The bands are the ones `pm_guard` calls
/// for a dispatched subagent — the secret-read rule, the secret-copy rule, and
/// the destructive-deletion rule — plus the unclassifiable check ahead of them.
fn refusal(command: &str) -> Option<String> {
    unclassifiable_command(command)
        .map(str::to_string)
        .or_else(|| evaluate_secret_file_read_command(command))
        .or_else(|| evaluate_secret_file_copy_command(command, cwd()))
        .or_else(|| evaluate_destructive_delete_command(command, cwd()).map(str::to_string))
}

/// #7839: a `sed` expression's regex wildcard was read as a dotfile glob.
///
/// Why: the classifier knew `awk`'s program and an interpreter's `-c` value
/// but not `sed`'s expression, so `.*` reached the argv word scan, matched the
/// `.env.*` family through the glob-overlap arm, and refused an edit to an
/// ordinary scratchpad script.
/// What: the issue's shape — an in-place `sed` whose expression carries the
/// dot-star wildcard — must allow, while a `sed` naming a real dotenv operand
/// must still refuse.
/// Test: itself.
#[test]
fn guard_7839_sed_expression_wildcard() {
    for command in [
        "sed -i '' 's/^.*$/replaced/' /tmp/scratch/patch.sh",
        "sed -i 's/^.*$/replaced/' /tmp/scratch/patch.sh",
        "sed -e 's|^.*foo|bar|' -e 's/.*//' /tmp/scratch/patch.sh",
        "sed -n 's/.*version = \"\\(.*\\)\"/\\1/p' Cargo.toml",
    ] {
        assert_eq!(
            evaluate_secret_file_read_command(command),
            None,
            "a sed expression's wildcard names no file: {command}"
        );
    }
    // The bound: the reported #7266 line-range read of a real secret operand.
    assert!(
        evaluate_secret_file_read_command("sed -n '38,46p' .env").is_some(),
        "a sed reading a dotenv operand must still refuse"
    );
    assert!(
        evaluate_secret_file_read_command("sed -f script.sed .env").is_some(),
        "`-f` loads the program from a file, so every positional stays a path"
    );
}

/// #7833: a here-document body's `word:` token was read as a secret-shaped
/// filename.
///
/// Why: PINNED, not fixed here — #7266 round 9 lifted a data here-document's
/// body out of the argv text, which closed this shape before the issue was
/// worked. The row exists so the next reader can see the issue was verified
/// rather than skipped.
/// What: the issue's shape — a heredoc writing a Python script whose body
/// carries an f-string `word:` literal — must allow; a body naming a real
/// dotenv file must still refuse.
/// Test: itself.
#[test]
fn guard_7833_heredoc_body_word_token() {
    for command in [
        "cat > /tmp/scratch/probe.py <<'PY'\nprint(f\"skillCount: {n}\")\nPY",
        "cat > /tmp/scratch/probe.py <<'PY'\nprint(f\"skillCount: {skills}\")\nprint('agentCount:', a)\nPY",
    ] {
        assert_eq!(
            evaluate_secret_file_read_command(command),
            None,
            "a heredoc body's word token names no file: {command}"
        );
    }
    assert!(
        evaluate_secret_file_read_command("cat > /tmp/s/p.py <<'PY'\nopen('.env').read()\nPY")
            .is_some(),
        "a heredoc body naming a real dotenv file must still refuse"
    );
}

/// #7479: the dotenv class refused committed placeholder files.
///
/// Why: PINNED, not fixed here — the `*.example`/`*.sample`/`*.template`
/// exemption already sits in the shared name matcher. The row keeps the three
/// suffixes asserted from the READ surface the issue reported them on, since
/// nothing else in this file exercises them.
/// What: reading a placeholder must allow; the real dotenv files beside it
/// must still refuse.
/// Test: itself.
#[test]
fn guard_7479_dotenv_placeholder_suffix() {
    for command in [
        "cat .env.example",
        "cat config/.env.sample",
        "cat .env.template",
    ] {
        assert_eq!(
            evaluate_secret_file_read_command(command),
            None,
            "a dotenv placeholder is safe to read: {command}"
        );
    }
    for command in ["cat .env", "cat .env.local", "cat .env.production"] {
        assert!(
            evaluate_secret_file_read_command(command).is_some(),
            "a real dotenv file must still refuse: {command}"
        );
    }
}

/// #7744: an `awk` `/regex/{action}` rule's body was read as a filename.
///
/// Why: #7397 narrowed the tokenizer for an accumulator-style awk rule, but a
/// pattern-match rule opening with `/` reached the same word scan, and the
/// bracket-class collapse turned `/.*[Oo]pen/` into a candidate the
/// `.env.*` family overlapped. The refusal quoted the regex fragment as the
/// file name.
/// What: the issue's shape must allow against a real markdown operand, while
/// an awk naming a dotenv operand must still refuse.
/// Test: itself.
#[test]
fn guard_7744_awk_pattern_match_rule() {
    for command in [
        "awk '/.*[Oo]pen/ {print}' docs/specs/domain-service-shell.md",
        "awk '/^.*$/{n++} END{print n}' docs/specs/domain-service-shell.md",
        "awk -F';' '/.*[Cc]losed/{print $2}' docs/specs/domain-service-shell.md",
    ] {
        assert_eq!(
            evaluate_secret_file_read_command(command),
            None,
            "an awk pattern-match rule names no file: {command}"
        );
    }
    assert!(
        evaluate_secret_file_read_command("awk '{print}' .env").is_some(),
        "an awk naming a dotenv operand must still refuse"
    );
}

/// #7743: the `ls`/`stat` exemption did not survive a `2>&1` redirect.
///
/// Why: the argv-side redirect parser read `2>&1` as an attached target — a
/// file named `&1` — so a read-only listing that also named a dotenv path was
/// refused as laundering that file into it. A descriptor duplication opens no
/// file at all, which the sibling byte scanner had known since #5356.
/// What: the issue's shape must allow; a real redirect of a real secret into
/// an ordinary name must still refuse.
/// Test: itself.
#[test]
fn guard_7743_ls_with_a_stderr_redirect() {
    for command in [
        "ls -d .env.local 2>&1",
        "ls -d src docs .env.local 2>&1",
        "stat .env.local 2>&1",
        "ls -d .env.local 2>&- ",
    ] {
        assert_eq!(
            refusal(command),
            None,
            "a descriptor duplication launders nothing: {command}"
        );
    }
    for command in ["cat .env > notes.md", "cat .env &> notes.md"] {
        assert!(
            evaluate_secret_file_copy_command(command, cwd()).is_some(),
            "a real redirect of a secret into an ordinary name must still refuse: {command}"
        );
    }
}

/// #7738: a regex literal inside a `python3 -c` program string was read as a
/// filename.
///
/// Why: the token WAS identified as inline program text, but the program-text
/// scan still read `.*?` through the glob-overlap arm, where a leading `.`
/// plus wildcards reaches the `.env` families. In program text a `.` followed
/// by a quantifier is the regex wildcard, never a dotfile.
/// What: the issue's verbatim shape must allow, while a secret named
/// literally in the same position must still refuse.
/// Test: itself.
#[test]
fn guard_7738_python_inline_regex_literal() {
    for command in [
        r#"python3 -c "import re; m = re.search(r'PASSAGE = \"\"\"(.*?)\"\"\"', s)""#,
        r#"python3 -c 'import re; print(re.findall(r".*?", text))'"#,
        r#"perl -e 'print $1 if /.*?(\d+)/'"#,
    ] {
        assert_eq!(
            evaluate_secret_file_read_command(command),
            None,
            "a regex literal in program text names no file: {command}"
        );
    }
    assert!(
        evaluate_secret_file_read_command(r#"python3 -c "print(open('.env').read())""#).is_some(),
        "a secret named literally inside an inline program must still refuse"
    );
}

/// #7728: a heredoc append whose only redirect target is inside the worktree.
///
/// Why: the refusal this issue reports — "too complex to verify that it stays
/// inside the worktree" — is emitted by the Claude Code binary, not by any
/// `trusty-*` binary, exactly as established for #7477 and #7436 in the
/// sibling `false_positive_tests` module. This guard answers the shape, and
/// answers it ALLOW, so there is no change to make here.
/// What: asserts both bands a dispatched subagent reaches classify the shape,
/// and that the PM classifier's only answer for it is the ordinary shell-write
/// prohibition, which is a different rule with a different owner.
/// Test: itself.
#[test]
fn guard_7728_heredoc_append_inside_a_worktree() {
    let command = "cat >> /Users/x/packages/app-session/src/grant.test.ts <<'TESTS'\n\
                   it('grants once', () => {});\n\
                   TESTS";
    assert_eq!(
        unclassifiable_command(command),
        None,
        "the guard must be able to establish what this runs"
    );
    assert_eq!(evaluate_secret_file_read_command(command), None);
    assert_eq!(evaluate_secret_file_copy_command(command, cwd()), None);
    assert_eq!(evaluate_destructive_delete_command(command, cwd()), None);
}

/// #7863: a three-dot `git diff` range refused while the two-dot form is
/// admitted.
///
/// Why: same finding as #7728 — the refusal comes from the harness, not from
/// here. This guard admits BOTH spellings, so the admission gap the issue
/// describes cannot originate in this repository and no pattern here needs the
/// triple-dot range added to it.
/// What: asserts every band allows both spellings.
/// Test: itself.
#[test]
fn guard_7863_three_dot_git_diff_range() {
    for command in [
        "git diff --stat main...HEAD",
        "git diff --stat main HEAD",
        "git diff origin/main...HEAD -- crates/trusty-mpm",
    ] {
        assert_eq!(
            unclassifiable_command(command),
            None,
            "the guard must be able to establish what this runs: {command}"
        );
        assert_eq!(
            refusal(command),
            None,
            "a read-only diff must allow: {command}"
        );
        assert_eq!(evaluate_bash_command(command), None, "{command}");
    }
}

/// The tokenizer never allows a command because it could not read it.
///
/// Why: the fail-open direction is the one that matters. A guard that treats
/// "the lexer failed" as "there were no tokens" allows exactly the command it
/// could not parse, so the shared tokenizer reports an ERROR and every caller
/// either refuses or falls back to the raw byte scan, which can only add
/// denials.
/// What: asserts the error arm itself, that a refusal names the parse problem
/// rather than a program the guard never resolved, and that an unparseable
/// segment naming a secret or a delete verb still refuses.
/// Test: itself.
#[test]
fn guard_tokenizer_parse_failure_refuses_naming_the_parse_problem() {
    assert_eq!(
        tokenize("awk '{print} terraform.tfvars"),
        Err(TokenizeError::UnbalancedQuoting)
    );
    let reason = evaluate_secret_file_read_command("awk '{print} terraform.tfvars")
        .expect("an unparseable segment naming a secret must refuse");
    assert!(
        reason.contains("unbalanced"),
        "the refusal must name the parse problem, got: {reason}"
    );
    assert!(
        evaluate_destructive_delete_command("rm -rf 'unterminated", cwd()).is_some(),
        "an unparseable segment naming a delete verb must refuse"
    );
    assert!(
        unclassifiable_command("sh -c \"git worktree remove 'unterminated\"").is_some(),
        "an unlexable wrapper must refuse ahead of every rule"
    );
}

/// The relaxations above cost none of the guarantees the guards exist for.
///
/// Why: each row above narrows one reading. The bound on all of them is that a
/// command which genuinely reads a secret, reproduces one under a name the
/// read guard prints, or deletes a protected root is still refused.
/// What: three such commands, one per rule, asserted refused.
/// Test: itself.
#[test]
fn guard_tokenizer_still_refuses_a_real_secret_read_copy_and_delete() {
    assert!(
        evaluate_secret_file_read_command("cat /Users/x/proj/.env").is_some(),
        "reading a dotenv file must still refuse"
    );
    assert!(
        evaluate_secret_file_copy_command("cat .env > notes.md", cwd()).is_some(),
        "laundering a dotenv file into an ordinary name must still refuse"
    );
    assert!(
        evaluate_destructive_delete_command("rm -rf /", cwd()).is_some(),
        "deleting the filesystem root must still refuse"
    );
}

/// The program-text arms bound their own relaxations at the first operand.
///
/// Why: two of the relaxations above are positional, and a positional rule is
/// wrong at its edge or nowhere. The quantifier release must not fire on `.?`,
/// which is a working shell glob rather than a regex quantifier; and the
/// operand skips must not walk PAST a program onto the file beside it, which
/// would exempt the operand the rule exists to screen.
/// What: a bracket-class dotenv glob inside an inline program, an inline
/// program whose text is itself a flag spelling, and an awk program that is
/// the empty string — each with a real dotenv operand behind it.
/// Test: itself.
#[test]
fn guard_tokenizer_bounds_the_program_text_relaxations() {
    assert!(
        evaluate_secret_file_read_command("python3 -c '-e' .env").is_some(),
        "only the first inline-program value is program text; the operand is a path"
    );
    assert!(
        evaluate_secret_file_read_command("awk '' .env").is_some(),
        "an empty awk program is the program; the operand behind it is a path"
    );
    assert!(
        evaluate_secret_file_read_command(r#"python3 -c "print(open('.[e]nv').read())""#).is_some(),
        "a bracket-class dotenv glob is a glob, not a regex quantifier"
    );
}

/// A `<<'WORD'` written inside a quoted string opens no here-document, so it
/// hides no deletion on the lines that follow.
///
/// Why: a proposed relaxation that stops reading a quoted body as code has to
/// find the body first, and a scan without quote state reads `echo "use
/// <<'PY'"` as a real operator — blanking every line up to the next `PY`,
/// including a live `rm -rf /` between them. This guard denies all three rows
/// today; the row exists so a future attempt at #7190 cannot turn one into an
/// allow.
/// What: three shapes, each a fake operator inside a string wrapping a real
/// deletion — a double-quoted outer string, a single-quoted outer string with a
/// `<<"WORD"` delimiter, and the same fake operator behind a genuine quoted
/// body whose apostrophe unbalances the command's quotes.
/// Test: itself.
#[test]
fn guard_a_heredoc_operator_inside_a_string_hides_no_deletion() {
    for command in [
        "echo \"note: use <<'PY' syntax\"\nrm -rf /\nPY",
        "echo 'note: use <<\"PY\" syntax'\nrm -rf /\nPY",
        "python3 - <<'PY'\n# don't\nPY\necho \"use <<'ZZ' here\"\nrm -rf /\nZZ",
    ] {
        assert!(
            evaluate_destructive_delete_command(command, cwd()).is_some(),
            "a `<<'WORD'` inside a string opens no body: {command}"
        );
    }
}

/// A quoted here-document body that a capture hands back to the shell is shell
/// text again, and its deletion is refused.
///
/// Why: a quoted delimiter stops the shell expanding the BODY, but `$( )`, a
/// backtick, `<( )` or `>( )` turns `cat`'s output — the body — back into shell
/// text that `eval`, `source`, `.`, a bare `$x` or any unenumerated consumer
/// runs. This guard refuses every row because the body tokenizes to `rm -rf /`;
/// an attempt at #7190 that blanks the body allows them, which is why the rows
/// are pinned here rather than left to the issue (owner ruling, 2026-09-15).
/// What: one row per re-execution shape — `eval` with the terminator on its own
/// line and glued to `)`, a capture opened on the line before the operator, a
/// backtick capture, a body whose apostrophe unbalances the command's quotes,
/// `source <( )` and `. <( )` in both terminator spellings, `. /dev/stdin <
/// <( )`, word-split execution of a captured variable, a backtick capture run
/// as the command itself, and an output process substitution whose shell is
/// spelled with quotes.
///
/// A DOUBLE-quoted capture (`eval "$(cat <<'PY' … PY)"`) is deliberately absent:
/// the deletion rule reads the whole `"$(…)"` as one argument and allows it,
/// both here and in released `tm` builds. That is its own defect, not a
/// here-document one.
/// Test: itself.
#[test]
fn guard_a_captured_heredoc_body_deletion_still_refuses() {
    let rows = [
        "eval $(cat <<'PY'\nrm -rf /\nPY\n)",
        "eval $(cat <<'PY'\nrm -rf /\nPY)",
        "eval $(\ncat <<'PY'\nrm -rf /\nPY\n)",
        "eval `cat <<'PY'\nrm -rf /\nPY\n`",
        "eval $(cat <<'PY'\n# the agent's step\nrm -rf /\nPY\n)",
        "source <(cat <<'PY'\nrm -rf /\nPY\n)",
        "source <(cat <<'PY'\nrm -rf /\nPY)",
        "source <(cat <<\"PY\"\n# don't\nrm -rf /\nPY\n)",
        ". <(cat <<'PY'\nrm -rf /\nPY\n)",
        ". <(cat <<'PY'\nrm -rf /\nPY)",
        ". /dev/stdin < <(cat <<'PY'\nrm -rf /\nPY\n)",
        "x=$(cat <<'PY'\nrm -rf /\nPY\n)\n$x",
        "`cat <<'PY'\nrm -rf /\nPY\n`",
        "cat <<'PY' > >(\"ba\"sh)\nrm -rf /\nPY",
    ];
    let allowed: Vec<&str> = rows
        .iter()
        .copied()
        .filter(|command| evaluate_destructive_delete_command(command, cwd()).is_none())
        .collect();
    assert!(
        allowed.is_empty(),
        "a captured here-document body is shell text again, so its deletion must refuse; \
         allowed: {allowed:?}"
    );
}
