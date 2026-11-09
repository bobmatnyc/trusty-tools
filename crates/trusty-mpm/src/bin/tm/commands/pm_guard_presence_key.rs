//! The presence-key Keychain floor of `tm hook --pm-guard` (#8878 PR2a).
//!
//! Why: the grant key is the login-Keychain item [`KEYCHAIN_SERVICE`] /
//! [`KEYCHAIN_ACCOUNT`], and `KeySource::load` takes whatever item answers to
//! that service. A hooked caller that adds the item before Bob's first
//! `tm presence local`, deletes and re-adds it, or widens its access list
//! knows the key and can seal its own grants (critic HIGH,
//! `.claude/scratch-8878-critic/pr2a-verdict.md`). Bob Q9b ("assume only the
//! user can enter commands") makes every hooked caller the threat: PMs,
//! agents, subagents and the Architect itself.
//! What: [`evaluate_presence_key_write`] reads the text the way the
//! credential-print trigger does (quotes and backslashes removed, ASCII case
//! folded) and denies when it
//! 1. names the key item next to a Keychain writer: a `security` item-write
//!    verb ([`WRITE_VERBS`]) or a Keychain API call ([`API_WRITERS`]); or
//! 2. runs a `security` item-write verb whose words the shell chooses at run
//!    time (`$`, a backtick), other than the password value; or
//! 3. runs one with no literal `-s` service or `-a` account, which selects or
//!    creates an item the guard cannot tell from the key; or
//! 4. runs a `security` verb outside [`READ_ONLY_VERBS`]
//!    ([`judge_security_verbs`]), whatever item it names: every `security`
//!    word is read, not only one in command position.
//!
//! `pm_guard_floor` runs it on every path for every caller, and
//! `pm_guard_secret_script::presence_key` runs it over the body of every
//! script the command runs. Neither bypass lifts it.
//! Residual: a process outside any hook (Q9b: only Bob types there), a name
//! an interpreter's code assembles at run time (`base64`, `chr`), a command
//! name the shell builds (`S=security; $S …`, `/usr/bin/s?curity`), a script
//! the script walk cannot locate, and, through those, a Keychain search list
//! that puts another keychain first (`security list-keychains -s`); #10865
//! tracks the access-list check that closes the hooked ones.
//! Test: `pm_guard_presence_key_tests.rs`; on every path in
//! `pm_guard_floor_tests::presence_key_writes_bind_every_caller_8878`.

use trusty_mpm::core::presence_key::{KEYCHAIN_ACCOUNT, KEYCHAIN_SERVICE};

/// `security` verbs that add, delete or re-permission a generic password.
pub(crate) const WRITE_VERBS: &[&str] = &[
    "add-generic-password",
    "delete-generic-password",
    "set-generic-password-partition-list",
];

/// Keychain API names (case folded) that write or delete an item: the
/// Python `keyring` and Rust `keyring` calls, `security-framework`, and the
/// Security framework's C API.
pub(crate) const API_WRITERS: &[&str] = &[
    "set_password",
    "delete_password",
    "set_secret",
    "delete_credential",
    "set_generic_password",
    "delete_generic_password",
    "secitemadd",
    "secitemupdate",
    "secitemdelete",
    "seckeychainadd",
    "seckeychainitem",
];

/// Options whose value is the password itself, never an item selector.
const VALUE_OPTIONS: &[&str] = &["-w", "-X"];

/// Bytes that end one shell command.
const SEGMENT_END: &[char] = &[';', '|', '&', '\n', ')', '('];

/// The deny for `text` (a Bash command or a script body) that can write the
/// presence key item, or `None`.
///
/// Test: `every_spelling_naming_the_key_item_is_denied_8878`,
/// `a_run_time_word_in_a_security_write_is_denied_8878`,
/// `a_security_write_with_no_literal_selector_is_denied_8878`,
/// `other_keychain_items_and_reads_pass_8878`.
pub(crate) fn evaluate_presence_key_write(text: &str) -> Option<String> {
    finding(text).map(|what| {
        format!(
            "Hard-floor deny (#8878): this command {what}. The presence grant key is the \
             login-Keychain item `{KEYCHAIN_SERVICE}` / `{KEYCHAIN_ACCOUNT}`; a session that \
             adds, deletes or re-permissions it can seal its own presence grants, so no hooked \
             session may, the Architect included (owner ruling Q9b). `tm presence local` \
             creates the key itself. `TRUSTY_MPM_PM_UNRESTRICTED` / `TRUSTY_MPM_DISABLE_HOOKS` \
             do not lift this floor."
        )
    })
}

