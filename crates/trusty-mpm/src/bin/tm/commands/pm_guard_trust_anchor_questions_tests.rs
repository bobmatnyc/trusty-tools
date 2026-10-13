//! #9815 (P2a of #8433): the channel question table is a trust anchor for
//! every session, the Architect included, with no live taint.

use super::super::tests::{Fixture, allowlist, architect_env, fixture, payload, pm_env};
use super::super::{AnchorWrite, HookEnv, evaluate};
use super::questions_verdict;
use serde_json::{Value, json};
use trusty_mpm::channels::question_table::{lock_path, questions_path};

const LOG: &str = "~/.trusty-tools/trusty-mpm/channels/questions.jsonl";
const LOCK: &str = "~/.trusty-tools/trusty-mpm/channels/questions.jsonl.lock";
const TORN: &str = "~/.trusty-tools/trusty-mpm/channels/questions.jsonl.torn";

/// [`fixture`] holding the table as the daemon leaves it: the log and lock.
fn with_table() -> Fixture {
    let fx = fixture();
    let log = questions_path(&fx.home);
    std::fs::create_dir_all(log.parent().expect("dir")).expect("mkdir");
    std::fs::write(&log, "").expect("log");
    std::fs::write(lock_path(&log), "").expect("lock");
    fx
}

fn verdict(fx: &Fixture, env: &HookEnv, tool: &str, input: Value) -> Option<String> {
    evaluate(&payload(fx, tool, input), env, || allowlist(fx))
}

fn bash(fx: &Fixture, env: &HookEnv, command: &str) -> Option<String> {
    verdict(fx, env, "Bash", json!({ "command": command }))
}

/// The deny `command` gets from `env`, which must be this rule's.
fn denied(fx: &Fixture, env: &HookEnv, command: &str) {
    let reason = bash(fx, env, command).unwrap_or_else(|| panic!("allowed: {command}"));
    assert!(reason.contains("#9815"), "{command}: {reason}");
}

/// 🔴 #9815: a PM and the Architect, with no taint live, may not append to,
/// truncate, delete, move or replace the question log, or its directory.
#[test]
fn anchor_denies_append_rm_mv_and_truncate_of_the_questions_log() {
    let fx = with_table();
    for env in [pm_env(&fx), architect_env(&fx)] {
        for command in [
            format!("echo '{{\"v\":1}}' >> {LOG}"),
            format!(": > {LOG}"),
            format!("truncate -s 0 {LOG}"),
            format!("sed -i '' -e '$d' {LOG}"),
            format!("rm {LOG}"),
            format!("rm -f {LOG}"),
            format!("unlink {LOG}"),
            format!("mv {LOG} /tmp/away.jsonl"),
            format!("mv /tmp/forged.jsonl {LOG}"),
            format!("cp /tmp/forged.jsonl {LOG}"),
            format!("ln -sf /tmp/forged.jsonl {LOG}"),
            "cd ~/.trusty-tools/trusty-mpm/channels && echo x >> questions.jsonl".to_owned(),
            "rm -rf ~/.trusty-tools/trusty-mpm/channels".to_owned(),
            "mv ~/.trusty-tools/trusty-mpm/channels /tmp/away".to_owned(),
        ] {
            denied(&fx, &env, &command);
        }
        let path = questions_path(&fx.home).display().to_string();
        for tool in ["Write", "Edit", "MultiEdit"] {
            let input = json!({ "file_path": path, "content": "{}" });
            let reason = verdict(&fx, &env, tool, input).expect(tool);
            assert!(reason.contains("#9815"), "{tool}: {reason}");
        }
        assert_eq!(bash(&fx, &env, "echo x > notes.md"), None, "unrelated");
    }
}

/// 🔴 #9815: the lock may not be deleted, replaced or written, so a second
/// daemon cannot take a fresh lock beside a live table.
#[test]
fn anchor_denies_rm_and_replace_of_the_questions_lock() {
    let fx = with_table();
    for env in [pm_env(&fx), architect_env(&fx)] {
        for command in [
            format!("rm {LOCK}"),
            format!("mv /tmp/x {LOCK}"),
            format!("ln -sf /tmp/x {LOCK}"),
            format!("echo x > {LOCK}"),
        ] {
            denied(&fx, &env, &command);
        }
    }
}

