//! Unit tests for the #9791 rule (`pm_guard_secret_file_list.rs`).
//!
//! The deny rows go through [`file_list_flag_text`] and through
//! `evaluate_secret_file_read`, the entry `pm_guard` calls, so a row proves
//! both the rule and its wiring. The text-slot rows check the new rule only:
//! `git commit -m '… .env'` already denies under #7266.

use super::*;
use crate::commands::pm_guard_secret_read::evaluate_secret_file_read;

/// The unified secret verdict for a Bash `command`.
fn secret(command: &str) -> Option<String> {
    let input = serde_json::json!({ "command": command });
    evaluate_secret_file_read("Bash", Some(&input))
}

/// Every `(command, secret word)` row denies through the rule and the entry,
/// with the `secret-file-read` label, `file -f` and the secret word in the
/// reason.
fn assert_denied(rows: &[(&str, &str)]) {
    let mut wrong = Vec::new();
    for (command, word) in rows {
        let ok = |reason: &Option<String>| {
            reason.as_deref().is_some_and(|r| {
                r.starts_with("secret-file-read:")
                    && r.contains("`file -f`/`-m`")
                    && r.contains(&format!("`{word}`"))
            })
        };
        let rule = file_list_flag_text(command);
        let entry = secret(command);
        if !ok(&rule) || !ok(&entry) {
            wrong.push((*command, rule, entry));
        }
    }
    assert!(wrong.is_empty(), "expected a #9791 DENY: {wrong:#?}");
}

/// Every row is allowed by the rule and by the unified entry.
fn assert_allowed(commands: &[&str]) {
    let denied: Vec<(&str, Option<String>, Option<String>)> = commands
        .iter()
        .map(|c| (*c, file_list_flag_text(c), secret(c)))
        .filter(|(_, rule, entry)| rule.is_some() || entry.is_some())
        .collect();
    assert!(denied.is_empty(), "expected ALLOW: {denied:#?}");
}

/// 🔴 REGRESSION (#9791): each list-reading flag on a secret-bearing file.
/// Allowed on 59fff9bc7e, because `file` is a safe-handling verb by name.
#[test]
fn the_list_flags_deny_on_a_secret_file() {
    assert_denied(&[
        ("file -f .env", ".env"),
        ("file --files-from .env", ".env"),
        ("file --files-from=.env", ".env"),
        ("file -bf .env", ".env"),
        ("file -fb .env", ".env"),
        ("file -f.env", ".env"),
        ("file -m .env", ".env"),
        ("file --magic-file .env", ".env"),
        ("file --magic-file=.env", ".env"),
        ("file -bm .env", ".env"),
        ("file -f apps/web/.env.local", "apps/web/.env.local"),
        ("file -f terraform.tfvars", "terraform.tfvars"),
        ("file -f ~/.ssh/id_rsa", "~/.ssh/id_rsa"),
    ]);
}

/// 🔴 REGRESSION (#9791): the call wrapped, chained, piped, redirected or
/// spelled with quote and backslash joins.
#[test]
fn wrappers_redirections_and_quote_joins_still_deny() {
    assert_denied(&[
        ("FILE -f .env", ".env"),
        ("/usr/bin/file -f .env", ".env"),
        ("sudo file -f .env", ".env"),
        ("env LC_ALL=C file -f .env", ".env"),
        ("file -f '.en''v'", ".env"),
        ("file -f .e\\nv", ".env"),
        ("ls && file -f .env", ".env"),
        ("file -f .env | head", ".env"),
        ("file -f .env 2>&1", ".env"),
        ("file -f - < .env", ".env"),
        ("bash -c \"file -f .env\"", ".env"),
        ("bash <<< 'file -f .env'", ".env"),
        // #9791: a backslash-newline line continuation is ordinary wrapping.
        ("file -f \\\n.env", ".env"),
        ("file \\\n  --files-from .env", ".env"),
    ]);
}

/// #9791: plain `file` on a secret file, and the list flags on a file that
/// is not secret-bearing, keep today's allow.
#[test]
fn plain_file_and_non_secret_lists_allow() {
    assert_allowed(&[
        "file .env",
        "file -b .env",
        "file -L id_rsa",
        "file -i .env",
        "file --mime-type .env",
        "file -F: .env",
        "file README.md",
        "file -f names.txt",
        "file -m magic.mgc src/main.rs",
    ]);
}

/// #9791: a quoted prose slot naming the call is text, not a call. The new
/// rule alone is checked: `git commit -m '… .env'` already denies (#7266).
#[test]
fn quoted_text_slots_are_exempt() {
    for command in [
        "gh issue comment 9791 --body \"file -f .env prints the lines\"",
        "gh issue comment 9791 --body='file --files-from .env'",
        "gh pr create --title \"file -f .env\" --body \"x\"",
        "git commit -m \"docs: file -f .env\"",
    ] {
        assert_eq!(file_list_flag_text(command), None, "{command}");
    }
}

/// #9791 ruling Q1: a lone `echo`/`printf` of prose naming the call is not a
/// slot, so it denies, and the reason names the `--body-file` route.
#[test]
fn a_lone_echo_of_the_call_denies_with_the_body_file_hint() {
    for command in [
        "echo 'run file -f .env to see it'",
        "printf '%s\\n' 'file -f .env'",
    ] {
        let reason = file_list_flag_text(command).unwrap_or_default();
        assert!(reason.contains("`--body-file`"), "{command}: {reason}");
    }
}

/// #9791: the siblings the rule leaves alone already deny under #7266, and
/// keep denying.
#[test]
fn the_sibling_list_readers_stay_denied() {
    for command in [
        "grep -f .env src/",
        "grep --file=.env x",
        "egrep -f .env x",
        "rg -f .env",
        "xargs -a .env echo",
        "comm .env other",
        "sort -m .env",
    ] {
        assert!(secret(command).is_some(), "`{command}` must deny");
    }
}