/// What `text` does to the key item, as a clause, or `None`.
fn finding(text: &str) -> Option<&'static str> {
    // #8878: the credential-print trigger's reading — quotes and backslashes
    // gone, so `"trusty-mpm".presence` and `["security", …]` read whole; a
    // `\`-newline joins its lines, as the shell does.
    let raw: String = text
        .replace("\\\n", "")
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .collect();
    let folded = raw.to_ascii_lowercase();
    let names_item = folded.contains(KEYCHAIN_SERVICE) || folded.contains(KEYCHAIN_ACCOUNT);
    let verbs: Vec<usize> = WRITE_VERBS
        .iter()
        .flat_map(|verb| folded.match_indices(verb).map(|(at, v)| at + v.len()))
        .collect();
    if names_item && (!verbs.is_empty() || API_WRITERS.iter().any(|w| folded.contains(w))) {
        return Some("writes the presence key item in the Keychain");
    }
    // A verb with no `security` word is prose or a search pattern
    // (`grep -rn delete-generic-password crates`).
    if !folded.contains("security") {
        return None;
    }
    // #8878: the verb itself, judged before the literal-verb rules below.
    if let Some(what) = judge_security_verbs(&folded) {
        return Some(what);
    }
    // `to_ascii_lowercase` keeps byte offsets, so `raw` keeps option case.
    verbs.into_iter().find_map(|end| {
        let rest = &raw[end..];
        let segment = rest.split(SEGMENT_END).next().unwrap_or_default();
        judge_security_args(segment)
    })
}

/// The only `security` verbs a hooked caller runs; each reads and writes
/// nothing (#8878 Architect ruling: an allowlist, not a denylist).
const READ_ONLY_VERBS: &[&str] = &[
    // Prints usage.
    "help",
    // Lists signing identities (`scripts/install-trusty-*-signed.sh`).
    "find-identity",
    // Prints public certificates.
    "find-certificate",
    // Read an item; printing its secret (`-w`, `-g`) is the credential-print rule's.
    "find-generic-password",
    "find-internet-password",
];

/// Bytes that end the word `security` as the shell reads it.
const NAME_END: &str = ",$`;|&<>()]";

/// Bytes that make a word one the shell builds at run time or expands.
const SHELL_BUILT: &[char] = &['$', '`', '*', '?', '[', '{'];

/// The verb of every `security` word in `folded`: denied unless it is a
/// complete literal from [`READ_ONLY_VERBS`].
///
/// Why: a denylist of write verbs missed every spelling that hid the verb —
/// built at run time, globbed, abbreviated (`security` runs a unique prefix),
/// or behind a redirection or `-p prompt` (#8878 r2 HIGH).
/// What: every `security` word, not only one in command position, because
/// `echo 'security …' | sh` puts it after any word. Its stage ends at a
/// newline or a word starting `;`, `|`, `&`, `)`, `]`, `` ` `` or `#`.
/// Redirections and their targets are skipped; an option word runs nothing
/// unless it holds `i` or `p` (interactive mode reads verbs from stdin).
/// The first other word must be an allowlisted verb. Two shapes pass because
/// `security` runs nothing for them: no verb (it prints usage and exits 2),
/// and a plain lowercase word that is no verb nor a verb's prefix
/// (`unknown command`) — so `git commit -m "harden security for X"` passes.
/// Test: `a_run_time_word_in_a_security_write_is_denied_8878`,
/// `a_verb_outside_the_read_only_allowlist_is_denied_8878`,
/// `other_keychain_items_and_reads_pass_8878`.
fn judge_security_verbs(folded: &str) -> Option<&'static str> {
    folded.match_indices("security").find_map(|(at, word)| {
        let before = folded[..at].chars().next_back();
        if before.is_some_and(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) {
            return None;
        }
        let rest = &folded[at + word.len()..];
        // #8878: `security>/dev/null`, `security$IFS…` and `[security, …]`
        // end the name; `security-framework` or `security.md` is another word.
        if rest.starts_with(|c: char| !c.is_whitespace() && !NAME_END.contains(c)) {
            return None;
        }
        judge_security_stage(rest.split('\n').next().unwrap_or_default())
    })
}

