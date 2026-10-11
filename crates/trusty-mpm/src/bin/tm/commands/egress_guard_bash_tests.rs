//! Unit tests for the Bash arm of `tm hook --egress-guard` (#8454 S3). The
//! binary-level claims live in `tests/tm_hook_egress_guard_8454.rs`.

use super::*;
use crate::commands::egress_guard::{Verdict, decide, verdict};
use crate::commands::egress_guard_families::FamilyTable;
use crate::commands::hook_stdin::HookStdinError;

/// A Bash `PreToolUse` read carrying `input` as its `tool_input`.
fn bash_read(input: serde_json::Value) -> Result<serde_json::Value, HookStdinError> {
    Ok(serde_json::json!({ "tool_name": "Bash", "tool_input": input }))
}

/// The guard's verdict on a Bash call running `command`.
fn bash(command: &str) -> Verdict {
    decide(
        &bash_read(serde_json::json!({ "command": command })),
        &FamilyTable::builtin(),
    )
}

/// Assert `command` is an `egress-bash` deny naming `service`.
fn assert_denies(command: &str, service: &str) {
    match bash(command) {
        Verdict::Deny { check, reason } => {
            assert_eq!(check, BASH_CHECK, "`{command}`: {reason}");
            assert!(
                reason.contains(&format!("it reaches {service} (")),
                "`{command}` must name {service}: {reason}"
            );
        }
        Verdict::Allow => panic!("`{command}` must be denied as {service}"),
    }
}

/// Assert `command` is allowed.
fn assert_allows(command: &str) {
    assert_eq!(bash(command), Verdict::Allow, "`{command}` must be allowed");
}

/// `n` nested `$(echo …` levels around `inner`.
fn nested(inner: &str, n: usize) -> String {
    format!("{}{inner}{}", "echo $(".repeat(n), ")".repeat(n))
}

/// 🔴 #8454 S3: a network client naming a Slack host is denied, whatever the
/// client, casing, port or trailing dot.
#[test]
fn bash_egress_denies_curl_slack_host() {
    for command in [
        "curl -X POST https://slack.com/api/chat.postMessage -d text=hi",
        "wget -qO- https://files.slack.com/x",
        "curl https://a.slack-edge.com/x.png",
        "http POST slack.com/api/chat.postMessage text=hi",
        "xh slack.com/api/auth.test",
        "nc slack.com 443",
        "openssl s_client -connect slack.com:443",
        "CURL HTTPS://API.SLACK.COM/api",
        "curl https://slack.com./api",
    ] {
        assert_denies(command, "Slack");
    }
}

/// 🔴 #8454 S3: an incoming-webhook URL is denied, userinfo and port included.
#[test]
fn bash_egress_denies_hooks_slack_url() {
    for command in [
        "curl -d '{\"text\":\"hi\"}' https://hooks.slack.com/services/T0/B0/x",
        "curl \"https://bot@hooks.slack.com:443/services/T0/B0/x\"",
    ] {
        assert_denies(command, "Slack");
    }
}

/// 🔴 #8454 S3: both Notion API hosts are denied.
#[test]
fn bash_egress_denies_notion_host() {
    for command in [
        "curl https://api.notion.com/v1/pages -d @page.json",
        "wget https://www.notion.so/workspace/page",
    ] {
        assert_denies(command, "Notion");
    }
}

/// 🔴 #8454 S3: the Gmail API, web and SMTP hosts are denied.
#[test]
fn bash_egress_denies_gmail_googleapis_host() {
    for command in [
        "curl https://gmail.googleapis.com/gmail/v1/users/me/messages/send",
        "curl https://mail.google.com/mail/u/0",
        "openssl s_client -connect smtp.gmail.com:465",
    ] {
        assert_denies(command, "Gmail");
    }
}

/// 🔴 #8454 S3: the Google Chat API host is denied.
#[test]
fn bash_egress_denies_chat_googleapis_host() {
    assert_denies(
        "curl -X POST https://chat.googleapis.com/v1/spaces/AAA/messages",
        "Google Chat",
    );
}