/// 🔴 #9815: a link or file planted at the torn-line path is denied; the
/// daemon would append to it. The file does not exist yet, so no hard-link
/// identity can catch it: only the guarded path does.
#[test]
fn anchor_denies_a_link_planted_at_the_torn_file() {
    let fx = with_table();
    for env in [pm_env(&fx), architect_env(&fx)] {
        denied(&fx, &env, &format!("ln -s ~/.zshrc {TORN}"));
        denied(&fx, &env, &format!("echo x > {TORN}"));
    }
}

/// 🔴 #9815 (r-078): with no taint live, a write the guard cannot place
/// (`xargs cp`) is denied to the Architect, because it could land on the
/// table.
#[test]
fn the_architect_cannot_reach_the_question_table_through_an_unplaceable_write() {
    let fx = fixture();
    denied(&fx, &architect_env(&fx), "ls | xargs cp a.txt");
}

/// 🔴 #9815 (r-078): with no home the table cannot be located, so every write
/// is denied.
#[test]
fn questions_anchor_unknown_home_denies_every_write() {
    let writes = Ok(vec![AnchorWrite::Literal("/tmp/notes.md".to_owned())]);
    let reason = questions_verdict(&writes, None, None).expect("denied");
    assert!(reason.contains("#9815"), "{reason}");
}

/// 🔴 #9815 (r-078): a hard link to the log made before the rule is the log;
/// a write through it is denied to a PM and to the Architect.
#[test]
fn a_hard_link_to_the_questions_log_is_an_anchor() {
    let fx = with_table();
    let alias = fx.cwd.join("alias.jsonl");
    std::fs::hard_link(questions_path(&fx.home), &alias).expect("hard link");
    for env in [pm_env(&fx), architect_env(&fx)] {
        denied(&fx, &env, &format!("echo x >> {}", alias.display()));
    }
}

/// 🔴 #9815 (r-078): a log location that does not resolve (a link loop) is a
/// table the guard cannot locate, so every write is denied.
#[test]
fn an_unresolvable_questions_log_denies_every_write() {
    let fx = fixture();
    let log = questions_path(&fx.home);
    std::fs::create_dir_all(log.parent().expect("dir")).expect("mkdir");
    std::os::unix::fs::symlink("questions.jsonl", &log).expect("loop");
    for env in [pm_env(&fx), architect_env(&fx)] {
        denied(&fx, &env, "echo x > notes.md");
    }
}

/// The rows of `commands` whose verdict for `env` is not `want_denied`, with
/// the verdict each got, so one run lists every mismatch.
fn mismatches(fx: &Fixture, env: &HookEnv, commands: &[String], want_denied: bool) -> Vec<String> {
    commands
        .iter()
        .filter_map(|command| {
            let got = bash(fx, env, command);
            (got.is_some() != want_denied).then(|| format!("{command} -> {got:?}"))
        })
        .collect()
}

/// 🔴 #9815 (Architect ruling (b)): with no taint live, the Architect may make
/// routine writes whose possible file names cannot be one of the table's
/// files or its last three directories.
#[test]
fn the_architect_may_make_routine_writes_that_cannot_name_the_table() {
    let fx = with_table();
    let home_name = fx.home.file_name().expect("home name").to_string_lossy();
    let rows = [
        "sed -i 's/a/b/' src/*.rs".to_owned(),
        "sed -i 's/a/b/' crates/*/Cargo.toml".to_owned(),
        "echo x > \"$OUT\"".to_owned(),
        "echo x | tee \"$LOG\"".to_owned(),
        "cp a.txt \"$DEST\"".to_owned(),
        "cp a.txt \"${DEST}/\"".to_owned(),
        "cd d && cp a.txt .".to_owned(),
        "cd d && cp a.txt ..".to_owned(),
        "echo x > \"$OUT.log\"".to_owned(),
        "echo x > \"$D/notes.md\"".to_owned(),
        format!("cp a.txt \"$DEST/{home_name}\""),
        "cd d && rm -rf Users".to_owned(),
    ];
    let wrong = mismatches(&fx, &architect_env(&fx), &rows, false);
    assert!(wrong.is_empty(), "denied:\n{}", wrong.join("\n"));
}

