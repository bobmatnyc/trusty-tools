//! Validation findings and tree lint (moved from trusty-kb `validate.rs`,
//! #10746).
//!
//! Why: a deterministic store needs a deterministic conscience — one place that
//! reports every way the tree violates the OKF container or the profile, so a
//! client can fix issues rather than discover them at read time. SPEC-OKF-01
//! §2.2 adds three severities: an error breaches OKF §11, a warning is a
//! §5-§10 field family with the wrong shape, and an info is something §11 says
//! consumers must accept.
//!
//! What: [`Finding`] with [`Severity`] and [`FindingKind`];
//! [`conformance_findings`] for the per-concept §11 check a strict write runs;
//! [`lint_entities`] for the tree lint over loaded entities: the missing
//! required `type` field, dangling `[[wiki-links]]` (a link whose target
//! resolves to no entity, by slug), slug/filename mismatches (the filename
//! does not match `slugify(title)`), and duplicate aliases shared across
//! entities. [`sort_findings`] gives stable output. The file walk that loads
//! the entities belongs to the caller (`Bundle` from slice S3; trusty-kb's
//! `KbStore::validate` until then).
//!
//! Test: `lint_flags_missing_type_and_dangling_link`,
//! `lint_flags_slug_mismatch_and_duplicate_alias`,
//! `conformance_requires_non_empty_type`.

use std::collections::{BTreeSet, HashMap};

use serde::Serialize;
use serde_yaml::Value;

use crate::format::concept::{Entity, link_values, slugify, wiki_links};
use crate::format::fields::{Field, Frontmatter};
use crate::format::id::ConceptId;
use crate::profile::Profile;

/// How serious a finding is (SPEC-OKF-01 §2.2).
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// An OKF §11 conformance breach. Refuses a write under `StrictErrors`.
    Error,
    /// A §5-§10 field family with the wrong shape.
    Warning,
    /// Something OKF §11 says consumers must accept.
    Info,
}

/// The class of a validation finding.
///
/// The first five variants keep trusty-kb's order, which `sort_findings` uses.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// Frontmatter could not be parsed as a YAML mapping.
    ParseError,
    /// The required `type` field is absent, empty or not a string.
    MissingType,
    /// A `[[wiki-link]]` points at no existing entity.
    DanglingLink,
    /// The filename does not match `slugify(title)`.
    SlugMismatch,
    /// An alias is claimed by more than one entity.
    DuplicateAlias,
    /// A known key holds a value without the OKF shape; the value is kept.
    MalformedField,
    /// `status` holds a value OKF §5.4 does not define; the value is kept.
    UnknownStatus,
    /// A legacy value §2.8 cannot map stays under its legacy key.
    UnmappedLegacyValue,
    /// A `.md` file whose path fails the §2.11 ID rule; read-only.
    ForeignPath,
    /// #10746 S3: a malformed owned `verified`/`generated` value moved to its
    /// `legacy_` key with its raw bytes (§2.2.1).
    LegacyValueMoved,
    /// #10746 S3: a symbolic link in the bundle; not followed, not read
    /// (§2.11 file system rule 5).
    SymlinkSkipped,
    /// #10746 S3: two existing files share one §2.11 fold key.
    FoldCollision,
    /// #10746 S3: an `index.md` without a generator marker; left unchanged
    /// (§2.3).
    HandWrittenIndex,
    /// #10746 S3: the content changed since its recorded verification; the
    /// verification state reads Unverified (§2.10 "Out-of-band edits").
    VerificationMismatch,
    /// #10746 S3: a link `move_concept` left unchanged (§2.11 link rule 2, or
    /// a foreign or unparseable file).
    LinkNotRewritten,
    /// #10746 S3: a file in the bundle that could not be read as text.
    UnreadableFile,
    /// #10746 S5: a legacy `_sources/<id>.index.jsonl`; left unchanged and
    /// never read as index state (§4.9.1 rule 4).
    LegacyIndexJournal,
    /// #10746 S5: an incomplete operation whose files changed outside
    /// trusty-okf; nothing was overwritten (§4.11 rule 2).
    RecoveryConflict,
    /// #10746 S5: an incomplete operation, reported by `Bundle::open`
    /// without the §4.4 lock; `Bundle::recover_interrupted` finishes it.
    InterruptedOperation,
    /// #10746 S5: an operation completed but its staging directory under
    /// `_sources/ops/` was not removed; the next recovery removes it.
    StagingLeft,
    /// #10746 S5: another writer holds the bundle lock, so `Bundle::open`
    /// did not recover (§4.11 "Lock held elsewhere").
    RecoveryDeferred,
}

