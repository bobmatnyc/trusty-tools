//! What a Keeper record is to this backend: the listing rows it reads, the
//! record JSON it reads a value from, and the batch commands it writes.
//!
//! Why: #7519 P3 rulings 1, 3, 4 and 6 — one `login` record per key, titled
//! with the key, in the Keeper folder whose path is the trusty vault name.
//! A value reaches `keeper` only inside a batch command on stdin, as
//! `$BASE64:<text>`, so no quoting rule of Commander's batch parser can
//! split or rewrite it. Only validated names and listed uids that pass
//! [`checked_uid`] reach argv or a batch command.
//! What: [`Entry`] and [`parse_listing`] (an `ls --format json` answer),
//! [`titled`] and [`folders_named`], [`password_from`] (a `get --format
//! json` answer to the value), [`add_command`] and [`update_command`].
//!
//! UNCONFIRMED: the JSON shapes below are this backend's reading of
//! Commander's `ls --format json` and `get --format json`; no run against a
//! real account has pinned them. Every field is required and an unknown
//! row type fails the parse, so a different shape is an error, never a miss.
//! Test: `keeper_listing_and_record_parsers_fail_closed`,
//! `keeper_batch_commands_carry_the_value_only_as_base64`.

use serde::Deserialize;

use crate::api::{SecretKey, SecretValue, VaultName};

/// The record type every record this backend writes has.
pub(super) const RECORD_TYPE: &str = "login";

/// The field that holds the value.
const FIELD: &str = "password";

/// Longest uid accepted from `keeper` output; Keeper uids are 22 characters.
const MAX_UID_LEN: usize = 64;

/// One `ls --format json` row.
///
/// What: a folder or a record, told apart by `type`; any other `type`, or
/// a missing field, fails the parse.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(super) enum Entry {
    /// A subfolder.
    Folder { name: String },
    /// A record.
    Record {
        uid: String,
        title: String,
        record_type: String,
    },
}

/// A record row titled with the key.
#[derive(Debug)]
pub(super) struct Listed {
    pub(super) uid: String,
    pub(super) record_type: String,
}

/// The rows of an `ls --format json` answer.
///
/// What: the answer must be a JSON array. Empty output, or any other text,
/// is `Err` with a fixed reason; the parser's message, which could quote
/// the output, is dropped (#7519 ruling 3: exit 0 with text that is not a
/// listing is a failure).
/// Test: `keeper_listing_and_record_parsers_fail_closed`.
pub(super) fn parse_listing(stdout: &str) -> Result<Vec<Entry>, &'static str> {
    serde_json::from_str(stdout).map_err(|_| "Keeper's folder listing did not parse")
}

/// The record rows of `entries` titled exactly `key`, of any record type.
pub(super) fn titled(entries: Vec<Entry>, key: &SecretKey) -> Vec<Listed> {
    entries
        .into_iter()
        .filter_map(|entry| match entry {
            Entry::Record {
                uid,
                title,
                record_type,
            } if title == key.as_str() => Some(Listed { uid, record_type }),
            _ => None,
        })
        .collect()
}

/// How many folder rows of `entries` are named exactly `name`.
pub(super) fn folders_named(entries: &[Entry], name: &str) -> usize {
    entries
        .iter()
        .filter(|entry| matches!(entry, Entry::Folder { name: n } if n == name))
        .count()
}

/// `get --format json` for one record: identity, type and typed fields.
#[derive(Deserialize)]
struct Got {
    record_uid: String,
    title: String,
    #[serde(rename = "type")]
    kind: String,
    fields: Vec<GotField>,
}

#[derive(Deserialize)]
struct GotField {
    #[serde(rename = "type")]
    kind: String,
    value: Vec<serde_json::Value>,
}

