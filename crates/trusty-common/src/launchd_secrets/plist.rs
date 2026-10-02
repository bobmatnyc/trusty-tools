//! Reading and rewriting the `EnvironmentVariables` dict of a launchd plist.
//!
//! Why: split out of `launchd_secrets.rs` by #8236 when `--fix` stopped being a
//! delete and became a MIGRATE — which means the parser now has to hand back
//! the VALUE it found, not just the key, and that is a materially different
//! hazard surface from the key/value heuristics next door. Keeping the two
//! apart keeps each file well inside the 500-line cap and puts every byte that
//! can hold a secret in one place.
//!
//! What: [`PlistSecret`] is the only type here that holds a value, and it
//! cannot be printed. [`credential_entries`] reads the credential-keyed pairs,
//! [`scrub_plist_credential_env`] removes all of them, and
//! [`scrub_plist_keys`] removes a NAMED SUBSET — the one `--fix` uses, so a key
//! whose migration was not confirmed stays in the file.
//! [`is_binary_plist`] answers before any of that runs.
//!
//! Test: `launchd_secrets/tests.rs`.

use super::is_credential_env_key;

/// Magic bytes at the head of every binary (`bplist`) property list.
///
/// Why: a binary plist read as text is either an encoding error or, worse,
/// lossy nonsense that the scanner finds no `<key>` in and reports CLEAN. A
/// false clean on the one file this module exists to judge is the failure that
/// must not happen, so the magic is checked before anything else.
/// Test: `a_binary_plist_is_detected_by_magic`.
const BINARY_PLIST_MAGIC: &[u8] = b"bplist00";

/// A credential value read out of a plist. Never printable.
///
/// Why (#8236 item 9): `--fix` has to carry the value from the file to the
/// credential store, so for that moment the value IS in memory. A
/// `#[derive(Debug)]` anywhere on the path from here to the store would put it
/// in a panic message, a `tracing` field, or a test failure. This type's
/// `Debug` and `Display` render a fixed placeholder, and [`Self::expose`] is
/// the single, greppable way to reach the bytes.
/// What: a newtype over `String` with no `Clone`-to-`String` escape hatch
/// other than `expose`.
/// Test: `plist_secret_never_renders_its_value`,
/// `entries_carry_the_value_only_inside_the_wrapper`.
#[derive(Clone, PartialEq, Eq)]
pub struct PlistSecret(String);

impl PlistSecret {
    /// Wrap a value read from a plist.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw value. Every call site is a deliberate disclosure.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Is the wrapped value empty?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for PlistSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PlistSecret(<redacted>)")
    }
}

impl std::fmt::Display for PlistSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// One credential-keyed `EnvironmentVariables` entry.
///
/// Why: `--fix` needs the key to decide where the value belongs and the value
/// to put there; the doctor row needs only the key. One type, and the value
/// half is unprintable.
/// Test: `entries_carry_the_value_only_inside_the_wrapper`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlistCredentialEntry {
    /// The environment-variable name, verbatim from the file.
    pub key: String,
    /// Its value. Unprintable — see [`PlistSecret`].
    pub value: PlistSecret,
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
    pub(super) fn new(reason: impl Into<String>) -> Self {
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
/// What: `keys` names what was removed (empty means nothing was) and `xml` is
/// the document with those entries removed (byte-identical to the input when
/// `keys` is empty).
/// Test: `scrub_removes_the_credential_entry_and_keeps_the_rest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubbedPlist {
    /// The keys removed, in document order. Never contains a value.
    pub keys: Vec<String>,
    /// The plist with those entries removed.
    pub xml: String,
}

/// Do `bytes` begin a BINARY property list?
///
/// Why (#8236 item 4): `plutil -convert binary1` and several third-party
/// installers write `bplist00`. Read as UTF-8 that is either an error or
/// garbage with no `<key>` in it, which the text scanner would call CLEAN. A
/// false clean here is the one outcome this module must never produce.
/// Test: `a_binary_plist_is_detected_by_magic`,
/// `an_xml_plist_is_not_mistaken_for_a_binary_one`.
#[must_use]
pub fn is_binary_plist(bytes: &[u8]) -> bool {
    bytes.starts_with(BINARY_PLIST_MAGIC)
}