/// One structured validation finding.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Finding {
    /// How serious it is.
    pub severity: Severity,
    /// The finding class.
    pub kind: FindingKind,
    /// Root-relative path of the file. #10746: not in the SPEC-OKF-01 §4.2
    /// sketch; a foreign file has no concept ID, and trusty-kb's
    /// `kb_validate` output keys on `path`.
    pub path: String,
    /// The concept, when the file has a valid ID and the caller knows it.
    pub concept: Option<ConceptId>,
    /// Human-readable detail.
    pub message: String,
    /// The spec section the finding cites.
    pub spec_ref: &'static str,
}

impl Finding {
    /// A finding with no concept set.
    pub fn new(
        severity: Severity,
        kind: FindingKind,
        path: String,
        message: String,
        spec_ref: &'static str,
    ) -> Finding {
        Finding {
            severity,
            kind,
            path,
            concept: None,
            message,
            spec_ref,
        }
    }

    /// A parse-error finding for a file whose frontmatter does not parse.
    pub fn parse_error(path: String, message: String) -> Finding {
        Finding {
            severity: Severity::Error,
            kind: FindingKind::ParseError,
            path,
            concept: None,
            message,
            spec_ref: "OKF §11",
        }
    }
}

/// A loaded entity with its location, as the caller's tree walk found it.
#[derive(Debug, Clone)]
pub struct LoadedEntity {
    /// The filename without `.md`.
    pub slug: String,
    /// Root-relative path of the file.
    pub path: String,
    /// The parsed entity.
    pub entity: Entity,
}

/// The OKF §11 findings for one concept's frontmatter.
///
/// Why: a strict write refuses a concept that breaks §11 (§2.2); this is the
/// check it runs.
/// What: an error when `type` is absent, empty or not a string. Malformed
/// optional families are read-time warnings from
/// [`Frontmatter::from_mapping`], not repeated here.
/// Test: `conformance_requires_non_empty_type`.
pub fn conformance_findings(fm: &Frontmatter, path: &str) -> Vec<Finding> {
    let ok = matches!(&fm.r#type, Field::Valid(t) if !t.trim().is_empty());
    if ok {
        return Vec::new();
    }
    vec![Finding {
        severity: Severity::Error,
        kind: FindingKind::MissingType,
        path: path.to_string(),
        concept: None,
        message: "required field `type` is missing, empty or not a string".to_string(),
        spec_ref: "OKF §11",
    }]
}

/// Lint loaded entities as a tree.
///
/// Why/What: see the module doc. `known_slugs` is every entity filename slug
/// the walk saw, including files whose frontmatter did not parse, so a link
/// to an unparseable file is not dangling.
/// Test: `lint_flags_missing_type_and_dangling_link`,
/// `lint_flags_slug_mismatch_and_duplicate_alias`.
pub fn lint_entities(
    profile: &Profile,
    loaded: &[LoadedEntity],
    known_slugs: &BTreeSet<String>,
) -> Vec<Finding> {
    let mut findings = Vec::new();

    // alias_slug -> set of owning entity slugs (for duplicate detection).
    let mut alias_owners: HashMap<String, Vec<String>> = HashMap::new();
    for l in loaded {
        for alias in aliases(&l.entity) {
            let owners = alias_owners.entry(slugify(&alias)).or_default();
            if !owners.contains(&l.slug) {
                owners.push(l.slug.clone());
            }
        }
    }

    for l in loaded {
        lint_entity(profile, l, known_slugs, &mut findings);
    }

    // Duplicate aliases: an alias slug owned by >1 entity.
    let mut dup_keys: Vec<(&String, &Vec<String>)> = alias_owners
        .iter()
        .filter(|(_, owners)| owners.len() > 1)
        .collect();
    dup_keys.sort_by(|a, b| a.0.cmp(b.0));
    for (alias, owners) in dup_keys {
        for l in loaded.iter().filter(|l| owners.contains(&l.slug)) {
            findings.push(info(
                &l.path,
                FindingKind::DuplicateAlias,
                format!(
                    "alias '{alias}' is shared with {} other entit{}",
                    owners.len() - 1,
                    if owners.len() == 2 { "y" } else { "ies" }
                ),
            ));
        }
    }
    findings
}

/// Sort findings by path, then kind, for stable output.
pub fn sort_findings(findings: &mut [Finding]) {
    findings.sort_by(|a, b| (a.path.as_str(), a.kind as u8).cmp(&(b.path.as_str(), b.kind as u8)));
}

/// A profile-level finding: neither an OKF §11 breach nor a §5-§10 shape.
fn info(path: &str, kind: FindingKind, message: String) -> Finding {
    Finding {
        severity: Severity::Info,
        kind,
        path: path.to_string(),
        concept: None,
        message,
        spec_ref: "SPEC-OKF-01 §2.2",
    }
}

/// Emit the per-entity findings (missing type, slug mismatch, dangling links).
fn lint_entity(
    profile: &Profile,
    l: &LoadedEntity,
    known_slugs: &BTreeSet<String>,
    findings: &mut Vec<Finding>,
) {
    if l.entity.get_str("type").is_none() {
        findings.push(Finding {
            severity: Severity::Error,
            kind: FindingKind::MissingType,
            path: l.path.clone(),
            concept: None,
            message: "required field `type` is missing".to_string(),
            spec_ref: "OKF §11",
        });
    }
    if let Some(title) = l.entity.get_str("title") {
        let want = slugify(title);
        if want != l.slug {
            findings.push(info(
                &l.path,
                FindingKind::SlugMismatch,
                format!("filename slug '{}' != slugify(title) '{}'", l.slug, want),
            ));
        }
    }
    for target in entity_links(profile, &l.entity) {
        if !known_slugs.contains(&slugify(&target)) {
            findings.push(info(
                &l.path,
                FindingKind::DanglingLink,
                format!("dangling wiki-link [[{target}]]"),
            ));
        }
    }
}

/// All `[[wiki-link]]` targets referenced by an entity — from its edge
/// frontmatter fields and its body — de-duplicated in stable order.
fn entity_links(profile: &Profile, entity: &Entity) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Value::Mapping(map) = &entity.frontmatter {
        for (k, v) in map {
            if k.as_str().and_then(|f| profile.inverse_of(f)).is_some() {
                out.extend(link_values(v));
            }
        }
    }
    out.extend(wiki_links(&entity.body));
    out.sort();
    out.dedup();
    out
}

