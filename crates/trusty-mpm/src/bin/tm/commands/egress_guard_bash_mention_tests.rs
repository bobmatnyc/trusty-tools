//! Unit tests for the #11273 inert-mention rule of GP-044
//! (`egress_guard_bash_mention.rs`), judged through the Bash arm's own
//! [`bash_egress_deny`], which no role or thread changes.

use crate::commands::egress_guard_bash::{BashDeny, bash_egress_deny};

/// 🔴 #11273 criterion 5: a family host named only as inert text — a `gh`
/// title or body, a commit message, a branch name, a path, a `grep`/`rg`
/// pattern — is no Bash channel hit. Each of these was denied on base
/// 4894f7bf5b, where a host in any word denied.
#[test]
fn mention_only_commands_pass_for_every_role_11273() {
    for command in [
        "gh issue create --title \"egress: hooks.slack.com is denied\" --body \"see api.slack.com\"",
        "gh pr create --title 'note gmail.googleapis.com scope' --body 'x y'",
        "gh pr comment 12 --body \"chat.googleapis.com mention\"",
        "git commit -m \"docs: name hooks.slack.com in the guard doc\"",
        "git checkout -b fix/hooks.slack.com-mention",
        "git add docs/api.notion.com.md && git commit -m 'add note'",
        "git log --grep slack.com",
        "grep -rn \"hooks.slack.com\" crates/",
        "rg 'gmail.googleapis.com' src | wc -l",
        "cat notes/api.notion.com.txt",
        "ls docs/chat.googleapis.com",
        "cd /tmp && grep mail.google.com f | head -3",
        // #11273 round 3: a quoted `>` is text, a prose `=` value is prose,
        // and a here-document operator opens no file.
        "git commit -m \"guard: a -> hooks.slack.com <b>\"",
        "gh issue create --title=\"egress: hooks.slack.com x\" --body y",
        "gh release create v1 --notes \"names api.slack.com\"",
        "cat <<'EOF'\nhooks.slack.com\nEOF",
    ] {
        assert_eq!(bash_egress_deny(command), None, "{command}");
    }
}

/// 🔴 #11273 criterion 5: anything that can reach a family endpoint or run
/// a family sender keeps today's deny — a network program, a substitution
/// (backticks inside double quotes included), a socket redirect, a nested
/// shell, a `gh` host word or option, a program that runs project code, an
/// assignment or wrapper in front, and a family program.
#[test]
fn look_alike_mentions_stay_denied_11273() {
    for command in [
        "curl https://hooks.slack.com/x",
        "wget hooks.slack.com",
        "nc hooks.slack.com 443",
        "http POST https://slack.com/api/chat.postMessage",
        "python3 -c \"import urllib.request as u; u.urlopen('https://hooks.slack.com/x')\"",
        "echo $(curl https://hooks.slack.com/x)",
        "gh issue create --title \"x `curl hooks.slack.com`\" --body y",
        "git commit -m \"$(curl https://hooks.slack.com/x)\"",
        "echo hi > /dev/tcp/hooks.slack.com/443",
        "grep x f | sh -c 'curl hooks.slack.com'",
        "echo hooks.slack.com | xargs curl",
        "gh repo clone https://hooks.slack.com/a/b",
        "gh issue list --repo hooks.slack.com/o/r",
        "gh api https://slack.com/api/chat.postMessage",
        "git push https://hooks.slack.com/x main",
        "git -c core.sshCommand=x status hooks.slack.com",
        "rg --pre 'curl hooks.slack.com' x",
        "cargo test \"hooks.slack.com mention\"",
        "H=hooks.slack.com curl $H",
        "env grep hooks.slack.com f",
        "tm slack send '#ops' hi",
    ] {
        assert!(
            matches!(bash_egress_deny(command), Some(BashDeny::Channel(_))),
            "{command}"
        );
    }
}

/// 🔴 #11273 review H1: a redirect to or from a socket path stays denied in
/// every quoting and operator spelling. Each passed on 753969b1b6, which read
/// the socket path in the raw text only.
#[test]
fn quoted_socket_redirects_stay_denied_11273() {
    assert_all_denied(&[
        "echo hi > \"/dev/\"tcp/hooks.slack.com/80",
        "echo hi >\"/dev/\"tcp/hooks.slack.com/80",
        "echo hi > /dev/'tcp'/hooks.slack.com/80",
        "echo hi > /dev/\\tcp/hooks.slack.com/80",
        "echo hi >\t\"/dev/\"tcp/hooks.slack.com/80",
        "echo hi >> \"/dev/\"tcp/hooks.slack.com/80",
        "cat <> \"/dev/\"tcp/hooks.slack.com/80",
        "cat < \"/dev/\"tcp/hooks.slack.com/80",
        "cat 0< \"/dev/\"udp/hooks.slack.com/53",
        "echo hi 3> \"/dev/\"tcp/hooks.slack.com/80",
        "echo hi &> \"/dev/\"tcp/hooks.slack.com/80",
        "echo hi > $(printf /dev)/tcp/hooks.slack.com/80",
    ]);
}

/// 🔴 #11273 review M1/L1: a `gh` repo, host or URL word that names a family
/// host denies however it is padded or joined, and `git -C`/`--work-tree`
/// point git at another repository's hooks. Each passed on 753969b1b6.
#[test]
fn padded_gh_hosts_and_git_repo_options_stay_denied_11273() {
    assert_all_denied(&[
        "gh issue list --repo \"hooks.slack.com/o/r \"",
        "gh issue list --repo \" hooks.slack.com/o/r\"",
        "gh issue list --repo \"hooks.slack.com/o/r\t\"",
        "gh issue list --repo=\"hooks.slack.com/o/r \"",
        "gh issue list -R \"hooks.slack.com/o/r \"",
        "gh repo view \"https://hooks.slack.com/o/r \"",
        "git -C /tmp/hooks.slack.com status",
        "git --work-tree=/tmp/hooks.slack.com status",
    ]);
}

/// Every command in `commands` is a Bash channel deny; names each that is not.
fn assert_all_denied(commands: &[&str]) {
    let passed: Vec<&&str> = commands
        .iter()
        .filter(|command| !matches!(bash_egress_deny(command), Some(BashDeny::Channel(_))))
        .collect();
    assert!(passed.is_empty(), "not denied: {passed:#?}");
}