/// 🔴 #8454 S3: every mail client in command position is denied, and so is an
/// `smtp://` URL or an `smtp.*` host on a mail port.
#[test]
fn bash_egress_denies_sendmail_and_mail_clients() {
    for command in [
        "sendmail bob@example.com < msg.txt",
        "/usr/sbin/sendmail -t",
        "mail -s hi bob@example.com",
        "Mail -s hi bob@example.com",
        "mailx bob@example.com",
        "msmtp bob@example.com",
        "mutt -s hi bob@example.com",
        "swaks --to bob@example.com",
        "curl --url smtp://mx.example.com --mail-rcpt bob@example.com",
        "curl smtps://mx.example.com:465",
        "nc smtp.example.com 25",
        "telnet smtp.example.org 587",
        "openssl s_client -connect smtp.example.net:465",
    ] {
        assert_denies(command, "SMTP mail");
    }
}

/// 🔴 #8454 S3: the channel binaries are denied in command position, by path
/// too, and `gchat-mcp` with any argv but the exact carve-out.
#[test]
fn bash_egress_denies_channel_binaries() {
    for (command, service) in [
        ("slack-mcp", "Slack"),
        ("slack-mcp send --channel C1 hi", "Slack"),
        ("~/.cargo/bin/slack-mcp serve", "Slack"),
        ("gchat-mcp send --space x hi", "Google Chat"),
        ("gchat-mcp doctor --offline extra", "Google Chat"),
        ("gws gmail users messages send", "Google Workspace"),
        ("gworkspace-mcp", "Google Workspace"),
    ] {
        assert_denies(command, service);
    }
}

/// 🔴 #8454 S3 (Architect ruling 3): only the exact argv
/// `gchat-mcp doctor --offline` is carved out; plain `doctor` is denied.
#[test]
fn bash_egress_denies_gchat_mcp_doctor_without_offline() {
    for command in [
        "gchat-mcp doctor",
        "gchat-mcp doctor --online",
        "gchat-mcp --offline doctor",
        "gchat-mcp doctor --offline --send",
    ] {
        assert_denies(command, "Google Chat");
    }
}

/// 🔴 #8454 S3: `tm slack` and `tm gchat` are denied, past tm's global flags.
#[test]
fn bash_egress_denies_tm_slack_subcommand() {
    for (command, service) in [
        ("tm slack send hi", "Slack"),
        ("tm --url http://127.0.0.1:1 slack status", "Slack"),
        ("/usr/local/bin/tm slack", "Slack"),
        ("tm gchat send hi", "Google Chat"),
    ] {
        assert_denies(command, service);
    }
}

/// 🔴 #8454 S3: a Python `-c` program importing `smtplib` or naming a family
/// host is denied.
#[test]
fn bash_egress_denies_python_one_liner() {
    assert_denies(
        "python3 -c 'import smtplib; smtplib.SMTP(\"mx\").sendmail(a, b, m)'",
        "SMTP mail",
    );
    assert_denies("python3.12 -c 'import smtplib'", "SMTP mail");
    assert_denies(
        "python -c \"import urllib.request as u; u.urlopen('https://hooks.slack.com/x')\"",
        "Slack",
    );
}

/// 🔴 #8454 S3: a Node or Bun `-e` program naming a family host is denied.
#[test]
fn bash_egress_denies_node_one_liner() {
    assert_denies("node -e \"fetch('https://hooks.slack.com/x')\"", "Slack");
    assert_denies("bun -e \"fetch('https://api.notion.com/v1')\"", "Notion");
}

/// 🔴 #8454 S3: wrappers do not hide the program: `sh -c`, `bash -c`, `env`,
/// `sudo` with an option, `nohup`, `time` and `command`.
#[test]
fn bash_egress_denies_wrapped_in_sh_c() {
    for command in [
        "sh -c 'sendmail bob@example.com < m'",
        "bash -c \"mailx bob\"",
        "env FOO=1 sendmail bob",
        "sudo -u root sendmail bob",
        "nohup swaks --to bob &",
        "time msmtp bob",
        "command mail -s x bob",
    ] {
        assert_denies(command, "SMTP mail");
    }
}

