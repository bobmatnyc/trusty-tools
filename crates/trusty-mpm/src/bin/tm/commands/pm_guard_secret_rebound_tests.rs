//! #10566: a secret DELETE through a program name the command rebinds. Every
//! fixture is a fake placeholder; nothing is executed.

use super::*;
use crate::commands::pm_guard_secret_read::evaluate_secret_file_read;

/// A literal DELETE tail of a repository secret, with the program removed.
const TAIL: &str = "api -X DELETE repos/o/r/actions/secrets/K";

/// Every row is refused by the #8875 rule and end to end.
fn all_refuse(rows: &[String]) {
    let allowed: Vec<&String> = rows
        .iter()
        .filter(|c| {
            let rule = evaluate_gh_api_secret_delete(c);
            assert!(
                rule.as_deref().is_none_or(|r| r.contains("issue #8875")),
                "{c}: {rule:?}"
            );
            let unified = serde_json::json!({ "command": c });
            rule.is_none() || evaluate_secret_file_read("Bash", Some(&unified)).is_none()
        })
        .collect();
    assert!(allowed.is_empty(), "must refuse: {allowed:#?}");
}

/// 🔴 REGRESSION (#10566): a name rebound by `hash -p`, `alias`, a function,
/// `enable`, a command table or an `eval`, then run — plainly, behind
/// `command`/`exec`, inside `$( … )` or backticks — refuses as the plain
/// `gh api` DELETE does.
#[test]
fn a_call_through_a_rebound_name_is_refused_10566() {
    let rows: Vec<String> = [
        format!("hash -p /usr/bin/gh ls; ls {TAIL}"),
        format!("alias ls=gh; ls {TAIL}"),
        "alias ls='gh api -X DELETE'; ls repos/o/r/actions/secrets/K".to_string(),
        format!("function ls {{ gh \"$@\"; }}; ls {TAIL}"),
        format!("ls() {{ gh \"$@\"; }}; ls {TAIL}"),
        format!("BASH_CMDS[ls]=/usr/bin/gh; ls {TAIL}"),
        format!("commands[ls]=/usr/bin/gh; ls {TAIL}"),
        "BASH_ALIASES[ls]='gh api'; ls -X DELETE repos/o/r/actions/secrets/K".to_string(),
        format!("enable -f ./fake.so ls; ls {TAIL}"),
        format!("eval 'hash -p /usr/bin/gh ls'; ls {TAIL}"),
        format!("hash -p /usr/bin/gh ls; eval 'ls {TAIL}'"),
        format!("hash -p /usr/bin/gh ls; command ls {TAIL}"),
        "alias ls=gh; exec ls secret delete K".to_string(),
        format!("hash -p /usr/bin/gh ls; echo \"$(ls {TAIL})\""),
        format!("hash -p /usr/bin/gh ls; echo `ls {TAIL}`"),
        "hash -p /usr/bin/curl get; get -X DELETE https://api.example.test/repos/o/r/actions/secrets/K"
            .to_string(),
    ]
    .to_vec();
    all_refuse(&rows);
}

/// 🔴 REGRESSION (#10566, fail closed): when the guard cannot tell which name
/// a command rebinds — a sourced file, a new `PATH`, an `eval` of an
/// expansion, a computed name, an unlexable `alias` — or an alias chain runs
/// past the re-judge bound, any word may run gh.
#[test]
fn an_unknown_rebinding_fails_closed_10566() {
    let rows: Vec<String> = [
        format!("source ./fake-defs.sh; ls {TAIL}"),
        format!("PATH=./shim:/usr/bin ls {TAIL}"),
        format!("eval \"$DEFS\"; ls {TAIL}"),
        format!("hash -p /usr/bin/gh \"$N\"; ls {TAIL}"),
        // An open `"` makes the `alias` segment unlexable.
        format!("alias ls=\"g\\\"h; ls {TAIL}"),
    ]
    .to_vec();
    all_refuse(&rows);
    let chain: Vec<String> = (0..=MAX_DEPTH + 1)
        .map(|n| format!("a{n}=a{}", n + 1))
        .collect();
    let command = format!("alias {}; a0", chain.join(" "));
    assert!(
        evaluate_gh_api_secret_delete(&command).is_some(),
        "an alias chain past the bound must deny: {command}"
    );
}

