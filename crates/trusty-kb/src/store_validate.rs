//! `kb_validate` — the [`KbStore`] file walk over trusty-okf's tree lint.
//!
//! Why: #10746 moved the findings and lint rules to
//! `trusty_okf::format::validate` (re-exported here as [`crate::validate`]).
//! The walk over collection directories stays with [`KbStore`] until slice S3
//! gives trusty-okf its own `Bundle`, and S11 removes this crate.
//!
//! What: [`KbStore::validate`] loads every entity (reserved files excluded),
//! reports a parse error for each file whose frontmatter does not parse, and
//! hands the rest to `lint_entities`: the missing required `type` field,
//! dangling `[[wiki-links]]`, slug/filename mismatches and duplicate aliases.
//! Findings are sorted for stable output.
//!
//! Test: `validate_flags_missing_type`, `validate_flags_dangling_link`,
//! `validate_flags_slug_mismatch`, `validate_flags_duplicate_alias`.

use std::collections::BTreeSet;

use crate::store::KbStore;
use crate::validate::{Finding, LoadedEntity, lint_entities, sort_findings};

impl KbStore {
    /// Lint the whole tree.
    ///
    /// Why/What/Test: see the module doc.
    pub fn validate(&self) -> anyhow::Result<Vec<Finding>> {
        let mut findings = Vec::new();
        let mut loaded: Vec<LoadedEntity> = Vec::new();
        let mut known_slugs: BTreeSet<String> = BTreeSet::new();

        for coll in self.collection_dirs_on_disk()? {
            for (slug, path) in self.entity_files(&coll)? {
                // A link to an unparseable file still resolves, as before.
                known_slugs.insert(slug.clone());
                match self.read_entity_at(&path) {
                    Ok(Some(entity)) => loaded.push(LoadedEntity {
                        slug,
                        path: self.rel(&path),
                        entity,
                    }),
                    Ok(None) => {}
                    Err(e) => findings.push(Finding::parse_error(
                        self.rel(&path),
                        format!("frontmatter parse error: {e}"),
                    )),
                }
            }
        }

        findings.extend(lint_entities(&self.profile, &loaded, &known_slugs));
        sort_findings(&mut findings);
        Ok(findings)
    }
}

#[cfg(test)]
mod tests {
    use crate::put::empty_map;
    use crate::schema::Profile;
    use crate::store::KbStore;
    use crate::validate::FindingKind;

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