/// The `aliases` list of an entity as strings.
fn aliases(entity: &Entity) -> Vec<String> {
    match entity.frontmatter.get("aliases") {
        Some(Value::Sequence(seq)) => seq
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        Some(Value::String(s)) => vec![s.to_string()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(slug: &str, content: &str) -> LoadedEntity {
        LoadedEntity {
            slug: slug.to_string(),
            path: format!("people/{slug}.md"),
            entity: Entity::from_content(content).unwrap(),
        }
    }

    fn slugs(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// Why: the missing-`type` class is OKF's one hard requirement, and a
    /// dangling link breaks the graph.
    /// What: lints an entity with no `type` and a link to a missing target;
    /// asserts both findings with their severities, and no dangling finding
    /// once the target slug is known.
    /// Test: self-contained.
    #[test]
    fn lint_flags_missing_type_and_dangling_link() {
        let profile = Profile::default_profile();
        let ada = loaded("ada", "---\ntitle: Ada\nknows: \"[[Ghost]]\"\n---\n\nx\n");
        let f = lint_entities(&profile, std::slice::from_ref(&ada), &slugs(&["ada"]));
        assert!(
            f.iter()
                .any(|x| x.kind == FindingKind::MissingType && x.severity == Severity::Error)
        );
        assert!(
            f.iter()
                .any(|x| x.kind == FindingKind::DanglingLink && x.severity == Severity::Info)
        );
        let f = lint_entities(&profile, &[ada], &slugs(&["ada", "ghost"]));
        assert!(!f.iter().any(|x| x.kind == FindingKind::DanglingLink));
    }

    /// Why: a filename that disagrees with the title breaks slug lookup, and a
    /// shared alias is an entity-resolution hazard.
    /// What: asserts one SlugMismatch and two DuplicateAlias findings.
    /// Test: self-contained.
    #[test]
    fn lint_flags_slug_mismatch_and_duplicate_alias() {
        let profile = Profile::default_profile();
        let a = loaded(
            "wrongname",
            "---\ntype: Person\ntitle: Ada\naliases: [Countess]\n---\n",
        );
        let b = loaded(
            "augusta",
            "---\ntype: Person\ntitle: Augusta\naliases: [Countess]\n---\n",
        );
        let mut f = lint_entities(&profile, &[a, b], &slugs(&["wrongname", "augusta"]));
        sort_findings(&mut f);
        assert_eq!(
            f.iter()
                .filter(|x| x.kind == FindingKind::SlugMismatch)
                .count(),
            1
        );
        assert_eq!(
            f.iter()
                .filter(|x| x.kind == FindingKind::DuplicateAlias)
                .count(),
            2
        );
    }

    /// Why: criterion 3's refusal rests on this check.
    /// What: absent, empty and list-valued `type` each give one error; a
    /// string `type` gives none.
    /// Test: self-contained.
    #[test]
    fn conformance_requires_non_empty_type() {
        for yaml in ["title: x\n", "type: ''\n", "type: [a, b]\n"] {
            let Value::Mapping(m) = serde_yaml::from_str::<Value>(yaml).unwrap() else {
                panic!("mapping")
            };
            let (fm, _) = Frontmatter::from_mapping(&m, "a.md");
            assert_eq!(conformance_findings(&fm, "a.md").len(), 1, "{yaml}");
        }
        let Value::Mapping(m) = serde_yaml::from_str::<Value>("type: Metric\n").unwrap() else {
            panic!("mapping")
        };
        let (fm, _) = Frontmatter::from_mapping(&m, "a.md");
        assert!(conformance_findings(&fm, "a.md").is_empty());
    }
}
