//! Normalisation of authenticated-source items into concepts (SPEC-OKF-01
//! §2.15, Appendix C).
//!
//! Why: trusty-okf owns IDs, provenance and the concept body for mail and
//! Google Docs items, while acquisition stays in an adapter process outside
//! it (§2.15 "Who owns what", B1, B6). One fixed record shape between the
//! two keeps the adapter thin and the normalisation testable without an
//! account.
//! What: [`SourceRecord`] is one fetched item as an adapter returns it.
//! [`ConnectorKind::of`] maps a store kind (`gmail` to mail; `drive` and
//! `gdocs` to docs). [`check_selection`] refuses a run with no account or
//! no source selection (`SelectionRequired`). [`concept_id`] derives
//! `mail/<source-id>/<thread-id>/<message-id>` and
//! `docs/<source-id>/<file-id>` from immutable source IDs, never from a
//! subject. [`normalise`] picks the body (mail: the full text part, else
//! the HTML part; docs: the Markdown export, else the HTML export), lists
//! attachments by name, type and size without extracting them, records the
//! source dates and revision as provenance, and reports a mail item with no
//! decodable text part as `degraded: snippet_only`: no concept under the
//! default policy, the flagged snippet under [`ContentPolicy::AllowSnippet`].
//! A docs item with only an encoded (base64) export is `unsupported_format`.
//! Raw MIME and any other record field outside the shape are never read.
//! Slice S9 of #10746.
//! Test: `crates/trusty-okf/tests/adapter_criteria.rs` (criteria 86, 88,
//! 89, 91-93), `record::tests`.
//!
//! # Spec References
//! - SPEC-OKF-01 §2.15 items 1-12, Appendix C.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};

use super::plan::{ContentPolicy, ImportSpec};
use super::source::ListEntry;
use crate::OkfError;
use crate::build::registry::{ConnectorParams, Locator};
use crate::format::id::ConceptId;

/// The normaliser version; a change re-imports every connector item.
pub const NORMALISER_VERSION: &str = "1";

/// One body part or export of an item.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    /// The MIME type (`text/plain`, `text/html`, `text/markdown`).
    pub mime: String,
    /// `utf8` (default) or an encoding the host does not decode (`base64`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    /// The part's data.
    pub data: String,
}

/// One attachment, listed and never extracted (3k A).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// The file name.
    pub name: String,
    /// The MIME type.
    pub mime: String,
    /// The size in bytes.
    pub size: u64,
}

/// One fetched item, as an adapter returns it (Appendix C).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRecord {
    /// The immutable item ID: the message ID or the file ID.
    pub item_id: String,
    /// The thread ID of a message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The source revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// The subject or document name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Source dates: `internal_date` (epoch milliseconds) and `date_header`
    /// for mail; `modified_time` and `created_time` for docs.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dates: BTreeMap<String, String>,
    /// Body parts or exports, in any order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
    /// The source's short preview, never a body by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// Attachments, listed only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// The adapter's per-item notes (for example a dropped image).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// Other source fields; checked against the protected set (§2.13.1).
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/// The normalisation family of a connector store kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorKind {
    /// `gmail`.
    Mail,
    /// `drive` and `gdocs` (native Docs export).
    Docs,
}

impl ConnectorKind {
    /// The family of `kind`, or `None` for a kind trusty-okf cannot import.
    pub fn of(kind: &str) -> Option<ConnectorKind> {
        match kind {
            "gmail" => Some(ConnectorKind::Mail),
            "drive" | "gdocs" => Some(ConnectorKind::Docs),
            _ => None,
        }
    }

    /// The extractor tag a plan and a provenance key show.
    pub fn tag(self) -> String {
        let name = match self {
            ConnectorKind::Mail => "mail-normaliser",
            ConnectorKind::Docs => "docs-normaliser",
        };
        format!("{name}@{NORMALISER_VERSION}")
    }

    /// Body preference, best first (Appendix C "Body", "Structure").
    fn preferred(self) -> [&'static str; 2] {
        match self {
            ConnectorKind::Mail => ["text/plain", "text/html"],
            ConnectorKind::Docs => ["text/markdown", "text/html"],
        }
    }
}

