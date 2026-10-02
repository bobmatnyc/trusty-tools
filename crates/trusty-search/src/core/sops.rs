//! Content check for sops-encrypted files (#8922).
//!
//! Why: an encrypted secrets file under an indexed path was chunked and served
//! by search, because no ingest path looked at content for the `sops` metadata
//! block. sops keeps a file's extension (`secrets.yaml`, `.env`, `config.json`),
//! so a filename rule cannot find it.
//! What: [`is_sops_encrypted`], called by the indexer on every write, so the
//! walker, `index_file`, the watcher and both reconciles share one answer.
//! Test: `sops_files_in_every_format_are_detected`,
//! `plain_files_that_mention_sops_are_not_detected`.

/// The value prefix sops writes for every encrypted value, the MAC included.
const ENCRYPTED_VALUE: &str = "ENC[AES256_GCM,";

/// Whether `content` is a sops-encrypted document.
///
/// Why: see the module docs — the extension says nothing, the content does.
/// What: `true` when the content holds a sops encrypted value AND a sops
/// metadata block in one of the formats sops writes: a top-level `sops:` key
/// (YAML), a `"sops": {` object (JSON, and the binary format sops stores as
/// JSON), `sops_version=` / `sops_mac=` keys (dotenv), or a `[sops]` section
/// (INI). Requiring both keeps a file that only names sops from matching. The
/// check reads a string the caller already holds, so it has no error path.
/// Test: `sops_files_in_every_format_are_detected`,
/// `plain_files_that_mention_sops_are_not_detected`.
pub fn is_sops_encrypted(content: &str) -> bool {
    if !content.contains(ENCRYPTED_VALUE) {
        return false;
    }
    if has_json_sops_object(content) {
        return true;
    }
    content.lines().any(|line| {
        let line = line.trim_end();
        line == "sops:"
            || line == "[sops]"
            || line.starts_with("sops_version=")
            || line.starts_with("sops_mac=")
    })
}

/// Whether `content` holds a `"sops"` key whose value is an object.
fn has_json_sops_object(content: &str) -> bool {
    content.match_indices("\"sops\"").any(|(at, key)| {
        let rest = content[at + key.len()..].trim_start();
        rest.strip_prefix(':')
            .is_some_and(|value| value.trim_start().starts_with('{'))
    })
}

/// An encrypted value, built from parts so no source file holding a fixture is
/// itself a sops document to an index that walks it.
#[cfg(test)]
pub(crate) fn enc(data: &str) -> String {
    format!(
        "ENC[AES256{}data:{data},iv:aXY=,tag:dGFn,type:str]",
        "_GCM,"
    )
}

/// A sops-encrypted YAML file, the shape the #8922 report names.
#[cfg(test)]
pub(crate) fn sample_sops_yaml() -> String {
    format!(
        "db_password: {}\nsops:\n    mac: {}\n    version: 3.8.1\n",
        enc("c2VjcmV0"),
        enc("bWFj")
    )
}

#[cfg(test)]
mod tests {
    use super::{enc, is_sops_encrypted};

    /// #8922: each format sops writes is detected from content alone, whatever
    /// the extension. Fails with `is_sops_encrypted` returning `false`.
    #[test]
    fn sops_files_in_every_format_are_detected() {
        let v = enc("c2VjcmV0");
        let mac = enc("bWFj");
        let cases = [
            (
                "yaml",
                format!("password: {v}\nsops:\n    mac: {mac}\n    version: 3.8.1\n"),
            ),
            (
                "json",
                format!("{{\n  \"password\": \"{v}\",\n  \"sops\": {{\n    \"mac\": \"{mac}\"\n  }}\n}}\n"),
            ),
            (
                "json-compact",
                format!("{{\"password\":\"{v}\",\"sops\":{{\"mac\":\"{mac}\"}}}}"),
            ),
            (
                "dotenv",
                format!("PASSWORD={v}\nsops_mac={mac}\nsops_version=3.8.1\n"),
            ),
            (
                "ini",
                format!("[db]\npassword = {v}\n\n[sops]\nmac = {mac}\n"),
            ),
        ];
        for (format, content) in cases {
            assert!(is_sops_encrypted(&content), "{format}: {content}");
        }
    }

    /// #8922: a sops metadata key with no encrypted value, or an encrypted
    /// value with no metadata block, is not a sops document — so a doc or a
    /// config that only mentions sops stays indexable.
    #[test]
    fn plain_files_that_mention_sops_are_not_detected() {
        let v = enc("c2VjcmV0");
        let cases = [
            "sops:\n  creation_rules: []\n".to_string(),
            "{\"sops\": {\"note\": \"plain\"}}".to_string(),
            format!("example value: {v}\n"),
            format!("# sops: documented here\nvalue = \"{v}\"\n"),
            "fn sops() {}\n".to_string(),
        ];
        for content in cases {
            assert!(!is_sops_encrypted(&content), "{content}");
        }
    }
}