/// Every credential-keyed `EnvironmentVariables` entry in `xml`, with values.
///
/// Why: `--fix` migrates rather than deletes (#8236 item 2), so it needs the
/// value to import before it may remove the entry.
/// What: [`is_credential_env_key`] over each `<key>` inside the
/// `EnvironmentVariables` dict, pairing it with the text of the value element
/// that follows. A document with no such dict yields an empty vector.
///
/// # Errors
///
/// [`PlistScrubError`] when the dict is unterminated or a key has no value
/// element — see [`scrub_plist_credential_env`].
///
/// Test: `entries_carry_the_value_only_inside_the_wrapper`,
/// `entries_are_empty_for_a_clean_plist`.
pub fn credential_entries(xml: &str) -> Result<Vec<PlistCredentialEntry>, PlistScrubError> {
    Ok(walk(xml)?
        .into_iter()
        .filter(|found| is_credential_env_key(&found.key))
        .map(|found| PlistCredentialEntry {
            key: found.key,
            value: PlistSecret::new(found.value),
        })
        .collect())
}

/// Rewrite `xml` without its credential-bearing `EnvironmentVariables` entries.
///
/// Why: an existing install is not fixed by a renderer guard — the plaintext
/// is already on disk, and on a host whose daemon is not reinstalled it stays
/// there.
/// What: [`scrub_plist_keys`] over every credential-keyed entry. Preserves
/// every other byte, including whitespace and key order.
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
    let wanted: Vec<String> = walk(xml)?
        .into_iter()
        .filter(|found| is_credential_env_key(&found.key))
        .map(|found| found.key)
        .collect();
    scrub_plist_keys(xml, &wanted)
}

/// Rewrite `xml` without the `EnvironmentVariables` entries named in `keys`.
///
/// Why (#8236 item 2): `--fix` may remove ONLY the entries whose value it has
/// confirmed into the credential store. A key with no registry mapping, or one
/// whose import failed, has to stay — removing it would break the feature it
/// configures while protecting nothing that a rotation would not.
/// What: as [`scrub_plist_credential_env`], restricted to the named keys
/// (case-sensitive, matching the file's own spelling). Keys not present are
/// silently absent from the result's `keys`.
///
/// # Errors
///
/// As [`scrub_plist_credential_env`].
///
/// Test: `scrub_plist_keys_removes_only_the_named_key`,
/// `scrub_plist_keys_with_no_names_is_byte_identical`.
pub fn scrub_plist_keys(xml: &str, keys: &[String]) -> Result<ScrubbedPlist, PlistScrubError> {
    let found = walk(xml)?;
    let mut removed = Vec::new();
    let mut drops: Vec<(usize, usize)> = Vec::new();
    for entry in found {
        if keys.iter().any(|k| k == &entry.key) {
            removed.push(entry.key);
            drops.push((entry.start, entry.end));
        }
    }

    let mut out = xml.to_string();
    for (from, to) in drops.iter().rev() {
        out.replace_range(*from..*to, "");
    }
    Ok(ScrubbedPlist {
        keys: removed,
        xml: out,
    })
}

/// One `<key>`/value pair found inside the `EnvironmentVariables` dict.
struct Found {
    key: String,
    value: String,
    /// Byte offset of the entry's `<key>`.
    start: usize,
    /// Byte offset just past the value element and its trailing newline.
    end: usize,
}

/// Every `EnvironmentVariables` pair, in document order.
///
/// Why: one parse shared by the read, the full scrub and the selective scrub,
/// so the three can never disagree about what the file holds.
///
/// # Errors
///
/// As [`scrub_plist_credential_env`].
fn walk(xml: &str) -> Result<Vec<Found>, PlistScrubError> {
    let Some((start, end)) = env_dict_span(xml)? else {
        return Ok(Vec::new());
    };

    let mut out = Vec::new();
    let mut cursor = start;
    while let Some(key_open) = find_from(xml, "<key>", cursor, end) {
        let key_body = key_open + "<key>".len();
        let key_close = find_from(xml, "</key>", key_body, end)
            .ok_or_else(|| PlistScrubError::new("an EnvironmentVariables <key> is unterminated"))?;
        let key = xml[key_body..key_close].trim().to_string();
        let value_end = value_element_end(xml, key_close + "</key>".len(), end)?;
        out.push(Found {
            key,
            value: value_text(xml, key_close + "</key>".len(), value_end),
            start: key_open,
            end: swallow_trailing_newline(xml, value_end),
        });
        cursor = value_end;
    }
    Ok(out)
}

/// The text between the value element's tags, in `xml[from..end]`.
///
/// Why: `--fix` imports this into the credential store. Returns the empty
/// string for a self-closing element (`<true/>`), which has no text and is
/// therefore never a credential worth migrating.
fn value_text(xml: &str, from: usize, end: usize) -> String {
    let Some(open) = xml[from..end].find('>').map(|o| from + o + 1) else {
        return String::new();
    };
    let Some(close) = xml[open..end].rfind("</").map(|o| open + o) else {
        return String::new();
    };
    if close < open {
        return String::new();
    }
    xml[open..close].trim().to_string()
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
