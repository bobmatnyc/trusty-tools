//! Typed read-only view of OKF v0.2 frontmatter (SPEC-OKF-01 §2.2.1, §2.4,
//! §4.2), plus the derived trust tier and staleness.
//!
//! Why: callers need `generated`, `verified`, `status` and `stale_after` as
//! types, but a value the types cannot hold must never be lost or rejected.
//! Each known key therefore reads as absent, valid or malformed, and a
//! malformed value keeps its YAML value and earns a warning.
//! What: [`Frontmatter::from_mapping`] builds the view and its findings;
//! [`trust_tier`] applies OKF §5.3; [`staleness`] applies OKF §5.5, with a
//! date-only or offset-less `stale_after` reading as unknown.
//! Test: `c04_bare_verified_mapping_is_one_element_list`,
//! `c05_trust_tier_follows_okf_5_3`,
//! `c06_staleness_unknown_for_date_only_or_offsetless`.

use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, FixedOffset};
use serde_yaml::{Mapping, Value};

use crate::format::validate::{Finding, FindingKind, Severity};

/// A known key's value: absent, valid, or kept as-is because it is malformed.
#[derive(Debug, Clone, PartialEq)]
pub enum Field<T> {
    /// The key is not present.
    Absent,
    /// The value has the OKF shape.
    Valid(T),
    /// The value is kept unchanged; reading it produced a warning.
    Malformed(Value),
}

impl<T> Field<T> {
    /// The valid value, if any.
    pub fn valid(&self) -> Option<&T> {
        match self {
            Field::Valid(v) => Some(v),
            _ => None,
        }
    }
}

/// An OKF §7 actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    /// `<producer>/<version>`.
    Producer {
        /// The producer.
        name: String,
        /// The version.
        version: String,
    },
    /// `human:<id>`.
    Human(String),
    /// `process:<id>`.
    Process(String),
    /// Any other string, kept verbatim.
    Other(String),
}

impl Actor {
    /// Parse an actor string. Never fails: an unrecognised form is `Other`.
    pub fn parse(s: &str) -> Actor {
        if let Some(id) = s.strip_prefix("human:") {
            Actor::Human(id.to_string())
        } else if let Some(id) = s.strip_prefix("process:") {
            Actor::Process(id.to_string())
        } else if let Some((name, version)) = s.split_once('/') {
            Actor::Producer {
                name: name.to_string(),
                version: version.to_string(),
            }
        } else {
            Actor::Other(s.to_string())
        }
    }
}

impl fmt::Display for Actor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Actor::Producer { name, version } => write!(f, "{name}/{version}"),
            Actor::Human(id) => write!(f, "human:{id}"),
            Actor::Process(id) => write!(f, "process:{id}"),
            Actor::Other(s) => f.write_str(s),
        }
    }
}

/// One `sources[]` entry (OKF §5.1).
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    /// The asset or scope descriptor. Required within an entry.
    pub resource: String,
    /// The attribution key.
    pub id: Option<String>,
    /// A display label.
    pub title: Option<String>,
    /// Who produced the source (an actor string).
    pub author: Option<String>,
}

/// `generated: {by, at}` (OKF §5.2). Only `by` is required.
#[derive(Debug, Clone, PartialEq)]
pub struct Generated {
    /// The producer of the current content.
    pub by: Actor,
    /// When the content last changed meaningfully.
    pub at: Option<DateTime<FixedOffset>>,
}

/// One `verified[]` event (OKF §5.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Verification {
    /// Who verified.
    pub by: Actor,
    /// When.
    pub at: DateTime<FixedOffset>,
    /// A new `stale_after` a `verify` sets in the same write (§2.4). Never
    /// read from `verified[]`; always `None` in a parsed view.
    pub stale_after: Option<DateTime<FixedOffset>>,
}