/// 🔴 #8454 S3: a mail client after a pipe or behind `xargs` is denied.
#[test]
fn bash_egress_denies_pipe_and_xargs() {
    for command in [
        "echo hi | mail -s x bob",
        "echo bob | xargs sendmail",
        "printf x | xargs -I{} mutt {}",
        "cd /tmp && mailx bob; echo done",
    ] {
        assert_denies(command, "SMTP mail");
    }
}

/// 🔴 #8454 S3: a program inside `$(…)`, backticks, `<(…)` or a subshell is
/// denied.
#[test]
fn bash_egress_denies_command_substitution_body() {
    for command in [
        "echo $(sendmail bob < m)",
        "x=\"$(mail -s hi bob)\"",
        "echo `msmtp bob`",
        "cat <(mutt bob)",
        "(mailx bob)",
    ] {
        assert_denies(command, "SMTP mail");
    }
}

/// 🔴 #8454 S3: an `eval` string is judged as the command it runs.
#[test]
fn bash_egress_denies_eval_string() {
    for command in ["eval \"sendmail bob\"", "eval mail -s x bob"] {
        assert_denies(command, "SMTP mail");
    }
}

/// 🔴 #8454 S3: a here-document body fed to an interpreter or a shell is
/// judged as program text.
#[test]
fn bash_egress_denies_heredoc_to_interpreter() {
    assert_denies(
        "python3 - <<'EOF'\nimport smtplib\nsmtplib.SMTP('mx').sendmail(a, b, m)\nEOF",
        "SMTP mail",
    );
    assert_denies("bash <<'EOF'\nsendmail bob < m\nEOF", "SMTP mail");
    assert_denies(
        "node <<'EOF'\nfetch('https://hooks.slack.com/x')\nEOF",
        "Slack",
    );
}

/// 🔴 #8454 S3: an `$'…'` quote is judged after decoding, and a host split
/// across adjacent quotes or by a backslash is judged as the word the shell
/// joins.
#[test]
fn bash_egress_denies_ansi_c_quoted_names() {
    assert_denies("$'\\x73endmail' bob", "SMTP mail");
    assert_denies("curl $'https://hooks.\\x73lack.com/x'", "Slack");
    assert_denies("curl \"https://hooks.sl\"\"ack.com/x\"", "Slack");
    assert_denies("curl https://hooks.sl\\ack.com/x", "Slack");
}

/// 🔴 #8454 S3 critic round 1 (CRITICAL): a command whose other quote the
/// decoder refuses is still judged on its decodable quotes. The raw scan
/// splits `hooks.\x73lack.com` at the backslash, so it alone allowed these.
#[test]
fn bash_egress_denies_ansi_c_beside_an_undecodable_quote() {
    assert_denies(
        "curl $'https://hooks.\\x73lack.com/services/x' -d $'hi\\n'",
        "Slack",
    );
    assert_denies("$'\\x73endmail' bob $\"x\"", "SMTP mail");
}

/// 🔴 #8454 S3 critic round 1 (HIGH): a shell keyword in front of the program
/// word does not hide it.
#[test]
fn bash_egress_denies_program_behind_a_shell_keyword() {
    for (command, service) in [
        ("if true; then sendmail bob </dev/null; fi", "SMTP mail"),
        ("{ mail -s x a@b </dev/null; }", "SMTP mail"),
        ("! sendmail bob", "SMTP mail"),
        ("while gws x; do :; done", "Google Workspace"),
        ("if ! tm slack send x; then :; fi", "Slack"),
    ] {
        assert_denies(command, service);
    }
}

/// 🔴 #8454 S3 (plan §5, fail closed): a command the guard cannot lex or nest
/// through is denied when its text holds a family token. An allow here is the
/// error arm failing open.
#[test]
fn bash_egress_denies_unlexable_with_family_token() {
    for (command, service) in [
        ("sendmail bob 'unclosed".to_owned(), "SMTP mail"),
        ("echo $(mail bob".to_owned(), "SMTP mail"),
        ("bash -c 'mutt bob".to_owned(), "SMTP mail"),
        ("$\"sendmail\" bob".to_owned(), "SMTP mail"),
        ("tm slack 'send".to_owned(), "Slack"),
        ("python3 -c 'import smtplib".to_owned(), "SMTP mail"),
        (nested("sendmail bob", 40), "SMTP mail"),
        (
            "curl https://hooks.slack.com/x \"unclosed".to_owned(),
            "Slack",
        ),
    ] {
        assert_denies(&command, service);
    }
}

