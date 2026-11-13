//! The one `token*` name the #7266 file rule releases: a usage tally (#10570).
//!
//! Why: the shared `token*` entry reads any name starting `token` as a
//! credential file, so the seeded `dashboard-serve.py`, which names the usage
//! tally `token-hourly.jsonl`, refused as a secret-file read.
//! What: [`is_token_tally`] answers for a stem starting `token`, carrying a
//! tally word, with a log extension, whose name without the `token` prefix no
//! other secret family claims (`token-hourly-credentials.jsonl` still counts).
//! Test: `a_token_tally_exempts_only_the_token_family_10570`.

use super::is_secret_read_target;

/// Final extensions of an append-only log a tally is written as.
const RECORD_EXTENSIONS: &[&str] = &["jsonl", "ndjson", "log"];

/// Stem words that make a `token*` log a tally of token counts.
const TALLY_WORDS: &[&str] = &[
    "hourly", "daily", "weekly", "usage", "count", "counts", "tally", "stats",
];

/// Whether basename `base` is a token-count log rather than a token file.
pub(super) fn is_token_tally(base: &str) -> bool {
    let lower = base.to_ascii_lowercase();
    let Some((stem, ext)) = lower.split_once('.') else {
        return false;
    };
    let Some(rest) = stem
        .strip_prefix("tokens")
        .or_else(|| stem.strip_prefix("token"))
    else {
        return false;
    };
    // #10641: a computed or glob name is is never an exempt tally.
    if base.contains(['$', '`', '{', '*', '?', '[']) {
        return false;
    }
    let last = ext.rsplit('.').next().unwrap_or_default();
    RECORD_EXTENSIONS.contains(&last)
        && rest.split(['-', '_']).any(|w| TALLY_WORDS.contains(&w))
        && !is_secret_read_target(&format!("{rest}.{ext}"))
        // #10641: `token-usage.key.log` keeps its inner `.key` family.
        && !ext
            .split('.')
            .any(|inner| is_secret_read_target(&format!("{rest}.{inner}")))
}

#[cfg(test)]
mod tests {
    use crate::commands::pm_guard_secret_read::{
        Scan, evaluate_secret_file_read_command, names_a_secret_file,
    };

    /// 🔴 REGRESSION (#10570 review): a `token` prefix never cancels another
    /// secret family, and a tally needs a tally word and a log extension.
    #[test]
    fn a_token_tally_exempts_only_the_token_family_10570() {
        let wrong: Vec<String> = [
            ("cat token-credentials.jsonl", true),
            ("cat token_secrets.csv", true),
            ("cat token-db.tfstate.log", true),
            ("cat /etc/secrets/token-api.ndjson", true),
            ("cat tokens-export.csv", true),
            ("cat token-hourly-credentials.jsonl", true),
            ("cat token.json", true),
            ("cat token-usage.key.log", true),
            ("cat token-hourly.pem.jsonl", true),
            ("cat local/usage/token-hourly.jsonl", false),
            // #10641: a glob name is never exempt. (`token-hourly-$T.jsonl`
            // is cut at `$` into fragments no rule reads; main allows it too.)
            ("cat token-hourly-*.jsonl", true),
            ("cat token-hourly.*.jsonl", true),
            ("cat token-hourly-{credentials,x}.jsonl", true),
            ("cat token-hourly.jsonl", false),
            ("cat local/usage/token-usage.jsonl", false),
            ("cat token-hourly.jsonl; cat ~/.netrc", true),
        ]
        .iter()
        .filter(|(c, deny)| evaluate_secret_file_read_command(c).is_some() != *deny)
        .map(|(c, deny)| format!("{c}: want deny={deny}"))
        .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
        assert!(!names_a_secret_file(
            "token-hourly.jsonl",
            Scan::ProgramText
        ));
    }
}
