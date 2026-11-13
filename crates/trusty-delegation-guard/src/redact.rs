//! Credential redaction and the 8 KiB prompt-excerpt cap (design §3).
//!
//! Why: an event stores a prompt excerpt, and a brief can quote a credential.
//! The design text points at the `pm_guard_secret_*` detectors in trusty-mpm;
//! depending on trusty-mpm from here would create a cycle once trusty-mpm
//! depends on this crate, so this module carries its own detector set
//! (#10892, PM ruling).
//! What: a fixed list of credential-shape patterns, each replaced by
//! `[REDACTED:<kind>]`; then a byte cap on a char boundary.
//! Test: `each_detector_redacts_its_shape`, `excerpt_is_capped_after_redaction`.

use std::sync::LazyLock;

use regex::Regex;

/// Maximum stored excerpt length in bytes, measured after redaction.
pub const EXCERPT_MAX_BYTES: usize = 8 * 1024;

/// (kind, pattern). Order matters: specific vendor shapes run before the
/// generic `key=value` and bearer forms so the label names the vendor.
const DETECTORS: &[(&str, &str)] = &[
    (
        "private_key",
        r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]*PRIVATE KEY-----|\z)",
    ),
    ("aws_access_key", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
    ("github_token", r"\bgh[pousr]_[A-Za-z0-9]{30,}"),
    ("github_pat", r"\bgithub_pat_[A-Za-z0-9_]{22,}"),
    ("anthropic_key", r"\bsk-ant-[A-Za-z0-9_\-]{20,}"),
    ("sk_key", r"\bsk-[A-Za-z0-9_\-]{20,}"),
    ("slack_token", r"\bxox[abposr]-[A-Za-z0-9\-]{10,}"),
    (
        "jwt",
        r"\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
    ),
    ("bearer", r"(?i)\bbearer\s+[A-Za-z0-9._~+/\-]{16,}=*"),
    (
        "assignment",
        r#"(?i)\b(?:api[_-]?key|secret|token|password|passwd)\b\s*[:=]\s*["']?[^\s"']{8,}"#,
    ),
];

static COMPILED: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    DETECTORS
        .iter()
        // Literal patterns, compiled once; `every_detector_pattern_compiles`
        // pins that each one is valid.
        .map(|(kind, pat)| (*kind, Regex::new(pat).expect("static detector pattern")))
        .collect()
});

/// Result of redacting one text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redacted {
    /// The text with every credential shape replaced.
    pub text: String,
    /// True when any detector matched.
    pub redacted: bool,
}

/// Replaces every credential shape in `input`.
///
/// Test: `each_detector_redacts_its_shape`, `clean_text_is_untouched`.
pub fn redact(input: &str) -> Redacted {
    let mut text = input.to_owned();
    let mut redacted = false;
    for (kind, re) in COMPILED.iter() {
        if re.is_match(&text) {
            redacted = true;
            text = re
                .replace_all(&text, format!("[REDACTED:{kind}]").as_str())
                .into_owned();
        }
    }
    Redacted { text, redacted }
}

/// A stored prompt excerpt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excerpt {
    /// Redacted text, at most [`EXCERPT_MAX_BYTES`] bytes.
    pub text: String,
    /// True when the redacted text was cut.
    pub truncated: bool,
    /// True when any detector matched.
    pub redacted: bool,
}

/// Redacts `prompt`, then caps it at [`EXCERPT_MAX_BYTES`] on a char boundary.
///
/// Why: capping first could cut a credential in half and leave a prefix the
/// detectors no longer recognise.
/// Test: `excerpt_is_capped_after_redaction`, `excerpt_cap_respects_char_boundary`.
pub fn excerpt(prompt: &str) -> Excerpt {
    let Redacted { text, redacted } = redact(prompt);
    Excerpt {
        truncated: text.len() > EXCERPT_MAX_BYTES,
        text: cap_bytes(&text, EXCERPT_MAX_BYTES).to_owned(),
        redacted,
    }
}

/// Cuts `text` to at most `max` bytes on a char boundary.
pub(crate) fn cap_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    &text[..cut]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_detector_pattern_compiles() {
        for (kind, pat) in DETECTORS {
            assert!(Regex::new(pat).is_ok(), "{kind} pattern must compile");
        }
        assert_eq!(COMPILED.len(), DETECTORS.len());
    }
}
