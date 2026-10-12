//! The review ledger's record type and the verdict it carries (#8624, PR-C).
//!
//! Why: an agent judges each exported row once, and the import later applies
//! that judgement. The ledger line is its own type, never an export row, so
//! nothing an export carries (masked text, ids) can be read back as a verdict.
//! What: `LedgerEntry` binds a [`Verdict`] to the row's ledger key and to
//! `SECRET_FILTER_VERSION`, plus a `verdict_digest` over all three. The
//! redaction ops are a closed enum that takes a span index and nothing else.
//! Test: `review::ledger_tests`, `review::admit_tests`.

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Version of the [`LedgerEntry`] line format.
pub(crate) const LEDGER_SCHEMA: u32 = 1;

/// One redaction step, addressing a span by its index into the row's
/// `find_secret_spans` output (the marker's `S{index}`).
///
/// Why (#8624 ruling): the op set is closed and takes no free text, so a
/// verdict can never smuggle replacement text or parse a marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum RedactOp {
    /// Replace the span with `[REDACTED:<RULE>]`, the rule label upper-cased.
    RedactSpan { span: usize },
    /// Remove the span's bytes.
    DeleteSpan { span: usize },
    /// Remove the whole line (or lines) holding the span.
    DropLine { span: usize },
}

impl RedactOp {
    /// The span index this op addresses.
    pub(crate) fn span(self) -> usize {
        match self {
            Self::RedactSpan { span } | Self::DeleteSpan { span } | Self::DropLine { span } => span,
        }
    }

    /// The op's name, as `--op` spells it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::RedactSpan { .. } => "redact_span",
            Self::DeleteSpan { .. } => "delete_span",
            Self::DropLine { .. } => "drop_line",
        }
    }
}

impl FromStr for RedactOp {
    type Err = String;

    /// Parse `<op>:<span index>`, e.g. `redact_span:0`.
    ///
    /// Test: `op_parses_name_and_index_only`.
    fn from_str(s: &str) -> Result<Self, String> {
        let (name, index) = s
            .split_once(':')
            .ok_or_else(|| format!("expected <op>:<span index>, got {s:?}"))?;
        let span: usize = index
            .parse()
            .map_err(|_| format!("span index {index:?} is not a number"))?;
        match name {
            "redact_span" => Ok(Self::RedactSpan { span }),
            "delete_span" => Ok(Self::DeleteSpan { span }),
            "drop_line" => Ok(Self::DropLine { span }),
            _ => Err(format!(
                "unknown op {name:?}; expected redact_span, delete_span or drop_line"
            )),
        }
    }
}

/// An agent's verdict on one exported row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// Struct variants, not unit ones: serde enforces `deny_unknown_fields` on an
// internally tagged variant only when it has a body.
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Verdict {
    /// Admit the content verbatim: the carve-out, tagged `kuzu-review:override`.
    Admit {},
    /// Admit the content after these ops; it is screened again.
    AdmitRedacted { ops: Vec<RedactOp> },
    /// Leave the row refused.
    KeepRefused {},
}

impl Verdict {
    /// The verdict's report label.
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Admit {} => "admit",
            Self::AdmitRedacted { .. } => "admit-redacted",
            Self::KeepRefused {} => "keep-refused",
        }
    }
}

/// One line of the review ledger.
///
/// Why (#8624): a verdict applies only to the exact content and detector the
/// reviewer judged. `key` is the export's ledger key (sha256 over store, id
/// and content), `filter_version` the detector's, and `digest` covers both
/// plus the verdict, so an edited line no longer matches.
/// Test: `digest_binds_key_version_and_verdict`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LedgerEntry {
    pub schema: u32,
    pub key: String,
    pub filter_version: u32,
    pub verdict: Verdict,
    pub digest: String,
}

impl LedgerEntry {
    /// An entry for `verdict` on `key`, with its digest computed.
    pub(crate) fn new(key: String, filter_version: u32, verdict: Verdict) -> Self {
        let digest = verdict_digest(&key, filter_version, &verdict);
        Self {
            schema: LEDGER_SCHEMA,
            key,
            filter_version,
            verdict,
            digest,
        }
    }

    /// Whether `digest` is the digest of this entry's own fields.
    pub(crate) fn digest_matches(&self) -> bool {
        self.digest == verdict_digest(&self.key, self.filter_version, &self.verdict)
    }
}

/// Lowercase hex sha256 of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// sha256 over a domain tag, `key`, `filter_version` and the verdict's
/// canonical text (`label` then `op:span` per op, in order), as hex.
///
/// What: every variable-length part is length-prefixed, so no two distinct
/// inputs share a byte string.
/// Test: `digest_binds_key_version_and_verdict`.
pub(crate) fn verdict_digest(key: &str, filter_version: u32, verdict: &Verdict) -> String {
    let mut canon = verdict.label().to_string();
    if let Verdict::AdmitRedacted { ops } = verdict {
        for op in ops {
            canon.push_str(&format!(";{}:{}", op.name(), op.span()));
        }
    }
    let mut bytes = b"kuzu-review-verdict/1\0".to_vec();
    for part in [key, canon.as_str()] {
        bytes.extend((part.len() as u64).to_be_bytes());
        bytes.extend(part.as_bytes());
    }
    bytes.extend(filter_version.to_be_bytes());
    sha256_hex(&bytes)
}
