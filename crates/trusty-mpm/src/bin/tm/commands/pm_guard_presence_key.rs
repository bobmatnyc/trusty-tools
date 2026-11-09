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
//! 4. runs any `security` verb built at run time, globbed or abbreviated
//!    ([`judge_security_verbs`]), whatever item it names.
//!
//! `pm_guard_floor` runs it on every path for every caller, and
//! `pm_guard_secret_script::presence_key` runs it over the body of every
//! script the command runs. Neither bypass lifts it.
//! Residual: a process outside any hook (Q9b: only Bob types there), a name
//! an interpreter's code assembles at run time (`base64`, `chr`), a script
//! the script walk cannot locate, and a Keychain search list that puts
//! another keychain first (`security list-keychains -s`); #10865 tracks the
//! access-list check that closes the hooked ones.
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
    // gone, so `"trusty-mpm".presence` and `["security", …]` read whole.
    let raw: String = text
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

/// The subcommand word of every `security` word in `folded`: denied when the
/// shell builds it at run time or expands it as a glob, or when it abbreviates
/// a verb (`security` runs a unique prefix: `add-generic-pass`).
///
/// Why: the write rules read only a literal [`WRITE_VERBS`] entry, so
/// `security add-generic-pass$(echo word) -s …` passed them (#8878 delta HIGH).
/// What: skips `security`'s own `-` options, then reads one word. A word that
/// is neither a known verb nor a prefix of one passes: `security` rejects it,
/// so prose such as `git commit -m "harden security for X"` is not denied.
/// Test: `a_run_time_word_in_a_security_write_is_denied_8878`,
/// `other_keychain_items_and_reads_pass_8878`.
fn judge_security_verbs(folded: &str) -> Option<&'static str> {
    folded.match_indices("security").find_map(|(at, word)| {
        let before = folded[..at].chars().next_back();
        if before.is_some_and(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) {
            return None;
        }
        let rest = &folded[at + word.len()..];
        // #8878: an argv list reads `[security, verb, …]`.
        if !rest.starts_with(|c: char| c.is_whitespace() || c == ',') {
            return None;
        }
        let verb = rest
            .split(|c: char| c.is_whitespace() || c == ',')
            .find(|w| !w.is_empty() && !w.starts_with('-'))?
            .trim_end_matches([']', ')']);
        if verb.contains(['$', '`', '*', '?', '[', '{']) {
            return Some(
                "runs a `security` verb the shell builds at run time or expands as a glob",
            );
        }
        let abbreviates = |known: &&str| known.starts_with(verb) && **known != *verb;
        (!verb.is_empty()
            && !SECURITY_VERBS.contains(&verb)
            && SECURITY_VERBS.iter().any(abbreviates))
        .then_some("runs an abbreviated `security` verb, which the guard does not resolve")
    })
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
