//! #9344: a data reader whose operator line injects config or names an alias
//! loses its trust, and the rm-root floor reads every data-reader body for a
//! destructive-root command whatever the reader. Rows of the floor, the
//! forbidden-verb guard and the unclassifiable refusal, beside [`super`]'s
//! operator-line predicates.

use std::path::Path;

use super::super::destructive_delete::{DeleteTarget, evaluate_destructive_delete_command};
use super::super::{evaluate_bash_command, unclassifiable_command};
use super::*;

/// The floor's verdict, judged from a directory that is no repository.
fn floor(command: &str) -> Option<DeleteTarget> {
    evaluate_destructive_delete_command(command, Path::new("/nonexistent-9344"))
}

/// Bodies a shell runs that only a whole-command reading denies: a `cd` the
/// delete then uses, and a substitution a quoted body holds as text.
const PAYLOADS: &[&str] = &["cd ~; rm -rf .", "echo $(rm -rf ~)", "rm -rf ~"];

/// Operator lines whose git or gh hands its stdin to a shell: a `!`-alias
/// made by `-c`, `--config`, `--config-env` or a `GIT_CONFIG_*` prefix, an
/// alias key on the line, and a subcommand no builtin names (an alias or an
/// extension made earlier). #9344 round 2: a prefix assignment whose value is
/// a substitution closed on the line still leaves the next word a program,
/// and stays that program's prefix.
const INJECTED: &[&str] = &[
    "X=$(true) git -c alias.x='!sh' x <<'O'",
    "X=`true` git -c alias.x='!sh' x <<'O'",
    "X=$(true) f <<'O'",
    "X=`true` f <<'O'",
    "X=$((1+2)) Y=$(echo a) f <<'O'",
    "GIT_EXEC_PATH=$(echo /tmp/x) git commit -F - <<'O'",
    "GIT_CONFIG_PARAMETERS=`printf x` git commit -F - <<'O'",
    "X=$(true) git commit -F - <<'O'",
    "X=$(true) tm x - <<'O'",
    "X=$(GIT_EXEC_PATH=/tmp/x git commit -F - <<'O'",
    "git -c alias.x='!sh' x <<'O'",
    "git -c alias.x=\\!sh x<<'O'",
    "git -C /tmp -c alias.x='!sh' x <<'O'",
    "git --config alias.x='!sh' x <<'O'",
    "git --config-env=alias.x=SH x <<'O'",
    "git --config-env alias.x=SH x <<'O'",
    "git -c core.hooksPath=/tmp/h commit -F - <<'O'",
    "git --exec-path=/tmp/x commit -F - <<'O'",
    "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=alias.x GIT_CONFIG_VALUE_0='!sh' git x <<'O'",
    "GIT_CONFIG_PARAMETERS=\"'alias.x=!sh'\" git x <<'O'",
    "git x <<'O'",
    "gh x <<'O'",
    "gh -R o/r x <<'O'",
    "cat <<'O' | git -c alias.x='!sh' x",
];

/// #9344: the critic's shape and its `--config`/`--config-env` variants run
/// their body through `sh`, so the floor judges that body as a command.
#[test]
fn config_or_alias_injected_reader_bodies_reach_the_floor_9344() {
    let mut allowed = Vec::new();
    for line in INJECTED {
        for payload in PAYLOADS {
            let command = format!("{line}\n{payload}\nO");
            if !floor(&command).is_some_and(DeleteTarget::is_floor) {
                allowed.push(command);
            }
        }
    }
    assert!(allowed.is_empty(), "allowed: {allowed:#?}");
}

/// #9344: an injected or alias-shaped reader line is no data line; a git line
/// that injects config runs a shell outright.
#[test]
fn injected_reader_lines_are_not_data_9344() {
    for line in INJECTED {
        assert!(!line_reads_as_data(line), "{line:?}");
    }
    for line in [
        "git -c alias.x='!sh' x <<'O'",
        "git --config-env=alias.x=SH x <<'O'",
        "GIT_CONFIG_COUNT=1 git x <<'O'",
        "GIT_EXEC_PATH=$(echo /tmp/x) git commit -F - <<'O'",
    ] {
        assert!(line_runs_a_shell(line), "{line:?}");
    }
}

/// #9344 rule 2: a data-reader body that holds a destructive-root command is
/// denied whatever the reader.
#[test]
fn a_data_reader_body_holding_a_root_delete_is_denied_9344() {
    for command in [
        "git commit -F - <<'EOF'\nrm -rf /\nEOF",
        "git commit -F - <<'EOF'\nfix: thing\n\n  sudo rm -rf ~\nEOF",
        "gh pr create --body-file - <<'EOF'\nok; rm -rf $HOME\nEOF",
        "gh issue comment 1 --body-file - <<'EOF'\nfind / -delete\nEOF",
        "cat > s.sh <<'EOF'\nrm -rf /\nEOF",
        "tee s.sh <<'EOF'\nrm -fr ~/\nEOF",
        "bash <<'O'\ncat > s.sh <<'E'\nrm -rf /\nE\nO",
        // #9344 round 2: a wrapper the resolver cannot measure fails closed.
        "cat > f <<'EOF'\nsudo --frobnicate rm -rf /\nEOF",
        "cat > f <<'EOF'\ntimeout soon rm -rf /\nEOF",
    ] {
        assert!(
            floor(command).is_some_and(DeleteTarget::is_floor),
            "{command:?}"
        );
    }
}

/// #9344: the commit-message and PR-body shapes stay allowed by every rule,
/// prose that names `rm` or a backticked root delete included. A bare
/// `rm -rf /` inside a sentence was denied before #9344 by the floor's
/// every-token scan, and still is.
#[test]
fn commit_and_pr_body_shapes_stay_allowed_9344() {
    for command in [
        "git commit -F - <<'EOF'\nfix: rm stale files\n\nThe floor denies `rm -rf /` and `rm -rf ~`.\nEOF",
        "git commit -F - <<'EOF'\nfix: make build passes; rm stale.log first\n\nRefs #9344\nEOF",
        "gh pr create --title t --body-file - <<'EOF'\n## Summary\nDo not rm the cache; run `rm -rf /` never.\nEOF",
        "gh issue comment 9344 --body-file - <<'EOF'\nWe rm old logs; the guard denies `rm -rf ~` in a body.\nEOF",
        "git -C /tmp/repo commit -F - <<'EOF'\nfix: thing\nEOF",
        "gh api repos/o/r/issues --input - <<'EOF'\n{\"body\": \"rm the cache\"}\nEOF",
    ] {
        assert_eq!(unclassifiable_command(command), None, "{command:?}");
        assert_eq!(evaluate_bash_command(command), None, "{command:?}");
        assert_eq!(floor(command), None, "{command:?}");
    }
}
