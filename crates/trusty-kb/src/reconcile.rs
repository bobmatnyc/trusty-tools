//! Inverse-edge reconciler — materialise the other side of every relationship.
//!
//! Why: the profile declares relationship edges in pairs (parent_of↔child_of,
//! works_at↔employee, …) and symmetric verbs (knows, spouse_of, …). Writing one
//! side should deterministically create the mirror on the target so the link
//! graph is navigable from either endpoint. This post-pass runs after every
//! [`crate::put`] and across the whole tree after [`crate::convert`], and is
//! idempotent: a second run finds the inverse already present and does nothing.
//!
//! What: [`KbStore::reconcile_entity`] walks a source entity's edge fields (any
//! key with an [`inverse_edge`]), resolves each `[[target]]` wiki-link to an
//! existing entity file, and unions a `[[source]]` link into the target's
//! inverse field. [`KbStore::reconcile_all`] applies it to every entity — the
//! converter's link-graph-maintenance step. Dangling links (no target file) are
//! left untouched for [`crate::validate`] to report; targets are never
//! fabricated.
//!
//! Test: `reconcile_materialises_symmetric_edge`, `reconcile_materialises_pair`,
//! `reconcile_is_idempotent`, `reconcile_skips_dangling`.

use std::path::PathBuf;

use serde_yaml::Value;

use crate::entity::{Entity, link_values, slugify};
use crate::schema::inverse_edge;
use crate::store::KbStore;

impl KbStore {
    /// Materialise inverse edges for one source entity onto its link targets.
    ///
    /// Why/What/Test: see the module doc.
    pub fn reconcile_entity(
        &self,
        _collection: &str,
        source_name: &str,
        source: &Entity,
        now: &str,
    ) -> anyhow::Result<Vec<String>> {
        // The link text targets use to point back at the source.
        let source_link = source
            .get_str("title")
            .map(str::to_string)
            .unwrap_or_else(|| source_name.to_string());
        let Value::Mapping(map) = &source.frontmatter else {
            return Ok(Vec::new());
        };

        let mut changed: Vec<String> = Vec::new();
        for (k, v) in map {
            let Some(field) = k.as_str() else { continue };
            let Some(inverse) = inverse_edge(field) else {
                continue;
            };
            for target in link_values(v) {
                // Never create a self-loop record on the same file.
                if slugify(&target) == slugify(&source_link) {
                    continue;
                }
                if let Some(path) = self.resolve_link(&target)?
                    && self.add_inverse_link(&path, inverse, &source_link, now)?
                {
                    changed.push(self.rel(&path));
                }
            }
        }
        changed.sort();
        changed.dedup();
        Ok(changed)
    }

    /// Reconcile every entity in the tree (used after conversion).
    ///
    /// Why: a bulk conversion writes many entities before any reconciliation;
    /// this second pass closes every edge deterministically.
    /// What: iterates collections then entities (both sorted), reconciling each.
    /// Test: exercised via the convert idempotency test.
    pub fn reconcile_all(&self, now: &str) -> anyhow::Result<usize> {
        let mut touched = 0;
        for coll in self.collection_dirs_on_disk()? {
            for (slug, path) in self.entity_files(&coll)? {
                if let Some(entity) = self.read_entity_at(&path)? {
                    let n = self.reconcile_entity(&coll, &slug, &entity, now)?;
                    touched += n.len();
                }
            }
        }
        Ok(touched)
    }

    /// Resolve a `[[target]]` link to an existing entity file path, by slug,
    /// searching every collection in deterministic order.
    ///
    /// Test: exercised via `reconcile_materialises_pair` and the validate
    /// dangling-link test.
    pub(crate) fn resolve_link(&self, target: &str) -> anyhow::Result<Option<PathBuf>> {
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
    fn add_inverse_link(
        &self,
        path: &std::path::Path,
        field: &str,
        source_link: &str,
        now: &str,
    ) -> anyhow::Result<bool> {
        let Some(mut entity) = self.read_entity_at(path)? else {
            return Ok(false);
        };
        let link = format!("[[{source_link}]]");
        let want_slug = slugify(source_link);

        let map = entity.map_mut();
        let key = Value::String(field.to_string());
        let already = map
            .get(&key)
            .map(|v| link_values(v).iter().any(|t| slugify(t) == want_slug))
            .unwrap_or(false);
        if already {
            return Ok(false);
        }
        // Normalise the field to a sorted sequence of link scalars.
        let mut links: Vec<String> = map
            .get(&key)
            .map(|v| link_values(v).iter().map(|t| format!("[[{t}]]")).collect())
            .unwrap_or_default();
        links.push(link);
        links.sort();
        links.dedup();
        map.insert(
            key,
            Value::Sequence(links.into_iter().map(Value::String).collect()),
        );
        // Touch `updated` since the target's content changed.
        map.insert(
            Value::String("updated".into()),
            Value::String(now.to_string()),
        );
        self.write_entity_at(path, &entity)
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

    fn fm(y: &str) -> Value {
        serde_yaml::from_str(y).unwrap()
    }

    /// Why: symmetric verbs mirror the same field on the target.
    /// What: Ada knows Grace; asserts Grace gains a `knows [[Ada]]` edge.
    /// Test: self-contained.
    #[test]
    fn reconcile_materialises_symmetric_edge() {
        let (_t, s) = store();
        s.put_entity(
            "people",
            "Grace",
            empty_map(),
            None,
            false,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        s.put_entity(
            "people",
            "Ada",
            fm("knows: \"[[Grace]]\"\n"),
            None,
            false,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
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
        s.put_entity(
            "people",
            "Leon",
            empty_map(),
            None,
            false,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        s.put_entity(
            "people",
            "Ada",
            fm("parent_of: \"[[Leon]]\"\n"),
            None,
            false,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        let leon = s.get_entity("people", "Leon").unwrap().unwrap();
        let links = link_values(leon.frontmatter.get("child_of").unwrap());
        assert_eq!(links, vec!["Ada".to_string()]);
    }

    /// Why: re-running the reconciler must not churn the target.
    /// What: reconciles twice; asserts the target file is byte-identical after
    /// the second pass.
    /// Test: self-contained.
    #[test]
    fn reconcile_is_idempotent() {
        let (_t, s) = store();
        s.put_entity(
            "people",
            "Grace",
            empty_map(),
            None,
            false,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        s.put_entity(
            "people",
            "Ada",
            fm("knows: \"[[Grace]]\"\n"),
            None,
            false,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        let path = s.entity_path("people", "Grace").unwrap();
        let after_first = std::fs::read_to_string(&path).unwrap();
        s.reconcile_all("2026-02-02T00:00:00Z").unwrap();
        let after_second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after_first, after_second, "reconcile must be idempotent");
    }

    /// Why: dangling links (no target file) must not fabricate an entity.
    /// What: Ada knows a nonexistent target; asserts no new file appears.
    /// Test: self-contained.
    #[test]
    fn reconcile_skips_dangling() {
        let (_t, s) = store();
        s.put_entity(
            "people",
            "Ada",
            fm("knows: \"[[Nobody]]\"\n"),
            None,
            false,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        assert!(s.get_entity("people", "Nobody").unwrap().is_none());
    }
}