/// `status` (OKF §5.4). An unknown value is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// `draft`.
    Draft,
    /// `stable`, also the meaning of an absent key.
    Stable,
    /// `deprecated`.
    Deprecated,
    /// Any other string, read with a warning.
    Other(String),
}

/// The OKF §5.3 trust tier. Derived only; never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustTier {
    /// No verification event.
    Unverified,
    /// Verified by non-`human:` actors only.
    MachineConfirmed,
    /// Verified by at least one `human:` actor.
    HumanReviewed,
}

/// Whether a concept is past its `stale_after` instant (OKF §5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Staleness {
    /// No `stale_after`, or `now` is before it.
    Fresh,
    /// `now` is on or after `stale_after`.
    Stale,
    /// `stale_after` is present but not an instant with an explicit offset.
    Unknown,
}

/// The typed read-only view of a concept's frontmatter (SPEC-OKF-01 §4.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Frontmatter {
    /// `type`, the one required key.
    pub r#type: Field<String>,
    /// `title`.
    pub title: Field<String>,
    /// `description`.
    pub description: Field<String>,
    /// `resource`.
    pub resource: Field<String>,
    /// `tags`.
    pub tags: Field<Vec<String>>,
    /// `sources`; a plain-string entry (trusty-kb v0.1) reads as malformed.
    pub sources: Field<Vec<Field<Source>>>,
    /// `generated`.
    pub generated: Field<Generated>,
    /// `verified`; a bare mapping reads as a one-element list.
    pub verified: Field<Vec<Field<Verification>>>,
    /// `status`.
    pub status: Field<Status>,
    /// `stale_after`; must be an instant with an explicit offset.
    pub stale_after: Field<DateTime<FixedOffset>>,
    /// `supersedes`.
    pub supersedes: Field<String>,
    /// `superseded_by`.
    pub superseded_by: Field<String>,
    /// Every other key, unknown and legacy, unchanged.
    pub extra: BTreeMap<String, Value>,
}

/// The keys [`Frontmatter`] types; every other key lands in `extra`.
pub const KNOWN_KEYS: [&str; 12] = [
    "type",
    "title",
    "description",
    "resource",
    "tags",
    "sources",
    "generated",
    "verified",
    "status",
    "stale_after",
    "supersedes",
    "superseded_by",
];

/// Parse an ISO 8601 instant that carries an explicit offset.
pub fn parse_instant(s: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(s.trim()).ok()
}

fn string_field(v: Option<&Value>) -> Field<String> {
    match v {
        None => Field::Absent,
        Some(Value::String(s)) => Field::Valid(s.clone()),
        Some(other) => Field::Malformed(other.clone()),
    }
}

fn str_of(m: &Mapping, key: &str) -> Option<String> {
    m.get(key).and_then(Value::as_str).map(str::to_string)
}

fn source_of(v: &Value) -> Field<Source> {
    let Value::Mapping(m) = v else {
        return Field::Malformed(v.clone());
    };
    let Some(resource) = str_of(m, "resource") else {
        return Field::Malformed(v.clone());
    };
    Field::Valid(Source {
        resource,
        id: str_of(m, "id"),
        title: str_of(m, "title"),
        author: str_of(m, "author"),
    })
}

fn generated_of(v: &Value) -> Field<Generated> {
    let Value::Mapping(m) = v else {
        return Field::Malformed(v.clone());
    };
    let Some(by) = str_of(m, "by") else {
        return Field::Malformed(v.clone());
    };
    let at = match m.get("at") {
        None => None,
        Some(Value::String(s)) => match parse_instant(s) {
            Some(t) => Some(t),
            None => return Field::Malformed(v.clone()),
        },
        Some(_) => return Field::Malformed(v.clone()),
    };
    Field::Valid(Generated {
        by: Actor::parse(&by),
        at,
    })
}

