//! Keep credential VALUES out of every generated launchd plist (#8236).
//!
//! Why: `~/Library/LaunchAgents/*.plist` is a world-readable file (`0644` by
//! launchd convention, and `LaunchdConfig::install` never tightens it). A
//! credential written into its `EnvironmentVariables` dict is therefore
//! readable by every process running as the user, lands in every Time Machine
//! backup, and is printed in full by the `plutil -p` any agent runs while
//! hunting for a log path — which is exactly how #8236 was found. The
//! workspace already ships a credential path that is not world-readable
//! (`credentials::resolve_env_var_bounded`: process env, then `.env.local`,
//! then a `0600` file store or the Keychain), so a plaintext plist entry is
//! never the only way to configure a daemon, only the most exposed one.
//!
//! What: the detection half is [`is_credential_env_key`] — REGISTRY membership
//! first, then a `_`-delimited suffix heuristic as a second net — and
//! [`looks_like_credential_value`] (a value carrying a well-known vendor
//! prefix, for `ProgramArguments`, where there is no key to read). The acting
//! half is [`strip_credential_env`], which `crate::launchd::LaunchdConfig::render_plist`
//! applies to every unit it renders, and the `plist` submodule (private; its
//! items are re-exported below), which reads and rewrites an
//! ALREADY-INSTALLED plist so `tm doctor --fix` can remediate a host without
//! reinstalling anything.
//!
//! Nothing here ever returns, logs, or formats a credential value: findings
//! are reported as KEY NAMES, and the one type that carries a value —
//! [`PlistSecret`] — cannot be printed. See `crate::credentials::redact` for
//! the masking used where a value genuinely has to be named.
//!
//! Deliberately NOT gated behind the `credentials` feature, and holding no
//! dependency on it: `launchd` is unconditional, and a guard that compiles out
//! under some feature set is not a guard. The registry it consults lives in
//! the equally unconditional [`crate::credential_registry`] for that reason.
//!
//! Test: `launchd_secrets/tests.rs`.
//!
//! [`is_credential_env_key`]: crate::launchd_secrets::is_credential_env_key
//! [`looks_like_credential_value`]: crate::launchd_secrets::looks_like_credential_value
//! [`strip_credential_env`]: crate::launchd_secrets::strip_credential_env
//! [`PlistSecret`]: crate::launchd_secrets::PlistSecret

mod plist;

pub use plist::{
    PlistCredentialEntry, PlistScrubError, PlistSecret, ScrubbedPlist, credential_entries,
    is_binary_plist, scrub_plist_credential_env, scrub_plist_keys,
};

/// Key suffixes whose value IS a credential.
///
/// Why: matched on `_`-delimited suffixes rather than substrings so
/// `MAX_TOKENS` and `TOKEN_BUDGET` — both real keys in this workspace — are not
/// swept up by a bare `contains("TOKEN")`. This is the SECOND net: a key the
/// registry already names is a credential before this table is consulted.
/// What: compared against the upper-cased key; a bare key equal to the suffix
/// (`TOKEN`, `PASSWORD`) matches too.
/// Test: `credential_keys_are_detected`, `tunable_keys_are_not_credentials`.
const CREDENTIAL_KEY_SUFFIXES: &[&str] = &[
    "TOKEN",
    "API_KEY",
    "APIKEY",
    "SECRET",
    "SECRET_KEY",
    "ACCESS_KEY",
    "PRIVATE_KEY",
    "PASSWORD",
    "PASSPHRASE",
    "CREDENTIAL",
    "CREDENTIALS",
];

/// Key suffixes that name a POINTER to a credential, never the value.
///
/// Why: `TRUSTY_BUGREPORT_GH_APP_KEY_FILE` holds a path and
/// `AWS_ACCESS_KEY_ID` holds an identifier. Stripping either from a plist
/// would break the daemon while protecting nothing, and a guard that breaks
/// working installs gets turned off.
/// What: checked BEFORE [`CREDENTIAL_KEY_SUFFIXES`], so the pointer reading
/// wins on any key that could be read both ways. It does NOT override the
/// registry: a name the registry declares a credential is one.
/// Test: `credential_reference_keys_are_not_credentials`.
const REFERENCE_KEY_SUFFIXES: &[&str] = &[
    "FILE", "PATH", "DIR", "URL", "URI", "ENV", "NAME", "ID", "ENABLED", "BUDGET", "LIMIT", "TTL",
    "SECS", "MODE", "SOURCE",
];

/// Vendor prefixes that mark a bare string as a credential.
///
/// Why: a `ProgramArguments` entry has no key to read, so the only signal left
/// is the value itself. These are the issuer-assigned prefixes, not a guess at
/// entropy — a heuristic that guessed would strip an argv the daemon needs.
/// Test: `credential_values_are_detected_by_prefix`.
const CREDENTIAL_VALUE_PREFIXES: &[&str] = &[
    "sk-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xapp-",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "AKIA",
    "ASIA",
];

/// Shortest string [`looks_like_credential_value`] will call a credential.
///
/// Why: a literal `sk-` or a redacted `ghp_…` placeholder carries the prefix
/// and no secret. Every real token of these families is far longer, so the
/// floor costs no coverage and stops the argv check firing on documentation.
const MIN_CREDENTIAL_VALUE_LEN: usize = 20;

