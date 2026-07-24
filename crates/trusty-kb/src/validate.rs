//! `kb_validate` — full-tree lint returning structured findings.
//!
//! Why: a deterministic store needs a deterministic conscience — one place that
//! reports every way the tree violates the OKF container or the profile, so a
//! client can fix issues rather than discover them at read time.
//!
//! What: [`KbStore::validate`] walks every entity (reserved files excluded) and
//! emits [`Finding`]s for: frontmatter parse errors, the missing required `type`
//! field, dangling `[[wiki-links]]` (a link whose target resolves to no entity,
//! by slug/title/alias), slug/filename mismatches (the filename does not match
//! `slugify(title)`), and duplicate aliases shared across entities. Findings are
//! sorted for stable output.
//!
//! Test: `validate_flags_missing_type`, `validate_flags_dangling_link`,
//! `validate_flags_slug_mismatch`, `validate_flags_duplicate_alias`.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;
use serde_yaml::Value;

use crate::entity::{Entity, link_values, slugify, wiki_links};
use crate::schema::inverse_edge;
use crate::store::KbStore;

/// The class of a validation finding.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// Frontmatter could not be parsed as a YAML mapping.
    ParseError,
    /// The required `type` field is absent.
    MissingType,
    /// A `[[wiki-link]]` points at no existing entity.
    DanglingLink,
    /// The filename does not match `slugify(title)`.
    SlugMismatch,
    /// An alias is claimed by more than one entity.
    DuplicateAlias,
}

/// One structured validation finding.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Finding {
    /// Root-relative path of the offending entity file.
    pub path: String,
    /// The finding class.
    pub kind: FindingKind,
    /// Human-readable detail.
    pub message: String,
}

/// A loaded entity with its location.
struct Loaded {
    slug: String,
    path: PathBuf,
    entity: Entity,
}

impl KbStore {
    /// Lint the whole tree.
    ///
    /// Why/What/Test: see the module doc.
    pub fn validate(&self) -> anyhow::Result<Vec<Finding>> {
        let mut findings = Vec::new();
        let mut loaded: Vec<Loaded> = Vec::new();

        for coll in self.collection_dirs_on_disk()? {
            for (slug, path) in self.entity_files(&coll)? {
                match self.read_entity_at(&path) {
                    Ok(Some(entity)) => loaded.push(Loaded { slug, path, entity }),
                    Ok(None) => {}
                    Err(e) => findings.push(Finding {
                        path: self.rel(&path),
                        kind: FindingKind::ParseError,
                        message: format!("frontmatter parse error: {e}"),
                    }),
                }
            }
        }

        // alias_slug -> set of owning entity slugs (for duplicate detection).
        let mut alias_owners: HashMap<String, Vec<String>> = HashMap::new();
        for l in &loaded {
            for alias in aliases(&l.entity) {
                let owners = alias_owners.entry(slugify(&alias)).or_default();
                if !owners.contains(&l.slug) {
                    owners.push(l.slug.clone());
                }
            }
        }

        for l in &loaded {
            self.lint_entity(l, &mut findings)?;
        }

        // Duplicate aliases: an alias slug owned by >1 entity.
        let mut dup_keys: Vec<(&String, &Vec<String>)> = alias_owners
            .iter()
            .filter(|(_, owners)| owners.len() > 1)
            .collect();
        dup_keys.sort_by(|a, b| a.0.cmp(b.0));
        for (alias, owners) in dup_keys {
            for l in loaded.iter().filter(|l| owners.contains(&l.slug)) {
                findings.push(Finding {
                    path: self.rel(&l.path),
                    kind: FindingKind::DuplicateAlias,
                    message: format!(
                        "alias '{alias}' is shared with {} other entit{}",
                        owners.len() - 1,
                        if owners.len() == 2 { "y" } else { "ies" }
                    ),
                });
            }
        }

        findings
            .sort_by(|a, b| (a.path.as_str(), a.kind as u8).cmp(&(b.path.as_str(), b.kind as u8)));
        Ok(findings)
    }