fn verification_of(v: &Value) -> Field<Verification> {
    let Value::Mapping(m) = v else {
        return Field::Malformed(v.clone());
    };
    match (str_of(m, "by"), str_of(m, "at").as_deref().and_then(parse_instant)) {
        (Some(by), Some(at)) => Field::Valid(Verification {
            by: Actor::parse(&by),
            at,
            stale_after: None,
        }),
        _ => Field::Malformed(v.clone()),
    }
}

impl Frontmatter {
    /// Build the typed view of a frontmatter mapping.
    ///
    /// Why: a read never fails on a finding (§4.4); a value the types cannot
    /// hold is kept and reported.
    /// What: returns the view and one warning per malformed known key or
    /// sub-entry, one per unknown `status` value. `path` names the file in
    /// each finding. Missing `type` is a write-time check
    /// ([`super::validate::conformance_findings`]), not a read finding here.
    /// Test: `c114_unchanged_put_writes_zero_bytes_and_reads_malformed`.
    pub fn from_mapping(m: &Mapping, path: &str) -> (Frontmatter, Vec<Finding>) {
        let tags = match m.get("tags") {
            None => Field::Absent,
            Some(Value::Sequence(seq)) if seq.iter().all(Value::is_string) => Field::Valid(
                seq.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
            ),
            Some(other) => Field::Malformed(other.clone()),
        };
        let sources = match m.get("sources") {
            None => Field::Absent,
            Some(Value::Sequence(seq)) => Field::Valid(seq.iter().map(source_of).collect()),
            Some(other) => Field::Malformed(other.clone()),
        };
        let verified = match m.get("verified") {
            None => Field::Absent,
            Some(Value::Sequence(seq)) => Field::Valid(seq.iter().map(verification_of).collect()),
            // OKF §5.2 / §11: a bare mapping is a one-element list.
            Some(one @ Value::Mapping(_)) => Field::Valid(vec![verification_of(one)]),
            Some(other) => Field::Malformed(other.clone()),
        };
        let status = match m.get("status") {
            None => Field::Absent,
            Some(Value::String(s)) => Field::Valid(match s.as_str() {
                "draft" => Status::Draft,
                "stable" => Status::Stable,
                "deprecated" => Status::Deprecated,
                other => Status::Other(other.to_string()),
            }),
            Some(other) => Field::Malformed(other.clone()),
        };
        let stale_after = match m.get("stale_after") {
            None => Field::Absent,
            Some(v @ Value::String(s)) => match parse_instant(s) {
                Some(t) => Field::Valid(t),
                None => Field::Malformed(v.clone()),
            },
            Some(other) => Field::Malformed(other.clone()),
        };
        let fm = Frontmatter {
            r#type: string_field(m.get("type")),
            title: string_field(m.get("title")),
            description: string_field(m.get("description")),
            resource: string_field(m.get("resource")),
            tags,
            sources,
            generated: m.get("generated").map_or(Field::Absent, generated_of),
            verified,
            status,
            stale_after,
            supersedes: string_field(m.get("supersedes")),
            superseded_by: string_field(m.get("superseded_by")),
            extra: m
                .iter()
                .filter_map(|(k, v)| {
                    let key = match k {
                        Value::String(s) => s.clone(),
                        other => serde_yaml::to_string(other).unwrap_or_default().trim().to_string(),
                    };
                    (!KNOWN_KEYS.contains(&key.as_str())).then_some((key, v.clone()))
                })
                .collect(),
        };
        let findings = fm.read_findings(path);
        (fm, findings)
    }