fn nonempty_str(params: &ConnectorParams, key: &str) -> bool {
    params
        .get(key)
        .and_then(toml::Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
}

/// The kind of a connector spec, after the §2.15 item 1 selection check.
///
/// Errors: `PolicyRefused` for a kind trusty-okf cannot import;
/// `SelectionRequired` when `account` is missing, or a mail store names no
/// `query`, or a docs store names neither `folder_id` nor `document_ids`.
pub fn check_selection(spec: &ImportSpec) -> Result<Option<ConnectorKind>, OkfError> {
    let Locator::Connector { kind, params } = &spec.store.locator else {
        return Ok(None);
    };
    let family = ConnectorKind::of(kind).ok_or_else(|| OkfError::PolicyRefused {
        path: std::path::PathBuf::from(&spec.store.id),
        reason: format!("policy_refused: no import normaliser for connector kind {kind}"),
    })?;
    if !nonempty_str(params, "account") {
        return Err(OkfError::SelectionRequired(format!(
            "store {} names no account label",
            spec.store.id
        )));
    }
    let selected = match family {
        ConnectorKind::Mail => nonempty_str(params, "query"),
        ConnectorKind::Docs => {
            nonempty_str(params, "folder_id")
                || params
                    .get("document_ids")
                    .and_then(toml::Value::as_array)
                    .is_some_and(|ids| !ids.is_empty())
        }
    };
    if !selected {
        let what = match family {
            ConnectorKind::Mail => "a query",
            ConnectorKind::Docs => "a folder_id or document_ids",
        };
        return Err(OkfError::SelectionRequired(format!(
            "store {} names no source selection: {what}",
            spec.store.id
        )));
    }
    Ok(Some(family))
}

/// Whether a run over `spec` can detect deletions: a docs store with a
/// recursive folder listing (Appendix C "Complete listing"). Mail never can
/// (DOC-55 C4).
pub fn deletion_capable(spec: &ImportSpec) -> bool {
    match &spec.store.locator {
        Locator::DocStore { .. } => true,
        Locator::Connector { kind, params } => {
            ConnectorKind::of(kind) == Some(ConnectorKind::Docs)
                && nonempty_str(params, "folder_id")
                && params.get("recursive").and_then(toml::Value::as_bool) == Some(true)
        }
    }
}

/// The concept ID of a listed connector item (§2.15 item 4).
///
/// Errors: `InvalidId` when a mail item has no thread ID or an ID segment
/// breaks §2.11.
pub fn concept_id(
    family: ConnectorKind,
    store: &str,
    entry: &ListEntry,
) -> Result<ConceptId, OkfError> {
    let raw = match family {
        ConnectorKind::Mail => {
            let Some(thread) = entry.group.as_deref().filter(|t| !t.is_empty()) else {
                return Err(OkfError::InvalidId {
                    input: entry.item_id.clone(),
                    rule: 1,
                });
            };
            format!("mail/{store}/{thread}/{}", entry.item_id)
        }
        ConnectorKind::Docs => format!("docs/{store}/{}", entry.item_id),
    };
    // Every segment must be one plain segment: a `/` inside a source ID
    // would add a level.
    let segments = raw.split('/').count();
    let expected = if family == ConnectorKind::Mail { 4 } else { 3 };
    if segments != expected {
        return Err(OkfError::InvalidId {
            input: raw,
            rule: 1,
        });
    }
    ConceptId::parse(&raw)
}

/// The thread concept of a mail item's concept ID.
pub fn thread_of(id: &ConceptId) -> Option<ConceptId> {
    let s = id.as_str();
    let (parent, _) = s.rsplit_once('/')?;
    if s.starts_with("mail/") && parent.split('/').count() == 3 {
        ConceptId::parse(parent).ok()
    } else {
        None
    }
}

/// The content of a normalised item's concept.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalConcept {
    /// The title.
    pub title: String,
    /// The Markdown body.
    pub body: String,
    /// The `source_timestamp`, ISO-8601.
    pub timestamp: Option<String>,
    /// Engine-placed provenance keys.
    pub provenance: Mapping,
    /// Source fields, checked against the protected set.
    pub fields: Mapping,
    /// A recorded loss written with the concept.
    pub degraded: Option<String>,
    /// Adapter notes, reported as findings.
    pub notes: Vec<String>,
}

/// A normalised item, or the reason it yields no concept.
#[derive(Debug, Clone, PartialEq)]
pub enum Normalised {
    /// The concept's content.
    Concept(Box<NormalConcept>),
    /// Read, but no concept is written: a recorded loss (`snippet_only`).
    Degraded(String),
    /// Not read further: a §2.14 rule 8 reason.
    Skip(String),
}

fn y(s: &str) -> Value {
    Value::String(s.to_string())
}