/// The value of record `uid`, from its `get --format json` answer.
///
/// Why: ruling 3 — only a confirmed answer is a value. A `get` that names
/// another record, title or type, or holds no single password, is an error.
/// What: the answer parses; `record_uid`, `title` and `type` are `uid`,
/// `key` and `login`; exactly one `password` field holds exactly one
/// string. Errors carry fixed text only, never the output.
/// Test: `keeper_listing_and_record_parsers_fail_closed`.
pub(super) fn password_from(
    stdout: &str,
    uid: &str,
    key: &SecretKey,
) -> Result<SecretValue, &'static str> {
    let got: Got =
        serde_json::from_str(stdout).map_err(|_| "Keeper's record answer did not parse")?;
    if got.record_uid != uid || got.title != key.as_str() || got.kind != RECORD_TYPE {
        return Err("Keeper answered with another record than the one listed");
    }
    let mut passwords = got.fields.into_iter().filter(|f| f.kind == FIELD);
    let (Some(field), None) = (passwords.next(), passwords.next()) else {
        return Err("the Keeper record does not hold exactly one password field");
    };
    match field.value.as_slice() {
        [serde_json::Value::String(value)] => Ok(SecretValue::new(value.as_str())),
        _ => Err("the Keeper record's password field does not hold one value"),
    }
}

/// A batch command and the encoded value inside it, both secret.
pub(super) struct Batch {
    /// The command line, newline-terminated, written to `keeper`'s stdin.
    pub(super) text: SecretValue,
    /// The `$BASE64:` text alone, which the runner also refuses in argv.
    pub(super) encoded: SecretValue,
}

/// `record-add` for a new `login` record titled `key` in folder `vault`.
///
/// Why: ruling 1 — the value is never argv; ruling 4 — the record goes in
/// the vault's own folder.
/// What: `record-add --folder=<vault> --title=<key> --record-type=login
/// password=$BASE64:<b64>`. `--name=value` forms, so a word starting with
/// `-` can never be read as a flag.
/// Test: `keeper_batch_commands_carry_the_value_only_as_base64`.
pub(super) fn add_command(
    vault: &VaultName,
    key: &SecretKey,
    value: &SecretValue,
) -> Result<Batch, &'static str> {
    let folder = batch_word(vault.as_str())?;
    let title = batch_word(key.as_str())?;
    batch(
        format!("record-add --folder={folder} --title={title} --record-type={RECORD_TYPE}"),
        value,
    )
}

/// `record-update` of the `password` field of record `uid`.
///
/// Test: `keeper_batch_commands_carry_the_value_only_as_base64`.
pub(super) fn update_command(uid: &str, value: &SecretValue) -> Result<Batch, &'static str> {
    batch(
        format!("record-update --record={}", checked_uid(uid)?),
        value,
    )
}

fn batch(head: String, value: &SecretValue) -> Result<Batch, &'static str> {
    // #7519 P3: an empty `$BASE64:` would clear the field, not set a value.
    if value.is_empty() {
        return Err("Keeper cannot store an empty value");
    }
    let encoded = SecretValue::new(base64(value.expose().as_bytes()));
    let text = SecretValue::new(format!("{head} {FIELD}=$BASE64:{}\n", encoded.expose()));
    Ok(Batch { text, encoded })
}

/// `raw`, when it is one batch word: no whitespace, quote, backslash, `#`,
/// `$` or control character, and no leading `-`.
fn batch_word(raw: &str) -> Result<&str, &'static str> {
    let safe = !raw.is_empty()
        && !raw.starts_with('-')
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '/'));
    if safe {
        Ok(raw)
    } else {
        Err("a name is not safe inside a Keeper batch command")
    }
}

/// `raw`, when it is safe as one argv word and inside a batch command.
///
/// What: 1–64 characters of `[A-Za-z0-9_-]`, Keeper's url-safe base64 uid
/// alphabet. A uid may start with `-`, so every argv use follows `--` and
/// every batch use is `--record=<uid>`.
/// Test: `keeper_listing_and_record_parsers_fail_closed`.
pub(super) fn checked_uid(raw: &str) -> Result<&str, &'static str> {
    let safe = !raw.is_empty()
        && raw.len() <= MAX_UID_LEN
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'));
    if safe {
        Ok(raw)
    } else {
        Err("Keeper returned a uid outside its uid alphabet")
    }
}

/// Standard base64 with padding (RFC 4648 §4).
///
/// Why: #7519 — `cli-backends` takes no new dependency, so the encoder is
/// local.
/// Test: `keeper_batch_commands_carry_the_value_only_as_base64`.
pub(super) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}