/// The words after one `security` word, up to its stage end.
fn judge_security_stage(stage: &str) -> Option<&'static str> {
    let mut words = stage
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty());
    while let Some(word) = words.next() {
        let ends = word.starts_with([';', '|', ')', ']', '`', '#'])
            || (word.starts_with('&') && !word.starts_with("&>"));
        if ends {
            return None;
        }
        // #8878: `2>/dev/null`, `2>&1`, `<<<x` skip themselves; `>`, `2>`,
        // `<<<` and `&>` also skip their target.
        let op = word.trim_start_matches(|c: char| c.is_ascii_digit());
        if op.starts_with(['<', '>']) || op.starts_with("&>") {
            if op.chars().all(|c| "<>&|-".contains(c)) {
                words.next();
            }
            continue;
        }
        // #8878: a word ends at a stage end or a backtick (`security
        // find-identity` in a comment); no allowlisted verb prefixes another,
        // so text glued after one only makes an unknown command.
        let verb = word
            .split([';', '|', '&', '<', '>', ')', ']', '`'])
            .next()?;
        // #8878: an option word is read whole; a substitution glued to `-q`
        // can add `p`.
        let judged = if word.starts_with('-') { word } else { verb };
        if judged.contains(SHELL_BUILT) {
            return Some("runs a `security` verb the shell builds at run time or expands");
        }
        if let Some(flags) = verb.strip_prefix('-') {
            // #8878: `-i` and `-p prompt` read verbs from stdin.
            if flags.contains(['i', 'p']) {
                return Some("runs `security` in interactive mode, which reads verbs from stdin");
            }
            continue;
        }
        let plain = verb.trim_end_matches(['.', ':']);
        let inert = !plain.is_empty()
            && plain.bytes().all(|b| b.is_ascii_lowercase())
            && !SECURITY_VERBS.iter().any(|known| known.starts_with(plain));
        return (!READ_ONLY_VERBS.contains(&verb) && !inert).then_some(
            "runs a `security` verb outside the read-only allowlist (find-generic-password, \
             find-internet-password, find-identity, find-certificate, help)",
        );
    }
    None
}

/// Every `security` subcommand (`security help`, macOS 26).
const SECURITY_VERBS: &[&str] = &[
    "help",
    "list-keychains",
    "list-smartcards",
    "default-keychain",
    "login-keychain",
    "create-keychain",
    "delete-keychain",
    "lock-keychain",
    "unlock-keychain",
    "set-keychain-settings",
    "set-keychain-password",
    "show-keychain-info",
    "dump-keychain",
    "create-keypair",
    "add-generic-password",
    "add-internet-password",
    "add-certificates",
    "find-generic-password",
    "delete-generic-password",
    "set-generic-password-partition-list",
    "find-internet-password",
    "delete-internet-password",
    "set-internet-password-partition-list",
    "find-key",
    "set-key-partition-list",
    "find-certificate",
    "find-identity",
    "delete-certificate",
    "delete-identity",
    "set-identity-preference",
    "get-identity-preference",
    "create-db",
    "export",
    "import",
    "export-smartcard",
    "cms",
    "install-mds",
    "add-trusted-cert",
    "remove-trusted-cert",
    "dump-trust-settings",
    "user-trust-settings-enable",
    "trust-settings-export",
    "trust-settings-import",
    "verify-cert",
    "authorize",
    "authorizationdb",
    "execute-with-privileges",
    "leaks",
    "error",
    "create-filevaultmaster-keychain",
    "smartcards",
    "translocate-policy-check",
    "translocate-status-check",
    "translocate-original-path",
    "requirement-evaluate",
    "filevault",
    "platformsso",
];

/// The words after a `security` write verb: denied when one is chosen at
/// run time or none selects an item the guard can tell from the key.
fn judge_security_args(segment: &str) -> Option<&'static str> {
    // An argv list reads `-s, svc]`: list punctuation is not part of a word.
    let words: Vec<&str> = segment
        .split_whitespace()
        .map(|w| w.trim_matches([',', '[', ']']))
        .filter(|w| !w.is_empty())
        .collect();
    let mut selector = false;
    let mut at = 0;
    while let Some(word) = words.get(at) {
        let value_option = VALUE_OPTIONS.iter().find(|o| word.starts_with(*o)).copied();
        if let Some(option) = value_option {
            // The password value may be a variable; it selects nothing.
            at += if *word == option { 2 } else { 1 };
            continue;
        }
        if word.contains(['$', '`']) {
            return Some("runs a Keychain item write whose words the shell chooses at run time");
        }
        if *word == "-s" || *word == "-a" {
            selector |= words.get(at + 1).is_some_and(|v| !v.contains(['$', '`']));
        } else if (word.starts_with("-s") || word.starts_with("-a")) && word.len() > 2 {
            selector = true;
        }
        at += 1;
    }
    (!selector).then_some(
        "runs a Keychain item write with no literal `-s` service or `-a` account, so it can \
         reach the presence key item",
    )
}

#[cfg(test)]
#[path = "pm_guard_presence_key_tests.rs"]
pub(crate) mod tests;