    /// The warnings a read of this view produces.
    fn read_findings(&self, path: &str) -> Vec<Finding> {
        let mut out = Vec::new();
        let mut warn = |kind: FindingKind, key: &str, spec_ref: &'static str| {
            out.push(Finding {
                severity: Severity::Warning,
                kind,
                path: path.to_string(),
                concept: None,
                message: format!("`{key}` does not have the OKF shape; value kept unchanged"),
                spec_ref,
            })
        };
        let malformed = FindingKind::MalformedField;
        // A malformed `type` warns here and is an error at write time.
        for (key, bad) in [
            ("type", matches!(self.r#type, Field::Malformed(_))),
            ("title", matches!(self.title, Field::Malformed(_))),
            ("description", matches!(self.description, Field::Malformed(_))),
            ("resource", matches!(self.resource, Field::Malformed(_))),
            ("tags", matches!(self.tags, Field::Malformed(_))),
            ("supersedes", matches!(self.supersedes, Field::Malformed(_))),
            ("superseded_by", matches!(self.superseded_by, Field::Malformed(_))),
        ] {
            if bad {
                warn(malformed, key, "OKF §4.1");
            }
        }
        match &self.sources {
            Field::Malformed(_) => warn(malformed, "sources", "OKF §5.1"),
            Field::Valid(list) if list.iter().any(|s| matches!(s, Field::Malformed(_))) => {
                warn(malformed, "sources[]", "OKF §5.1")
            }
            _ => {}
        }
        if matches!(self.generated, Field::Malformed(_)) {
            warn(malformed, "generated", "OKF §5.2");
        }
        match &self.verified {
            Field::Malformed(_) => warn(malformed, "verified", "OKF §5.2"),
            Field::Valid(list) if list.iter().any(|v| matches!(v, Field::Malformed(_))) => {
                warn(malformed, "verified[]", "OKF §5.2")
            }
            _ => {}
        }
        match &self.status {
            Field::Malformed(_) => warn(malformed, "status", "OKF §5.4"),
            Field::Valid(Status::Other(_)) => warn(FindingKind::UnknownStatus, "status", "OKF §5.4"),
            _ => {}
        }
        if matches!(self.stale_after, Field::Malformed(_)) {
            warn(malformed, "stale_after", "OKF §5.5; SPEC-OKF-01 §2.4");
        }
        out
    }
}

/// The OKF §5.3 trust tier of a frontmatter view.
///
/// Why: the tier is derived and never stored (SPEC-OKF-01 §2.4).
/// What: no verification event is `Unverified`; events by non-`human:`
/// actors only are `MachineConfirmed`; any `human:` actor is
/// `HumanReviewed`. A malformed event still counts through its `by` string,
/// because the tier reads the raw frontmatter (criterion 36). A malformed
/// `verified` value with no event, such as `verified: "yes"`, is
/// `Unverified`.
/// Test: `c05_trust_tier_follows_okf_5_3`.
pub fn trust_tier(f: &Frontmatter) -> TrustTier {
    let Field::Valid(events) = &f.verified else {
        return TrustTier::Unverified;
    };
    let actors: Vec<Actor> = events
        .iter()
        .filter_map(|e| match e {
            Field::Valid(v) => Some(v.by.clone()),
            Field::Malformed(Value::Mapping(m)) => str_of(m, "by").map(|s| Actor::parse(&s)),
            _ => None,
        })
        .collect();
    if actors.iter().any(|a| matches!(a, Actor::Human(_))) {
        TrustTier::HumanReviewed
    } else if actors.is_empty() {
        TrustTier::Unverified
    } else {
        TrustTier::MachineConfirmed
    }
}

/// Whether the concept is stale at `now` (OKF §5.5).
///
/// What: absent `stale_after` is `Fresh`; a valid instant compares with
/// `now`; a malformed value, for example date-only or offset-less, is
/// `Unknown` (SPEC-OKF-01 §2.4). The read that parsed it reported a warning.
/// Test: `c06_staleness_unknown_for_date_only_or_offsetless`.
pub fn staleness(f: &Frontmatter, now: DateTime<FixedOffset>) -> Staleness {
    match &f.stale_after {
        Field::Absent => Staleness::Fresh,
        Field::Valid(t) if now >= *t => Staleness::Stale,
        Field::Valid(_) => Staleness::Fresh,
        Field::Malformed(_) => Staleness::Unknown,
    }
}
