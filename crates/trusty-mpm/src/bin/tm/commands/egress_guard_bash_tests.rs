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
/// Critic round 2 (CRITICAL): the lenient decode reads `\u`/`\U` as bash does,
/// and a NUL (`\0`, `\c@`) ends the quote's text.
#[test]
fn bash_egress_denies_ansi_c_beside_an_undecodable_quote() {
    assert_denies(
        "curl $'https://hooks.\\x73lack.com/services/x' -d $'hi\\n'",
        "Slack",
    );
    assert_denies("$'\\x73endmail' bob $\"x\"", "SMTP mail");
    assert_denies("$'\\u0073endmail' bob", "SMTP mail");
    assert_denies("curl https://hooks.$'\\U00000073'lack.com/x", "Slack");
    assert_denies("$'\\x73endmail\\0' bob", "SMTP mail");
    assert_denies("$'\\x73endmail\\c@' bob", "SMTP mail");
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
        // Critic round 2: `coproc` and `time`, with `time -p` still read.
        ("coproc sendmail bob </dev/null", "SMTP mail"),
        ("time ! sendmail bob", "SMTP mail"),
        ("time -p sendmail bob", "SMTP mail"),
    ] {
        assert_denies(command, service);
    }
}

/// 🔴 #8454 S3 critic round 2 (HIGH): keyword nesting is followed only to
/// `MAX_SUBSTITUTION_DEPTH`; past it the command is judged raw, so deep
/// nesting neither overflows the stack nor hides a program.
#[test]
fn bash_egress_denies_keyword_nesting_past_the_depth_bound() {
    // Past the bound the raw scan reads every word, an argument included.
    let past_bound = format!("{}echo sendmail", "eval if ".repeat(40));
    assert_denies(&past_bound, "SMTP mail");
    let evals = format!("{}sendmail bob", "eval if ".repeat(10_000));
    assert_denies(&evals, "SMTP mail");
    let braces = format!(
        "{}sendmail bob{}",
        "{ ".repeat(10_000),
        "; }".repeat(10_000)
    );
    assert_denies(&braces, "SMTP mail");
    // Each keyword level re-maps with the walk's own bound: the product holds.
    let product = format!(
        "{}sendmail bob",
        format!("{}if ", "eval ".repeat(31)).repeat(40)
    );
    assert_denies(&product, "SMTP mail");
}

/// 🔴 #8454 S3 critic round 3 (HIGH): `time eval S` reads `S` twice — once
/// past the keyword, once as the `eval` wrapper's inner command — so the work
/// doubles per level. A run past the hook timeout is an allow, so the guard
/// must reach its deny within a bounded budget. The keyword cases this must
/// not disturb stay in `bash_egress_denies_program_behind_a_shell_keyword`
/// and `bash_egress_denies_keyword_nesting_past_the_depth_bound`.
#[test]
fn bash_egress_bounds_work_for_time_eval_nesting() {
    let command = format!("{}sendmail bob", "time eval ".repeat(32));
    let (send, recv) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(bash(&command));
    });
    match recv.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(Verdict::Deny { check, reason }) => {
            assert_eq!(check, BASH_CHECK, "{reason}");
            assert!(reason.contains("it reaches SMTP mail ("), "{reason}");
        }
        Ok(Verdict::Allow) => panic!("`time eval`×32 + `sendmail bob` must be denied"),
        Err(err) => panic!("`time eval`×32 was not judged within 2 s: {err}"),
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
        ("find -exec", "find . -exec sendmail bob {} +".to_owned()),
        ("ssh", "ssh host sendmail bob".to_owned()),
        ("watch", "watch sendmail bob".to_owned()),
        ("parallel", "parallel sendmail ::: bob".to_owned()),
        ("heredoc $(…)", "cat <<EOF\n$(sendmail bob)\nEOF".to_owned()),
    ] {
        assert_eq!(bash(&command), Verdict::Allow, "{residual}: `{command}`");
    }
}

/// The bound on a large-input verdict (#9699): generous for a debug build on a
/// loaded host, and below the seconds a quadratic or unbounded walk takes.
const LARGE_INPUT_BOUND: std::time::Duration = std::time::Duration::from_secs(5);

/// The verdict on `command`, which must arrive within `bound`: Claude Code
/// kills a hook at its timeout, and a killed hook is an allow (#9699).
fn bash_within(command: String, bound: std::time::Duration) -> Verdict {
    let shown: String = command.chars().take(48).collect();
    let (send, recv) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(bash(&command));
    });
    recv.recv_timeout(bound)
        .unwrap_or_else(|err| panic!("`{shown}…` was not judged within {bound:?}: {err}"))
}

/// Assert `verdict`, on the command shown as `what`, is an `egress-bash` deny
/// whose reason contains `needle`.
fn assert_deny_with(verdict: Verdict, what: &str, needle: &str) {
    match verdict {
        Verdict::Deny { check, reason } => {
            assert_eq!(check, BASH_CHECK, "{what}: {reason}");
            assert!(
                reason.contains(needle),
                "{what} must name `{needle}`: {reason}"
            );
        }
        Verdict::Allow => panic!("{what} must be denied (`{needle}`)"),
    }
}

