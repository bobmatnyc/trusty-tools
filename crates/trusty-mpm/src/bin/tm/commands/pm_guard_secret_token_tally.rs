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
    let last = ext.rsplit('.').next().unwrap_or_default();
    RECORD_EXTENSIONS.contains(&last)
        && rest.split(['-', '_']).any(|w| TALLY_WORDS.contains(&w))
        && !is_secret_read_target(&format!("{rest}.{ext}"))
}