/// 🔴 #8454 S3 (§4 fail closed): a Bash call whose `command` is missing or not
/// a string is denied.
#[test]
fn bash_egress_denies_missing_or_non_string_command() {
    for input in [
        serde_json::json!({}),
        serde_json::json!({ "command": null }),
        serde_json::json!({ "command": 7 }),
        serde_json::json!({ "command": ["sendmail"] }),
    ] {
        assert!(
            matches!(
                decide(&bash_read(input.clone()), &FamilyTable::builtin()),
                Verdict::Deny {
                    check: "egress-unclassifiable-payload",
                    ..
                }
            ),
            "{input}"
        );
    }
}

/// 🔴 #8454 S3: the trust-anchor floor answers before the Bash arm; an anchor
/// allow leaves the Bash verdict as it was.
#[test]
fn bash_egress_trust_anchor_verdict_precedes_the_bash_arm() {
    let table = FamilyTable::builtin();
    let read = bash_read(serde_json::json!({
        "command": "curl https://hooks.slack.com/services/x"
    }));
    match verdict(&read, &table, |_| Some("anchored".to_owned())) {
        Verdict::Deny { check, reason } => {
            assert_eq!(check, "trust-anchor-write");
            assert_eq!(reason, "anchored");
        }
        Verdict::Allow => panic!("an anchor deny must deny"),
    }
    match verdict(&read, &table, |_| None) {
        Verdict::Deny { check, reason } => {
            assert_eq!(check, BASH_CHECK);
            assert_eq!(
                reason,
                "tm hook --egress-guard denies this Bash command: it reaches Slack \
                 (hooks.slack.com) outside the owner channel, and no tm-launched session, PM \
                 or subagent may message Slack, Gmail, Notion or Google Chat directly (#8454). \
                 Ask the operator instead."
            );
        }
        Verdict::Allow => panic!("a Slack host must deny"),
    }
}

/// #8454 S3 control: ordinary network and git commands are allowed.
#[test]
fn bash_allows_curl_github() {
    for command in [
        "curl https://api.github.com/repos/o/r",
        "gh pr view 1",
        "git push origin feat/x",
        "wget https://example.com/x.tar.gz",
    ] {
        assert_allows(command);
    }
}

/// #8454 S3 control: hosts are judged on whole labels, so a family name
/// inside another host or word is not a family host.
#[test]
fn bash_allows_unrelated_notion_substring() {
    for command in [
        "curl https://notion.so.example.org/x",
        "curl https://slack.com.evil/x",
        "curl https://mynotion.so/x",
        "curl https://notslack.com/x",
        "echo notional",
        "grep -r notion src",
        "grep slack notes.md",
    ] {
        assert_allows(command);
    }
}

/// #8454 S3 (Architect ruling 3): the read-only operator diagnostic, by path
/// and behind an env assignment, is allowed.
#[test]
fn bash_allows_gchat_mcp_doctor_offline() {
    for command in [
        "gchat-mcp doctor --offline",
        "~/.cargo/bin/gchat-mcp doctor --offline",
        "env RUST_LOG=debug gchat-mcp doctor --offline",
    ] {
        assert_allows(command);
    }
}

/// #8454 S3 (plan §5): a command the guard cannot lex or nest through is
/// allowed when its text holds no family token.
#[test]
fn bash_allows_unlexable_without_family_token() {
    for command in [
        "echo 'unclosed".to_owned(),
        "grep foo \"x".to_owned(),
        "echo $(ls".to_owned(),
        "echo $\"hello\"".to_owned(),
        nested("ls", 40),
    ] {
        assert_allows(&command);
    }
}

/// #8454 S3: the bare `googleapis.com` apex and its other services are not a
/// family host.
#[test]
fn bash_allows_googleapis_storage() {
    for command in [
        "curl https://storage.googleapis.com/bucket/object",
        "curl https://googleapis.com",
        "curl https://www.googleapis.com/oauth2/v3/tokeninfo",
    ] {
        assert_allows(command);
    }
}