/// No over-deny (#10566): a rebound name that runs no DELETE, a self alias,
/// a GET listing through a rebound gh, and the common `source`/`PATH`/`eval`
/// shapes allow.
#[test]
fn a_rebound_name_with_no_delete_still_allows_10566() {
    for command in [
        "alias ll='ls -la'; ll \"$DIR\"",
        "alias ls='ls -l'; ls",
        "ls() { command ls -la \"$@\"; }; ls src",
        "hash -p /usr/bin/gh g; g api repos/o/r/actions/secrets",
        "source .venv/bin/activate && pytest -k \"$T\" --maxfail=1",
        "PATH=\"$HOME/.cargo/bin:$PATH\" cargo test -p fake-crate --no-fail-fast",
        "eval \"$(ssh-agent -s)\" && ssh-add -l",
        "hash -r; cargo build",
    ] {
        assert_eq!(
            evaluate_gh_api_secret_delete(command),
            None,
            "`{command}` must allow"
        );
    }
}

/// 🔴 REGRESSION (#10566 round 2, critic 1): a function defined with no
/// space before the brace or a space before the parens rebinds its name.
#[test]
fn a_paren_function_definition_rebinds_10566() {
    let rows: Vec<String> = ["ls(){", "ls (){", "ls ( ) {", "function ls(){"]
        .iter()
        .map(|def| format!("{def} gh \"$@\"; }}; ls {TAIL}"))
        .collect();
    all_refuse(&rows);
}

/// 🔴 REGRESSION (#10566 round 2, critic 2): a whole command or alias table
/// assigned at once, or appended to, may bind any name.
#[test]
fn a_whole_table_assignment_rebinds_10566() {
    let rows: Vec<String> = [
        "BASH_CMDS=([ls]=/usr/bin/gh)",
        "declare -A BASH_CMDS=([ls]=/usr/bin/gh)",
        "BASH_CMDS+=([ls]=/usr/bin/gh)",
        "BASH_ALIASES=([ls]=gh)",
        "commands=(ls /usr/bin/gh)",
        "aliases=(ls gh)",
        "functions+=(ls 'gh \"$@\"')",
    ]
    .iter()
    .map(|table| format!("{table}; ls {TAIL}"))
    .collect();
    all_refuse(&rows);
}

/// 🔴 REGRESSION (#10566 round 2, critic 7): forty `eval` words are read in
/// linear time and refuse (the re-judge bound), not hang.
#[test]
fn a_run_of_evals_is_judged_in_linear_time_10566() {
    let command = format!("{}true; ls {TAIL}", "eval ".repeat(40));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(evaluate_gh_api_secret_delete(&command).is_some());
    });
    let refused = rx.recv_timeout(std::time::Duration::from_secs(5));
    assert_eq!(refused, Ok(true), "must refuse within 5 s");
}

/// 🔴 REGRESSION (#10622): a name bound through the zsh global or suffix
/// alias tables, by entry, whole assignment or `alias -s`, refuses as the
/// plain `gh api` DELETE does.
#[test]
fn a_zsh_global_or_suffix_alias_rebinds_10622() {
    let rows: Vec<String> = [
        format!("galiases[ls]=gh; ls {TAIL}"),
        "galiases[ls]='gh api'; ls -X DELETE repos/o/r/actions/secrets/K".to_string(),
        format!("galiases=(ls gh); ls {TAIL}"),
        format!("saliases[sh]=gh; ./fake.sh {TAIL}"),
        format!("saliases=(sh gh); ./fake.sh {TAIL}"),
        format!("alias -s sh=gh; ./fake.sh {TAIL}"),
    ]
    .to_vec();
    all_refuse(&rows);
}