/// `echo` and `filler` repeated to exactly `len` bytes; it names no channel.
fn padded(len: usize, filler: &str) -> String {
    let mut command = "echo ".to_owned();
    while command.len() + filler.len() <= len {
        command.push_str(filler);
    }
    command.push_str(&"a".repeat(len - command.len()));
    command
}

/// 🔴 #9699 (b): a command over the 64 KiB cap is denied whole and the reason
/// names the cap, though it names no channel. A family token past the cap
/// denies the same way: the command is never scanned in part.
#[test]
fn bash_egress_denies_a_command_over_the_byte_cap_9699() {
    for command in [
        padded(65_537, "a"),
        padded(256 * 1024, "ab "),
        format!("{} sendmail bob", padded(70_000, "ab ")),
    ] {
        let what = format!("a {}-byte command", command.len());
        assert_deny_with(bash_within(command, LARGE_INPUT_BOUND), &what, "64 KiB");
    }
}

/// 🔴 #9699: the cap is inclusive. 65 536 bytes is read as any command is;
/// 65 537 bytes is denied.
#[test]
fn bash_egress_byte_cap_boundary_is_65536_bytes_9699() {
    assert_eq!(bash(&padded(65_536, "a")), Verdict::Allow, "65 536 bytes");
    assert_deny_with(bash(&padded(65_537, "a")), "65 537 bytes", "64 KiB");
}

/// #9699 control: a ~60 KiB here-document of plain prose, under the cap and
/// naming no channel, is allowed within the bound.
#[test]
fn bash_allows_a_60_kib_heredoc_9699() {
    let mut body = String::new();
    for n in 0.. {
        if body.len() >= 60 * 1024 {
            break;
        }
        body.push_str(&format!(
            "line {n}: the cache (target dir) keeps it's state; see notes\n"
        ));
    }
    let command = format!("cat > notes.md <<'EOF'\n{body}EOF");
    assert!(
        (60 * 1024..=65_536).contains(&command.len()),
        "{}",
        command.len()
    );
    assert_eq!(bash_within(command, LARGE_INPUT_BOUND), Verdict::Allow);
}

/// 🔴 #9699 (a): once the walk gives up — budget spent, nesting too deep, a
/// segment that will not lex, an undecodable `$"…"` — the raw scan also reads
/// the text with its quotes removed, so a name spliced from quoted pieces
/// still denies. The `$'…'` row was already denied; it pins the decode path.
#[test]
fn bash_egress_denies_quote_split_names_in_the_raw_scan_9699() {
    let nest = |inner: &str| format!("{}{inner}{}", "$(".repeat(33), ")".repeat(33));
    for (command, service) in [
        (
            format!("{}se''ndmail bob", "time eval ".repeat(11)),
            "SMTP mail",
        ),
        (nest("se''ndmail bob"), "SMTP mail"),
        (format!("$\"x\"; {}", nest("se''ndmail bob")), "SMTP mail"),
        (r#"cu""rl hooks.sl''ack.com 'unclosed"#.to_owned(), "Slack"),
        ("curl 's'lack.com/api 'unclosed".to_owned(), "Slack"),
        (r"se\ndmail bob 'unclosed".to_owned(), "SMTP mail"),
        (r"curl $'\x73'lack.com/x 'unclosed".to_owned(), "Slack"),
    ] {
        let what = format!("`{}`", command.chars().take(48).collect::<String>());
        let needle = format!("it reaches {service} (");
        assert_deny_with(bash_within(command, LARGE_INPUT_BOUND), &what, &needle);
    }
}

/// #9699 control: removing quotes joins only the pieces a quote joins; words
/// the shell keeps apart stay apart in the raw scan. The S3 argument rulings
/// hold on the walk.
#[test]
fn bash_allows_quote_separated_words_in_the_raw_scan_9699() {
    for command in [
        r#"echo "se" "ndmail" 'unclosed"#,
        "echo 'mai' 'lx' \"unclosed",
        "grep mail file",
        "git log --grep mail",
        "echo gws http mail",
    ] {
        assert_allows(command);
    }
}

/// 🔴 #9699 (c): an open quote and tens of thousands of `(` sent the shared
/// `lift` pass quadratic. Near the cap the command is judged within the
/// bound, and the program behind the parens still denies.
#[test]
fn bash_egress_judges_an_open_quote_and_many_parens_within_bound_9699() {
    let command = format!("echo 'x {} sendmail bob", "(".repeat(65_000));
    assert!(command.len() <= 65_536, "{}", command.len());
    let verdict = bash_within(command, LARGE_INPUT_BOUND);
    assert_deny_with(verdict, "`echo 'x ((…`", "it reaches SMTP mail (");
}

/// #9699 (b): the walk's budget counts calls, not bytes. Under the cap its
/// worst case — `time eval` doubling the reads of a near-cap payload — is
/// still judged within the bound.
#[test]
fn bash_egress_judges_a_near_cap_time_eval_payload_within_bound_9699() {
    let prefix = "time eval ".repeat(12);
    let command = format!("{prefix}{}", padded(65_536 - prefix.len(), "ab "));
    assert_eq!(command.len(), 65_536);
    assert_eq!(bash_within(command, LARGE_INPUT_BOUND), Verdict::Allow);
}