/// #8454 S3 (Architect §11): a family program word used as an ARGUMENT is
/// allowed — only the command position or a family host denies. The
/// interpreter scan reads `-c`/`-e` text only, never a script operand.
#[test]
fn bash_allows_family_words_as_arguments() {
    for command in [
        "grep mail file",
        "git log --grep mail",
        "grep -r gws .",
        "echo http",
        "echo sendmail",
        "cat mail.txt",
        "cargo build -p slack-mcp",
        "command -v sendmail",
        "python3 smtplib_check.py",
        "python3 -m pytest -k smtplib",
        "tm hook --egress-guard",
        "tm doctor",
    ] {
        assert_allows(command);
    }
}

/// #8454 S3 documented limit: a host built from a variable or a command
/// substitution is not detected. This is an accepted residual (plan §4,
/// items 1 and 7); the test pins today's allow outcome.
#[test]
fn bash_variable_built_host_is_not_detected() {
    for command in [
        "H=slack; curl https://hooks.$H.com/services/x",
        "curl \"https://hooks.$(printf sl)ack.com/services/x\"",
    ] {
        assert_allows(command);
    }
}

/// #8454 S3 documented limit: a script run by path is not read. This is an
/// accepted residual (plan §4, item 3, the #8879 class); the test pins
/// today's allow outcome.
#[test]
fn bash_script_by_path_is_not_detected() {
    for command in [
        "./notify.sh",
        "python3 send_report.py",
        "bash ./scripts/post.sh",
    ] {
        assert_allows(command);
    }
}

/// #8454 S3 documented limit: an encoded payload is not decoded. This is an
/// accepted residual (plan §4, item 2); the test pins today's allow outcome.
#[test]
fn bash_base64_payload_is_not_detected() {
    assert_allows("echo c2VuZG1haWwgYm9i | base64 -d | sh");
}

/// #8454 S3 documented limit (PM ruling, critic round 1): a host or program
/// the shell or curl constructs by brace expansion or a glob is not detected.
/// This folds into residual 1; the test pins today's allow outcome.
#[test]
fn bash_shell_constructed_name_is_not_detected() {
    for command in [
        "curl https://hooks.slack.co{m,}/x",
        "curl 'https://hooks.slack.{com}/x'",
        "{sendmail,bob@x}",
        "/usr/sbin/sendmai?",
    ] {
        assert_allows(command);
    }
}

/// #8454 S3 documented limits: residuals 4 (aliases, functions, rc files),
/// 5 (self-configured tools), 6 (DNS, IP-literal or shortener fronting) and
/// 8 (Unicode confusables), plus a program run by `find -exec`, `ssh`,
/// `watch` or `parallel` and a `$(…)` in an unquoted here-document fed to a
/// non-shell. The test pins today's allow outcome for each.
#[test]
fn bash_other_residuals_are_not_detected() {
    let confusable = format!(
        "curl https://hooks.{}lack.com/x",
        char::from_u32(0x455).expect("Cyrillic dze is a char")
    );
    for (residual, command) in [
        ("4 alias", "alias sm=sendmail; sm bob".to_owned()),
        ("4 rc file", ". ~/.notifyrc; notify bob".to_owned()),
        ("5 gh extension", "gh notify-team hi".to_owned()),
        ("5 rclone", "rclone copy f team-remote:".to_owned()),
        ("5 aws sns", "aws sns publish --message hi".to_owned()),
        ("5 gcloud", "gcloud pubsub topics publish t".to_owned()),
        ("6 IP literal", "curl https://34.202.13.4/api/x".to_owned()),
        ("6 shortener", "curl https://bit.ly/abc123".to_owned()),
        ("8 confusable", confusable),
        ("find -exec", "find . -exec sendmail bob \\;".to_owned()),
        ("ssh", "ssh host sendmail bob".to_owned()),
        ("watch", "watch sendmail bob".to_owned()),
        ("parallel", "parallel sendmail ::: bob".to_owned()),
        ("heredoc $(…)", "cat <<EOF\n$(sendmail bob)\nEOF".to_owned()),
    ] {
        assert_eq!(bash(&command), Verdict::Allow, "{residual}: `{command}`");
    }
}
