//! Keep credential VALUES out of every generated launchd plist (#8236).
//!
//! Why: `~/Library/LaunchAgents/*.plist` is a world-readable file (`0644` by
//! launchd convention, and `LaunchdConfig::install` never tightens it). A
//! credential written into its `EnvironmentVariables` dict is therefore
//! readable by every process running as the user, lands in every Time Machine
//! backup, and is printed in full by the `plutil -p` any agent runs while
//! hunting for a log path — which is exactly how #8236 was found. The
//! workspace already ships a credential path that is not world-readable
//! (`credentials::resolve_key`: process env, then `.env.local`, then a `0600`
//! file store or the Keychain), so a plaintext plist entry is never the only
//! way to configure a daemon, only the most exposed one.
//!
//! What: the detection half is [`is_credential_env_key`] (a key that names a
//! credential VALUE, as opposed to a path or an id that merely points at one)
//! and [`looks_like_credential_value`] (a value carrying a well-known vendor
//! prefix, for `ProgramArguments`, where there is no key to read). The acting
//! half is [`strip_credential_env`], which [`crate::launchd::LaunchdConfig::render_plist`]
//! applies to every unit it renders, and [`scrub_plist_credential_env`], which
//! rewrites an ALREADY-INSTALLED plist so `tm doctor --fix` can remediate a
//! host without reinstalling anything.
//!
//! Nothing here ever returns, logs, or formats a credential value: findings
//! are reported as KEY NAMES. See `crate::credentials::redact` for the masking
//! used where a value genuinely has to be named.
//!
//! Deliberately NOT gated behind the `credentials` feature, and holding no
//! dependency on it: `launchd` is unconditional, and a guard that compiles out
//! under some feature set is not a guard.
//!
//! Test: `launchd_secrets/tests.rs`.

/// Key suffixes whose value IS a credential.
///
/// Why: matched on `_`-delimited suffixes rather than substrings so
/// `MAX_TOKENS` and `TOKEN_BUDGET` — both real keys in this workspace — are not
/// swept up by a bare `contains("TOKEN")`.
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
/// wins on any key that could be read both ways.
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
/// What: upper-cases `key`, returns `false` for any
/// [`REFERENCE_KEY_SUFFIXES`] ending (a path/id pointing AT a credential), and
/// otherwise `true` when the key is, or ends in `_` plus, one of
/// [`CREDENTIAL_KEY_SUFFIXES`].
/// Test: `credential_keys_are_detected`, `tunable_keys_are_not_credentials`,
/// `credential_reference_keys_are_not_credentials`.
///
/// # Code Contract
/// Preconditions:
/// - None. Every `&str` is accepted, including the empty string.
///
/// Postconditions:
/// - Pure and total: no I/O, no panic, no allocation of the input's content
///   beyond one upper-cased copy.
/// - A key matching a reference suffix is NEVER reported as a credential,
///   whatever else it ends in.
#[must_use]
pub fn is_credential_env_key(key: &str) -> bool {
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
/// [`crate::launchd::LaunchdConfig::render_plist`]. Dropping the pair rather
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

/// A plist that could not be read well enough to be judged (#8236).
///
/// Why: "there is no secret here" and "I could not tell" are different
/// answers, and collapsing them is how a scanner reports a compromised host as
/// clean. Every caller has to branch on this rather than defaulting to a pass.
/// What: one human-readable reason. Never carries plist content — the line of
/// a plist a parse failed on is exactly the line that could hold the secret.
/// Test: `scrub_reports_an_unterminated_environment_dict`,
/// `scrub_reports_a_key_with_no_value_element`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlistScrubError {
    /// What could not be read. Structural description only, never content.
    pub reason: String,
}

impl std::fmt::Display for PlistScrubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "could not parse the plist: {}", self.reason)
    }
}

impl std::error::Error for PlistScrubError {}