    /// Emit the per-entity findings (missing type, slug mismatch, dangling links).
    fn lint_entity(&self, l: &Loaded, findings: &mut Vec<Finding>) -> anyhow::Result<()> {
        let rel = self.rel(&l.path);
        if l.entity.get_str("type").is_none() {
            findings.push(Finding {
                path: rel.clone(),
                kind: FindingKind::MissingType,
                message: "required field `type` is missing".to_string(),
            });
        }
        if let Some(title) = l.entity.get_str("title") {
            let want = slugify(title);
            if want != l.slug {
                findings.push(Finding {
                    path: rel.clone(),
                    kind: FindingKind::SlugMismatch,
                    message: format!("filename slug '{}' != slugify(title) '{}'", l.slug, want),
                });
            }
        }
        for target in self.entity_links(&l.entity) {
            if self.resolve_link(&target)?.is_none() {
                findings.push(Finding {
                    path: rel.clone(),
                    kind: FindingKind::DanglingLink,
                    message: format!("dangling wiki-link [[{target}]]"),
                });
            }
        }
        Ok(())
    }

    /// All `[[wiki-link]]` targets referenced by an entity — from its edge
    /// frontmatter fields and its body — de-duplicated in stable order.
    fn entity_links(&self, entity: &Entity) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if let Value::Mapping(map) = &entity.frontmatter {
            for (k, v) in map {
                if k.as_str().and_then(inverse_edge).is_some() {
                    out.extend(link_values(v));
                }
            }
        }
        out.extend(wiki_links(&entity.body));
        out.sort();
        out.dedup();
        out
    }
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
    use crate::put::empty_map;
    use crate::schema::Profile;

    fn store() -> (tempfile::TempDir, KbStore) {
        let tmp = tempfile::tempdir().unwrap();
        let s = KbStore::new(tmp.path().to_path_buf(), Profile::default_profile());
        (tmp, s)
    }

    fn write(s: &KbStore, coll: &str, file: &str, content: &str) {
        let dir = s.root.join(coll);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file), content).unwrap();
    }

    /// Why: the missing-`type` class is OKF's one hard requirement.
    /// What: writes an entity with no `type`; asserts a MissingType finding.
    /// Test: self-contained.
    #[test]
    fn validate_flags_missing_type() {
        let (_t, s) = store();
        write(&s, "people", "ada.md", "---\ntitle: Ada\n---\n\nbody\n");
        let findings = s.validate().unwrap();
        assert!(findings.iter().any(|f| f.kind == FindingKind::MissingType));
    }

    /// Why: dangling links break the graph and must be reported.
    /// What: writes an entity linking a nonexistent target; asserts a
    /// DanglingLink finding, and none once the target exists.
    /// Test: self-contained.
    #[test]
    fn validate_flags_dangling_link() {
        let (_t, s) = store();
        write(
            &s,
            "people",
            "ada.md",
            "---\ntype: Person\ntitle: Ada\nknows: \"[[Ghost]]\"\n---\n\nx\n",
        );
        let findings = s.validate().unwrap();
        assert!(findings.iter().any(|f| f.kind == FindingKind::DanglingLink));

        s.put_entity(
            "people",
            "Ghost",
            empty_map(),
            None,
            false,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        let findings2 = s.validate().unwrap();
        assert!(
            !findings2
                .iter()
                .any(|f| f.kind == FindingKind::DanglingLink)
        );
    }

    /// Why: a filename that disagrees with the title breaks slug-based lookup.
    /// What: writes a file whose name mismatches slugify(title); asserts a
    /// SlugMismatch finding.
    /// Test: self-contained.
    #[test]
    fn validate_flags_slug_mismatch() {
        let (_t, s) = store();
        write(
            &s,
            "people",
            "wrongname.md",
            "---\ntype: Person\ntitle: Ada Lovelace\n---\n\nx\n",
        );
        let findings = s.validate().unwrap();
        assert!(findings.iter().any(|f| f.kind == FindingKind::SlugMismatch));
    }

    /// Why: an alias claimed by two entities is an entity-resolution hazard.
    /// What: writes two entities sharing an alias; asserts DuplicateAlias
    /// findings on both.
    /// Test: self-contained.
    #[test]
    fn validate_flags_duplicate_alias() {
        let (_t, s) = store();
        write(
            &s,
            "people",
            "ada.md",
            "---\ntype: Person\ntitle: Ada\naliases: [Countess]\n---\n\nx\n",
        );
        write(
            &s,
            "people",
            "augusta.md",
            "---\ntype: Person\ntitle: Augusta\naliases: [Countess]\n---\n\ny\n",
        );
        let findings = s.validate().unwrap();
        let dups = findings
            .iter()
            .filter(|f| f.kind == FindingKind::DuplicateAlias)
            .count();
        assert_eq!(dups, 2);
    }
}
