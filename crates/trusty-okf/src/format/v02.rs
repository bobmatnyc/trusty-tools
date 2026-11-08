//! OKF v0.2 field mapping and v0.1 read fallbacks (SPEC-OKF-01 §2.8).
//!
//! Why: trusty-kb wrote OKF v0.1. trusty-okf writes v0.2, reads v0.1 through
//! the OKF §13 fallbacks, and never discards a value to make a concept
//! conformant: a legacy key is removed only when its value is recoverable
//! byte for byte from the new key.
//! What: [`normalize_v02`] applies the per-concept §2.8 rules through
//! [`Concept::set`] and [`Concept::remove`], so the raw-preserving render
//! keeps every other entry's bytes. `migrate` (slice S6) runs it per file
//! inside the journalled plan, apply and audit. [`generated_at`] and
//! [`sources`] read v0.2 values with the OKF §13 fallbacks.
//! Test: `c18_offset_updated_migrates_to_generated_at`,
//! `c19_date_only_updated_stays_with_warning`,
//! `c20_plain_string_sources_become_mappings`,
//! `c21_legacy_keys_byte_identical_after_normalize`,
//! `c22_v01_timestamp_and_citations_read_as_v02`.

use chrono::{DateTime, FixedOffset};
use serde_yaml::{Mapping, Value};

use crate::format::concept::Concept;
use crate::format::fields::{Field, Source, parse_instant};
use crate::format::validate::{Finding, FindingKind, Severity};

/// The `generated.by` actor a migration writes when it maps `updated`
/// (SPEC-OKF-01 §2.8, proposal). v0.1 data names no producer.
pub const LEGACY_PRODUCER: &str = "process:trusty-kb-legacy";

/// Apply the §2.8 per-concept rules to `c`.
///
/// What, each rule touching only its own keys:
/// - `updated` that parses as an instant with an explicit offset, on a
///   concept with no `generated`, becomes
///   `generated: {by: process:trusty-kb-legacy, at: <the same value>}`, and
///   `updated` is removed. Otherwise `updated` stays, no `generated` is
///   written, and a warning is returned.
/// - each plain-string `sources` entry `s` becomes `{resource: s}`, in order,
///   with no `id`; a mapping entry passes through.
/// - a bare `verified` mapping becomes a one-element list.
///
/// Every other key is left alone, including `created`, `aliases`,
/// `same_as`, `uid`, `trust`, the `source_*` keys and wiki-link fields.
/// Test: `c18_offset_updated_migrates_to_generated_at`,
/// `c19_date_only_updated_stays_with_warning`,
/// `c20_plain_string_sources_become_mappings`,
/// `c21_legacy_keys_byte_identical_after_normalize`.
pub fn normalize_v02(c: &mut Concept) -> Vec<Finding> {
    let mut findings = Vec::new();
    let has_generated = c.get("generated").is_some();
    if let Some(updated) = c.get("updated").cloned()
        && !has_generated
    {
        if updated.as_str().and_then(parse_instant).is_some() {
            let mut generated = Mapping::new();
            generated.insert("by".into(), LEGACY_PRODUCER.into());
            generated.insert("at".into(), updated);
            c.set("generated", Value::Mapping(generated));
            c.remove("updated");
        } else {
            findings.push(Finding {
                severity: Severity::Warning,
                kind: FindingKind::UnmappedLegacyValue,
                path: c.path(),
                concept: None,
                message: "`updated` is not an ISO 8601 datetime with an explicit offset; \
                          kept, and no `generated` written"
                    .to_string(),
                spec_ref: "SPEC-OKF-01 §2.8",
            });
        }
    }
    if let Some(Value::Sequence(seq)) = c.get("sources").cloned()
        && seq.iter().any(Value::is_string)
    {
        let mapped = seq.into_iter().map(|v| match v {
            Value::String(s) => resource_entry(s),
            other => other,
        });
        c.set("sources", Value::Sequence(mapped.collect()));
    }
    if let Some(one @ Value::Mapping(_)) = c.get("verified").cloned() {
        c.set("verified", Value::Sequence(vec![one]));
    }
    if let crate::format::id::ConceptRef::Id(id) = &c.id {
        for f in &mut findings {
            f.concept = Some(id.clone());
        }
    }
    findings
}

fn resource_entry(s: String) -> Value {
    let mut m = Mapping::new();
    m.insert("resource".into(), Value::String(s));
    Value::Mapping(m)
}

/// `generated.at`, falling back to a legacy `timestamp` (OKF §13.1).
///
/// What: a valid `generated` gives its `at`; an absent `generated` gives a
/// `timestamp` that parses as an instant with an explicit offset; anything
/// else gives `None`.
/// Test: `c22_v01_timestamp_and_citations_read_as_v02`.
pub fn generated_at(c: &Concept) -> Option<DateTime<FixedOffset>> {
    match &c.frontmatter.generated {
        Field::Valid(g) => g.at,
        Field::Absent => c
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_instant),
        Field::Malformed(_) => None,
    }
}

/// `sources`, falling back to a body `# Citations` list (OKF §13.1).
///
/// What: a `sources` list gives its valid entries, with a plain-string entry
/// read as `{resource: s}` (§2.8); an absent `sources` gives one entry per
/// item of the body's `# Citations` list. The body is never edited.
/// Test: `c22_v01_timestamp_and_citations_read_as_v02`.
pub fn sources(c: &Concept) -> Vec<Source> {
    let plain = |resource: String| Source {
        resource,
        id: None,
        title: None,
        author: None,
    };
    match &c.frontmatter.sources {
        Field::Valid(list) => list
            .iter()
            .filter_map(|s| match s {
                Field::Valid(v) => Some(v.clone()),
                Field::Malformed(Value::String(r)) => Some(plain(r.clone())),
                _ => None,
            })
            .collect(),
        Field::Absent => citations(&c.body).into_iter().map(plain).collect(),
        Field::Malformed(_) => Vec::new(),
    }
}

/// The items of a body `# Citations` list (any heading level), in order.
pub fn citations(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            inside = t
                .trim_start_matches('#')
                .trim()
                .eq_ignore_ascii_case("citations");
            continue;
        }
        if !inside {
            continue;
        }
        let item = t
            .strip_prefix("- ")
            .or_else(|| t.strip_prefix("* "))
            .or_else(|| t.strip_prefix("+ "));
        if let Some(item) = item.map(str::trim).filter(|i| !i.is_empty()) {
            out.push(item.to_string());
        }
    }
    out
}
