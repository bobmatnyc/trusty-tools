//! Per-issue regression rows for the shared Bash tokenizer (#7839, #7833,
//! #7479, #7744, #7743, #7738, #7728, #7863, #7190).
//!
//! Why: nine issues reported the same root cause in nine spellings — a guard
//! reading shell metacharacters as literal filenames or as executable code.
//! Five of them reproduce here and are fixed by [`super::bash_tokens`]; two
//! were already closed by an earlier round and are PINNED so the fix cannot
//! regress them; two turn out not to originate in this guard at all, and their
//! rows record that finding rather than a change. Keeping one row per issue,
//! each carrying the issue's own reproduction command, is what makes the next
//! reader able to tell which of those three things happened.
//! What: one test per issue named `guard_<issue>_<slug>`, plus the
//! still-refused controls that bound every relaxation and the tokenizer's own
//! fail-closed error arm.
//! Test: itself.
//!
//! This file is classified as a test file (3000-SLOC cap) by its
//! `_tests.rs` basename.

use std::path::Path;

use super::bash_tokens::{TokenizeError, tokenize};
use super::{
    evaluate_bash_command, evaluate_destructive_delete_command, evaluate_secret_file_copy_command,
    strip_quoted_heredoc_bodies, unclassifiable_command,
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
/// worked. The row exists so the #7190 body-stripping change below cannot
/// reintroduce it from the other direction, and so the next reader can see
/// the issue was verified rather than skipped.
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

/// #7190: the destructive-deletion guard read a quoted here-document's body as
/// executable code.
///
/// Why: a `<<'PY'` body is stdin data — the shell performs no expansion and no
/// substitution on it — so nothing written there ever runs. The guard split it
/// into segments anyway, and a Python line whose tokens happened to include a
/// bare delete verb reached #4031's fail-closed arm, refusing a script that
/// deletes nothing.
/// What: the issue's shape must allow; an UNQUOTED body, which really does
/// substitute, and a body handed to a shell, which really is source, must both
/// still refuse.
/// Test: itself.
#[test]
fn guard_7190_quoted_heredoc_python_body() {
    for command in [
        // The reported shape: a Python body whose apostrophe unbalances the
        // command's quotes, so the body's own lines reached the classifier.
        "python3 - <<'PY'\n# the agent's cleanup step\nverb = \"rm\"\n         pathlib.Path('out.txt').write_text(verb)\nPY",
        "python3 - <<'PY'\n# it isn't a real deletion\nunlink\nPY",
        "cat > /tmp/scratch/clean.py <<\"PY\"\n# don't run this\nrmdir\nPY",
    ] {
        assert_eq!(
            evaluate_destructive_delete_command(command, cwd()),
            None,
            "a quoted here-document body runs nothing: {command}"
        );
    }
    assert!(
        evaluate_destructive_delete_command("bash <<'SH'\nrm -rf /\nSH", cwd()).is_some(),
        "a body handed to a shell IS source and must still refuse"
    );
    assert!(
        evaluate_destructive_delete_command("cat <<EOF\nrm -rf /\nEOF", cwd()).is_some(),
        "an unquoted body is expanded by the shell and must still refuse"
    );
    assert!(
        evaluate_destructive_delete_command("rm -rf /", cwd()).is_some(),
        "the plain deletion must still refuse"
    );
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

/// #7190 review round 2: a quoted string that merely NAMES a here-document
/// operator opens no body, so it hides no deletion.
///
/// Why: the fallback scan #7190 added to recover a body whose own quotes do not
/// balance searched the raw bytes for `<<'WORD'` with no quote state at all,
/// and it ran whenever the primary scan claimed nothing — including the
/// ordinary case where the primary scan read every quote correctly. A `<<'PY'`
/// written inside a double-quoted argument was therefore read as a real
/// operator, and every line up to the next one reading `PY` was blanked before
/// the deletion rule scanned it. `rm -rf /` between the two was allowed.
/// What: three shapes, each a fake operator inside a string wrapping a real
/// deletion — the reported double-quoted outer string, a single-quoted outer
/// string with a `<<"WORD"` delimiter, and the same fake operator behind a
/// genuine quoted body whose apostrophe unbalances the command, which is the
/// one state that still arms the fallback. Each must leave the command
/// byte-identical through the stripper and refuse at the deletion rule.
/// Test: itself.
#[test]
fn guard_7190_a_quoted_operator_opens_no_heredoc_body() {
    for command in [
        // The reported shape.
        "echo \"note: use <<'PY' syntax\"\nrm -rf /\nPY",
        // Outer string single-quoted, fake delimiter double-quoted.
        "echo 'note: use <<\"PY\" syntax'\nrm -rf /\nPY",
        // A real `<<'PY'` body ahead of it leaves the command's quotes
        // unbalanced, so the fallback runs — and must still reject the fake
        // operator that follows.
        "python3 - <<'PY'\n# don't\nPY\necho \"use <<'ZZ' here\"\nrm -rf /\nZZ",
    ] {
        assert!(
            evaluate_destructive_delete_command(command, cwd()).is_some(),
            "a `<<'WORD'` inside a string opens no body: {command}"
        );
    }
    for command in [
        "echo \"note: use <<'PY' syntax\"\nrm -rf /\nPY",
        "echo 'note: use <<\"PY\" syntax'\nrm -rf /\nPY",
    ] {
        assert_eq!(
            strip_quoted_heredoc_bodies(command),
            command,
            "no body was opened, so no byte may be blanked: {command}"
        );
    }
}

/// #7190 review round 2: a here-document scan that ABANDONED claims nothing,
/// rather than handing the command to the weaker fallback.
///
/// Why: [`super::heredoc::HeredocBodies::scan`] gives up on the whole command
/// when a delimiter has no terminator line, and that empty result is
/// indistinguishable from "no here-document here". The fallback used to run on
/// both, so an unparsable command — the one case the guard must fail closed on
/// — got its lines blanked by a scan with no quote state.
/// What: a valid quoted body followed by an unterminated `<<EOF`. The deletion
/// inside the first body is not stripped, so the command refuses.
/// Test: itself.
#[test]
fn guard_7190_an_abandoned_heredoc_scan_claims_no_body() {
    let command = "cat <<'PY'\nrm -rf /\nPY\ncat <<EOF\nno terminator";
    assert_eq!(
        strip_quoted_heredoc_bodies(command),
        command,
        "an abandoned scan may not blank anything"
    );
    assert!(
        evaluate_destructive_delete_command(command, cwd()).is_some(),
        "a command this guard cannot parse must refuse"
    );
}

/// #7190 stays fixed: a genuine quoted body is still stripped, including one
/// opened after a closed quoted argument on the same line.
///
/// Why: the round-2 fix adds quote state to the fallback scan. An operator that
/// follows a quoted word which CLOSES is live syntax, and rejecting it would
/// re-open #7190 for every `cat "some file" <<'PY'`.
/// What: both rows must allow — the first is claimed by the primary scan, the
/// second only by the fallback, since the body's apostrophe unbalances the
/// command's quotes.
/// Test: itself.
#[test]
fn guard_7190_a_body_after_a_closed_quoted_word_is_still_stripped() {
    for command in [
        "cat \"my notes.txt\" <<'PY'\nrm -rf /\nPY",
        "cat \"my notes.txt\" <<'PY'\n# don't delete\nrm -rf /\nPY",
    ] {
        assert!(
            !strip_quoted_heredoc_bodies(command).contains("rm -rf"),
            "a real quoted body must still be blanked: {command}"
        );
        assert_eq!(
            evaluate_destructive_delete_command(command, cwd()),
            None,
            "a quoted here-document body runs nothing: {command}"
        );
    }
}