/// Does `key` name a credential VALUE?
///
/// Why: the one predicate every writer and every diagnostic here shares, so a
/// plist the renderer refuses to write and a plist `tm doctor` flags can never
/// disagree about what counts.
/// What: two nets, in order. A key registered in
/// [`crate::credential_registry::REGISTRY`] is a credential by DECLARATION and
/// returns `true` immediately — no heuristic gets a chance to disagree with the
/// table the resolver itself uses. Otherwise the key is upper-cased, rejected
/// for any [`REFERENCE_KEY_SUFFIXES`] ending (a path/id pointing AT a
/// credential), and accepted when it is, or ends in `_` plus, one of
/// [`CREDENTIAL_KEY_SUFFIXES`].
/// Test: `credential_keys_are_detected`, `tunable_keys_are_not_credentials`,
/// `credential_reference_keys_are_not_credentials`,
/// `every_registry_name_is_detected_as_a_credential`.
///
/// # Code Contract
/// Preconditions:
/// - None. Every `&str` is accepted, including the empty string.
///
/// Postconditions:
/// - Pure and total: no I/O, no panic, no allocation of the input's content
///   beyond one upper-cased copy.
/// - Every registered credential env var returns `true`.
/// - A key matching a reference suffix and absent from the registry is NEVER
///   reported as a credential, whatever else it ends in.
#[must_use]
pub fn is_credential_env_key(key: &str) -> bool {
    // #8236: registry first — a declared credential never depends on a suffix
    // heuristic agreeing with the table `resolve_env_var_bounded` reads.
    if crate::credential_registry::is_registered_credential_env_var(key) {
        return true;
    }
    let upper = key.trim().to_ascii_uppercase();
    if upper.is_empty() {
        return false;
    }
    if REFERENCE_KEY_SUFFIXES
        .iter()
        .any(|s| suffix_matches(&upper, s))
    {
        return false;
    }
    CREDENTIAL_KEY_SUFFIXES
        .iter()
        .any(|s| suffix_matches(&upper, s))
}

/// Is `upper` exactly `suffix`, or does it end in `_` + `suffix`?
///
/// Why: the `_` boundary is the whole point — see [`CREDENTIAL_KEY_SUFFIXES`].
/// Test: covered through [`is_credential_env_key`]'s tests.
fn suffix_matches(upper: &str, suffix: &str) -> bool {
    if upper == suffix {
        return true;
    }
    upper
        .strip_suffix(suffix)
        .is_some_and(|head| head.ends_with('_'))
}

/// Does `value` carry a known credential prefix?
///
/// Why: used where there is no key — a `ProgramArguments` string. See
/// [`CREDENTIAL_VALUE_PREFIXES`] for why this is a prefix table and not an
/// entropy heuristic.
/// What: `true` when the trimmed value is at least
/// [`MIN_CREDENTIAL_VALUE_LEN`] bytes AND starts with a known prefix, or has
/// the Telegram bot-token shape (`<digits>:<30+ token chars>`).
/// Test: `credential_values_are_detected_by_prefix`,
/// `credential_values_are_detected_for_telegram_shape`,
/// `ordinary_arguments_are_not_credential_values`.
#[must_use]
pub fn looks_like_credential_value(value: &str) -> bool {
    let value = value.trim();
    if value.len() < MIN_CREDENTIAL_VALUE_LEN {
        return false;
    }
    if CREDENTIAL_VALUE_PREFIXES
        .iter()
        .any(|p| value.starts_with(p))
    {
        return true;
    }
    is_telegram_bot_token_shape(value)
}

/// The Telegram bot-token shape: `<6..=12 digits>:<30+ token characters>`.
///
/// Why: the second credential #8236 found had no vendor prefix at all, so the
/// prefix table alone would have missed it. Hand-rolled rather than pulled in
/// as a regex — this module is unconditional and owes its dependents no new
/// crate.
/// Test: `credential_values_are_detected_for_telegram_shape`.
fn is_telegram_bot_token_shape(value: &str) -> bool {
    let Some((id, secret)) = value.split_once(':') else {
        return false;
    };
    (6..=12).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_digit())
        && secret.len() >= 30
        && secret
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Remove every credential-keyed pair from `pairs`, returning the keys removed.
///
/// Why: this is the acting half of the guard at the single choke point
/// `crate::launchd::LaunchdConfig::render_plist`. Dropping the pair rather
/// than failing the render is deliberate: a render that failed would abort
/// `service install` on exactly the hosts that most need the plist rewritten,
/// and the rewritten plist IS the remediation.
/// What: retains pairs whose key [`is_credential_env_key`] rejects, in order;
/// returns the removed KEYS, never their values.
/// Test: `strip_credential_env_removes_only_the_credential_pair`,
/// `strip_credential_env_is_a_noop_when_clean`.
///
/// # Code Contract
/// Postconditions:
/// - No element of the returned `Vec` is a credential value — only key names.
/// - After the call, `pairs.iter().all(|(k, _)| !is_credential_env_key(k))`.
/// - The relative order of the retained pairs is unchanged.
pub fn strip_credential_env(pairs: &mut Vec<(String, String)>) -> Vec<String> {
    let mut removed = Vec::new();
    pairs.retain(|(key, _)| {
        if is_credential_env_key(key) {
            removed.push(key.clone());
            false
        } else {
            true
        }
    });
    debug_assert!(
        pairs.iter().all(|(k, _)| !is_credential_env_key(k)),
        "strip_credential_env left a credential-keyed pair behind"
    );
    removed
}

/// Names of the credential-bearing `EnvironmentVariables` keys in `xml`.
///
/// Why: the read-only half, for a diagnostic that reports without writing.
/// What: [`scrub_plist_credential_env`]'s `keys`, discarding the rewrite.
///
/// # Errors
///
/// As [`scrub_plist_credential_env`].
///
/// Test: `plist_credential_env_keys_names_the_key`.
pub fn plist_credential_env_keys(xml: &str) -> Result<Vec<String>, PlistScrubError> {
    Ok(scrub_plist_credential_env(xml)?.keys)
}

#[cfg(test)]
mod tests;