/// Epoch milliseconds as ISO-8601 UTC; other text unchanged.
fn iso_of(ms: &str) -> String {
    ms.parse::<i64>()
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map_or_else(
            || ms.to_string(),
            |d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        )
}

fn decodable(p: &Part) -> bool {
    p.encoding.as_deref().is_none_or(|e| e == "utf8") && !p.data.trim().is_empty()
}

/// Normalise `rec` from a `family` store under `policy` (see the module
/// doc).
pub fn normalise(family: ConnectorKind, rec: &SourceRecord, policy: ContentPolicy) -> Normalised {
    let chosen = family
        .preferred()
        .iter()
        .find_map(|m| rec.parts.iter().find(|p| p.mime == *m && decodable(p)));
    let mut provenance = Mapping::new();
    let mut source = Mapping::new();
    source.insert(y("resource"), y(&rec.item_id));
    provenance.insert(y("sources"), Value::Sequence(vec![Value::Mapping(source)]));
    provenance.insert(y("extracted_by"), y(&family.tag()));
    if let Some(r) = &rec.revision {
        provenance.insert(y("source_revision"), y(r));
    }
    let mut degraded = None;
    let (body, mime) = match (chosen, family) {
        (Some(p), _) => (p.data.clone(), p.mime.clone()),
        (None, ConnectorKind::Docs) => return Normalised::Skip("unsupported_format".into()),
        (None, ConnectorKind::Mail) => match (&rec.snippet, policy) {
            (Some(s), ContentPolicy::AllowSnippet) if !s.trim().is_empty() => {
                degraded = Some("snippet_only".to_string());
                (s.clone(), "snippet".to_string())
            }
            _ => return Normalised::Degraded("snippet_only".into()),
        },
    };
    provenance.insert(y("body_format"), y(&mime));
    if let Some(d) = &degraded {
        provenance.insert(y("degraded"), y(d));
    }
    let timestamp = match family {
        ConnectorKind::Mail => {
            if let Some(t) = &rec.group {
                provenance.insert(y("thread_id"), y(t));
            }
            rec.dates
                .get("internal_date")
                .map(String::as_str)
                .map(iso_of)
        }
        ConnectorKind::Docs => rec.dates.get("modified_time").cloned(),
    };
    for (k, v) in &rec.dates {
        provenance.insert(y(k), y(v));
    }
    let mut text = body.trim_end().to_string();
    if !rec.attachments.is_empty() {
        text.push_str("\n\n## Attachments\n");
        for a in &rec.attachments {
            text.push_str(&format!("\n- `{}` ({}, {} bytes)", a.name, a.mime, a.size));
        }
        text.push('\n');
    }
    let fields = serde_yaml::to_value(&rec.fields)
        .ok()
        .and_then(|v| v.as_mapping().cloned())
        .unwrap_or_default();
    Normalised::Concept(Box::new(NormalConcept {
        title: rec
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| rec.item_id.clone()),
        body: text,
        timestamp,
        provenance,
        fields,
        degraded,
        notes: rec.notes.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(item: &str, group: Option<&str>) -> ListEntry {
        ListEntry {
            item_id: item.into(),
            group: group.map(str::to_string),
            ..ListEntry::default()
        }
    }

    /// Why: §2.15 item 4 — IDs come from immutable source IDs, with one
    /// plain segment each.
    /// What: mail and docs IDs; a missing thread and a `/` in an ID refuse.
    /// Test: self-contained.
    #[test]
    fn concept_ids_follow_the_appendix_c_rule() {
        let mail = concept_id(ConnectorKind::Mail, "s", &entry("m1", Some("t1"))).unwrap();
        assert_eq!(mail.as_str(), "mail/s/t1/m1");
        assert_eq!(thread_of(&mail).unwrap().as_str(), "mail/s/t1");
        let doc = concept_id(ConnectorKind::Docs, "s", &entry("f1", None)).unwrap();
        assert_eq!(doc.as_str(), "docs/s/f1");
        assert!(thread_of(&doc).is_none());
        assert!(concept_id(ConnectorKind::Mail, "s", &entry("m1", None)).is_err());
        assert!(concept_id(ConnectorKind::Docs, "s", &entry("a/b", None)).is_err());
    }

    /// Why: Appendix C — internalDate is epoch milliseconds.
    /// What: a millisecond value converts; other text is kept.
    /// Test: self-contained.
    #[test]
    fn internal_date_converts_from_epoch_millis() {
        assert_eq!(iso_of("1767225600000"), "2026-01-01T00:00:00Z");
        assert_eq!(iso_of("not-a-number"), "not-a-number");
    }
}
