//! Inverse-edge reconciliation — the [`KbStore`] file walk over trusty-okf's
//! pure reconciler.
//!
//! Why: the edge logic lives in `trusty_okf::profile::reconcile`; inverse
//! edges come from the profile. Resolving a link to an entity file and
//! writing it is the [`KbStore`] half, which ingest runs after each item
//! (`crate::kb::okg::ingest`), as trusty-kb's `put_entity` did after every
//! write. #10746 S11 moved it here from the deleted trusty-kb.
//!
//! What: [`KbStore::reconcile_entity`] resolves each `(target, inverse)` pair
//! of a source entity to an existing entity file and unions a `[[source]]`
//! link into it. Dangling links (no target file) are left untouched for
//! validation to report; targets are never fabricated.
//!
//! Test: `reconcile_materialises_symmetric_edge`, `reconcile_materialises_pair`,
//! `reconcile_is_idempotent`, `reconcile_skips_dangling`.

use std::path::PathBuf;

use trusty_okf::format::concept::{Entity, slugify};
use trusty_okf::profile::reconcile::{add_inverse_link, inverse_links, source_link};

use crate::kb::store::KbStore;

impl KbStore {
    /// Materialise inverse edges for one source entity onto its link targets.
    ///
    /// Why/What/Test: see the module doc. Returns the root-relative paths of
    /// the targets that changed, sorted.
    pub fn reconcile_entity(
        &self,
        _collection: &str,
        source_name: &str,
        source: &Entity,
        now: &str,
    ) -> anyhow::Result<Vec<String>> {
        let link = source_link(source_name, source);
        let mut changed: Vec<String> = Vec::new();
        for (target, inverse) in inverse_links(&self.profile, &link, source) {
            if let Some(path) = self.resolve_link(&target)?
                && self.add_inverse_link_at(&path, inverse, &link, now)?
            {
                changed.push(self.rel(&path));
            }
        }
        changed.sort();
        changed.dedup();
        Ok(changed)
    }

    /// Resolve a `[[target]]` link to an existing entity file path, by slug,
    /// searching every collection in deterministic order.
    ///
    /// Test: exercised via `reconcile_materialises_pair`.
    fn resolve_link(&self, target: &str) -> anyhow::Result<Option<PathBuf>> {
        let want = slugify(target);
        for coll in self.collection_dirs_on_disk()? {
            for (slug, path) in self.entity_files(&coll)? {
                if slug == want {
                    return Ok(Some(path));
                }
            }
        }
        Ok(None)
    }

    /// Union a `[[source]]` link into the `field` edge of the entity at `path`.
    /// Returns whether the target file actually changed.
    fn add_inverse_link_at(
        &self,
        path: &std::path::Path,
        field: &str,
        source_link: &str,
        now: &str,
    ) -> anyhow::Result<bool> {
        let Some(mut entity) = self.read_entity_at(path)? else {
            return Ok(false);
        };
        if !add_inverse_link(&mut entity, field, source_link, now) {
            return Ok(false);
        }
        self.write_entity_at(path, &entity)
    }
}

#[cfg(test)]
mod tests {
    use trusty_okf::format::concept::link_values;
    use trusty_okf::profile::Profile;

    use crate::kb::store::KbStore;

    const NOW: &str = "2026-01-01T00:00:00Z";

    fn store() -> (tempfile::TempDir, KbStore) {
        let tmp = tempfile::tempdir().unwrap();
        let s = KbStore::new(tmp.path().to_path_buf(), Profile::default_profile());
        (tmp, s)
    }

    /// Write one entity and reconcile it, as an ingest does per item.
    ///
    /// #10746 S11: trusty-kb's `put_entity` (the `kb_put_entity` MCP write,
    /// deleted with that crate) did both; the edge outcome is the same.
    fn put(s: &KbStore, collection: &str, name: &str, frontmatter: &str) {
        let path = s.entity_path(collection, name).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!("---\ntype: Person\ntitle: {name}\n{frontmatter}---\n\n"),
        )
        .unwrap();
        let entity = s.read_entity_at(&path).unwrap().unwrap();
        s.reconcile_entity(collection, name, &entity, NOW).unwrap();
    }

    /// Why: symmetric verbs mirror the same field on the target.
    /// What: Ada knows Grace; asserts Grace gains a `knows [[Ada]]` edge.
    /// Test: self-contained.
    #[test]
    fn reconcile_materialises_symmetric_edge() {
        let (_t, s) = store();
        put(&s, "people", "Grace", "");
        put(&s, "people", "Ada", "knows: \"[[Grace]]\"\n");
        let grace = s.get_entity("people", "Grace").unwrap().unwrap();
        let links = link_values(grace.frontmatter.get("knows").unwrap());
        assert_eq!(links, vec!["Ada".to_string()]);
    }

    /// Why: bidirectional pairs materialise the paired verb.
    /// What: Ada parent_of Leon; asserts Leon gains `child_of [[Ada]]`.
    /// Test: self-contained.
    #[test]
    fn reconcile_materialises_pair() {
        let (_t, s) = store();
        put(&s, "people", "Leon", "");
        put(&s, "people", "Ada", "parent_of: \"[[Leon]]\"\n");
        let leon = s.get_entity("people", "Leon").unwrap().unwrap();
        let links = link_values(leon.frontmatter.get("child_of").unwrap());
        assert_eq!(links, vec!["Ada".to_string()]);
    }

    /// Why: re-running the reconciler must not churn the target.
    /// What: reconciles Ada a second time at a later instant; asserts the
    /// target file is byte-identical after the second pass.
    /// Test: self-contained.
    #[test]
    fn reconcile_is_idempotent() {
        let (_t, s) = store();
        put(&s, "people", "Grace", "");
        put(&s, "people", "Ada", "knows: \"[[Grace]]\"\n");
        let path = s.entity_path("people", "Grace").unwrap();
        let after_first = std::fs::read_to_string(&path).unwrap();
        let ada = s.get_entity("people", "Ada").unwrap().unwrap();
        s.reconcile_entity("people", "Ada", &ada, "2026-02-02T00:00:00Z")
            .unwrap();
        let after_second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after_first, after_second, "reconcile must be idempotent");
    }

    /// Why: dangling links (no target file) must not fabricate an entity.
    /// What: Ada knows a nonexistent target; asserts no new file appears.
    /// Test: self-contained.
    #[test]
    fn reconcile_skips_dangling() {
        let (_t, s) = store();
        put(&s, "people", "Ada", "knows: \"[[Nobody]]\"\n");
        assert!(s.get_entity("people", "Nobody").unwrap().is_none());
    }
}