impl PlistScrubError {
    /// Build an error from a structural reason.
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

/// The result of scanning — and rewriting — one installed plist.
///
/// Why: the check and the repair ask the same question one call apart, and
/// answering both from one parse is what keeps `tm doctor` and
/// `tm doctor --fix` from disagreeing between the two reads.
/// What: `keys` names what was found (empty means clean) and `xml` is the
/// document with those entries removed (byte-identical to the input when
/// `keys` is empty).
/// Test: `scrub_removes_the_credential_entry_and_keeps_the_rest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubbedPlist {
    /// The credential-bearing `EnvironmentVariables` keys found, in document
    /// order. Never contains a value.
    pub keys: Vec<String>,
    /// The plist with those entries removed.
    pub xml: String,
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

/// Rewrite `xml` without its credential-bearing `EnvironmentVariables` entries.
///
/// Why: an existing install is not fixed by a renderer guard — the plaintext
/// is already on disk, and on a host whose daemon is not reinstalled it stays
/// there. This is the remediation `tm doctor --fix` applies in place.
/// What: locates the `EnvironmentVariables` dict, pairs each `<key>` with the
/// value element that follows it, and deletes the pairs whose key
/// [`is_credential_env_key`] accepts, preserving every other byte (including
/// whitespace and key order). A document with no such dict is returned
/// unchanged with no findings.
///
/// # Errors
///
/// [`PlistScrubError`] when the `EnvironmentVariables` dict is unterminated or
/// a key inside it has no value element. Both mean the scan could not see the
/// whole dict, so reporting "clean" would be a false negative on the one file
/// this module exists to judge.
///
/// Test: `scrub_removes_the_credential_entry_and_keeps_the_rest`,
/// `scrub_is_byte_identical_when_clean`,
/// `scrub_reports_an_unterminated_environment_dict`,
/// `scrub_reports_a_key_with_no_value_element`,
/// `scrub_reports_an_unterminated_key`.
pub fn scrub_plist_credential_env(xml: &str) -> Result<ScrubbedPlist, PlistScrubError> {
    let Some((start, end)) = env_dict_span(xml)? else {
        return Ok(ScrubbedPlist {
            keys: Vec::new(),
            xml: xml.to_string(),
        });
    };

    let mut keys = Vec::new();
    let mut drops: Vec<(usize, usize)> = Vec::new();
    let mut cursor = start;
    while let Some(key_open) = find_from(xml, "<key>", cursor, end) {
        let key_body = key_open + "<key>".len();
        let key_close = find_from(xml, "</key>", key_body, end)
            .ok_or_else(|| PlistScrubError::new("an EnvironmentVariables <key> is unterminated"))?;
        let key = xml[key_body..key_close].trim().to_string();
        let value_end = value_element_end(xml, key_close + "</key>".len(), end)?;
        if is_credential_env_key(&key) {
            keys.push(key);
            drops.push((key_open, swallow_trailing_newline(xml, value_end)));
        }
        cursor = value_end;
    }

    let mut out = xml.to_string();
    for (from, to) in drops.iter().rev() {
        out.replace_range(*from..*to, "");
    }
    Ok(ScrubbedPlist { keys, xml: out })
}

/// Byte span of the `EnvironmentVariables` dict's CONTENT, if the key is present.
///
/// Why: every scan and every deletion has to stay inside that one dict — a
/// `<key>TOKEN</key>` elsewhere in the document is not an environment variable
/// and deleting it would corrupt the unit.
/// What: finds `<key>EnvironmentVariables</key>`, then the `<dict>` that
/// follows, then its matching `</dict>` counting nested dicts. `Ok(None)` when
/// the key is absent.
///
/// # Errors
///
/// When the key is present but its dict never opens or never closes.
///
/// Test: `scrub_reports_an_unterminated_environment_dict`,
/// `scrub_reports_environment_variables_with_no_dict`,
/// `scrub_ignores_keys_outside_the_environment_dict`.
fn env_dict_span(xml: &str) -> Result<Option<(usize, usize)>, PlistScrubError> {
    let Some(key_at) = xml.find("<key>EnvironmentVariables</key>") else {
        return Ok(None);
    };
    let open = xml[key_at..]
        .find("<dict>")
        .map(|o| key_at + o + "<dict>".len())
        .ok_or_else(|| PlistScrubError::new("EnvironmentVariables is not followed by a <dict>"))?;

    let mut depth = 1usize;
    let mut cursor = open;
    while depth > 0 {
        let next_open = xml[cursor..].find("<dict>").map(|o| cursor + o);
        let next_close = xml[cursor..].find("</dict>").map(|o| cursor + o);
        match (next_open, next_close) {
            (Some(o), Some(c)) if o < c => {
                depth += 1;
                cursor = o + "<dict>".len();
            }
            (_, Some(c)) => {
                depth -= 1;
                if depth == 0 {
                    return Ok(Some((open, c)));
                }
                cursor = c + "</dict>".len();
            }
            _ => break,
        }
    }
    Err(PlistScrubError::new(
        "the EnvironmentVariables <dict> is unterminated",
    ))
}

/// End offset of the value element that follows a `</key>` at `from`.
///
/// Why: a plist value is `<string>…</string>` in every unit this workspace
/// generates, but a hand-edited one can carry `<data>`, `<integer>` or a
/// self-closing `<true/>`. Deleting a key without its value would leave the
/// dict malformed, so the pairing is resolved generically.
/// What: skips whitespace, reads the next element's tag name, and returns the
/// offset just past `</name>` (or just past `/>` for a self-closing element).
///
/// # Errors
///
/// When no element follows the key, or its closing tag is absent.
///
/// Test: `scrub_reports_a_key_with_no_value_element`,
/// `scrub_reports_a_value_element_with_no_closing_tag`,
/// `scrub_removes_a_self_closing_value`.
fn value_element_end(xml: &str, from: usize, end: usize) -> Result<usize, PlistScrubError> {
    let missing = || PlistScrubError::new("an EnvironmentVariables <key> has no value element");
    let open = find_from(xml, "<", from, end).ok_or_else(missing)?;
    let name_end = xml[open + 1..end]
        .find(['>', ' ', '/'])
        .map(|o| open + 1 + o)
        .ok_or_else(missing)?;
    let name = &xml[open + 1..name_end];
    let tag_close = find_from(xml, ">", name_end, end).ok_or_else(missing)?;
    if xml[open..=tag_close].ends_with("/>") {
        return Ok(tag_close + 1);
    }
    let closing = format!("</{name}>");
    find_from(xml, &closing, tag_close, end)
        .map(|o| o + closing.len())
        .ok_or_else(missing)
}

/// First occurrence of `needle` in `xml[from..end]`, as an absolute offset.
fn find_from(xml: &str, needle: &str, from: usize, end: usize) -> Option<usize> {
    if from >= end {
        return None;
    }
    xml[from..end].find(needle).map(|o| from + o)
}

/// Extend a deletion through the whitespace and one newline that follow it.
///
/// Why: deleting only the elements leaves a blank line where the entry was,
/// which shows up as spurious churn in every later diff of the plist.
/// Test: `scrub_removes_the_credential_entry_and_keeps_the_rest`.
fn swallow_trailing_newline(xml: &str, from: usize) -> usize {
    let mut at = from;
    for (offset, ch) in xml[from..].char_indices() {
        match ch {
            ' ' | '\t' | '\r' => at = from + offset + ch.len_utf8(),
            '\n' => return from + offset + 1,
            _ => break,
        }
    }
    at
}

#[cfg(test)]
mod tests;
