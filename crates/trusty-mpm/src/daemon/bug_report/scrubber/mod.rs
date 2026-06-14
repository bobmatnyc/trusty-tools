//! Sensitive-data scrubber for bug reports.
//!
//! Why: All captured errors are filed to the public `bobmatnyc/trusty-tools`
//!      repository. Before filing, we must strip personally-identifiable
//!      information and secrets: absolute file paths, usernames, API tokens,
//!      JWT strings, Bearer tokens, and key=value environment pairs. Scrubbing
//!      happens before the user reviews the preview — the preview body IS the
//!      filed body, so the scrubber runs exactly once per report.
//! What: [`scrub`] takes a raw string and returns a [`ScrubResult`] with the
//! cleaned text, a list of [`ScrubChange`] records describing what was removed,
//! and a human-readable redaction summary for display in the preview UI. The
//! body is truncated to [`MAX_BODY_BYTES`] after scrubbing to stay within
//! GitHub's 65 536 byte limit.
//!
//! The scrubber is intentionally conservative: when in doubt it redacts.
//! False positives (non-secret text that looks like a secret) are far
//! safer than false negatives (a real secret leaking into a public issue).
//!
//! Test: `tests::paths_redacted`, `tests::bearer_redacted`,
//!       `tests::truncation_applies`, `tests::aws_key_redacted`,
//!       `tests::google_key_redacted`, `tests::pem_block_redacted`,
//!       `tests::connection_string_redacted`, `tests::slack_token_redacted`,
//!       `tests::sk_prefix_redacted`, `tests::github_token_prefixes_redacted`.

mod core;
mod regexes;
mod rules;
mod types;

#[cfg(test)]
mod tests;

pub use core::{scrub, scrub_compat};
pub use types::{MAX_BODY_BYTES, ScrubChange, ScrubResult};