/// 🔴 #9815 (Architect ruling (b)): a target whose file name could still be
/// one of the table's files, or one of its last three directories, stays
/// denied to the Architect: a literal name behind an expansion, a glob or an
/// expansion with literal text that could match, `.` or `..` written or
/// deleted after a `cd`, a `~user` home, and a write `xargs` places.
#[test]
fn the_architect_is_denied_targets_that_could_name_the_table() {
    let fx = with_table();
    let rows = [
        "echo x > \"$D/questions.jsonl\"",
        "cp forged \"$D/QUESTIONS.JSONL.LOCK\"",
        "echo x > \"${D}/questions.jsonl.torn\"",
        "echo x > \"$D\"/questions.*",
        "sed -i 's/a/b/' \"$D\"/*.jsonl",
        "echo x > \"$D\"/questions.json[l]",
        "echo x > \"$D/${NAME}.jsonl\"",
        "echo x > \"$D/questions$EXT\"",
        "echo x > \"$(printf questions.jsonl)\"",
        "echo x > \"`printf questions.jsonl`\"",
        "echo x > \"${D:-x}\"",
        "echo x > \"$1\"",
        "echo x > \"$D\"/questions.{jsonl,x}",
        "echo x > \"$D\"/questions.jsonl.to?n",
        "mv forged \"$D/channels\"",
        "rm -rf \"$D/trusty-mpm\"",
        "rm -rf \"$D/.trusty-tools\"",
        "cd d && rm -rf .",
        "cd d && rm -rf ..",
        "cd d && cp -r forged/ .",
        "cd d && cp forged/questions.jsonl .",
        "cp a.txt ~bob",
        "ls | xargs cp a.txt",
        "ls | xargs sed -i 's/a/b/'",
    ]
    .map(str::to_owned);
    let wrong = mismatches(&fx, &architect_env(&fx), &rows, true);
    assert!(wrong.is_empty(), "allowed:\n{}", wrong.join("\n"));
    for row in &rows {
        denied(&fx, &architect_env(&fx), row);
    }
}

/// 🔴 #9815 (r-078): the table directory's resolved names count as well as
/// its spelled ones, so a `.trusty-tools` that is a link is matched by the
/// name it points at.
#[test]
fn a_linked_table_directory_is_matched_by_its_resolved_name() {
    let fx = fixture();
    let real = fx.home.join("real-tools");
    std::fs::create_dir_all(real.join("trusty-mpm").join("channels")).expect("mkdir");
    std::os::unix::fs::symlink(&real, fx.home.join(".trusty-tools")).expect("link");
    let log = questions_path(&fx.home);
    std::fs::write(&log, "").expect("log");
    std::fs::write(lock_path(&log), "").expect("lock");
    denied(&fx, &architect_env(&fx), "rm -rf \"$D/real-tools\"");
}

/// 🔴 #9815: the PM and agent rule is unchanged by the Architect's narrow
/// table rule: every expansion or glob target is still denied by #8878.
#[test]
fn the_pm_keeps_the_broad_name_rule() {
    let fx = with_table();
    let home_name = fx.home.file_name().expect("home name").to_string_lossy();
    for command in [
        "echo x > \"$OUT\"".to_owned(),
        "sed -i 's/a/b/' src/*.rs".to_owned(),
        "cd d && cp a.txt .".to_owned(),
        format!("cp a.txt \"$DEST/{home_name}\""),
    ] {
        let reason = bash(&fx, &pm_env(&fx), &command).unwrap_or_else(|| panic!("{command}"));
        assert!(reason.contains("#8878"), "{command}: {reason}");
    }
}
