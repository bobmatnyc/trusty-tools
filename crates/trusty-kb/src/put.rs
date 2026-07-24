//! `kb_put_entity` — deterministic create-or-merge of one entity file.
//!
//! Why: this is the mutating heart of the store, so it carries the strongest
//! determinism guarantees: the same inputs against the same state produce a
//! byte-identical file (sorted keys, change-detected `updated`), merge never
//! drops an existing key (OKF's unknown-key rule), and body text is appended
//! under a dated section idempotently. After the write it runs the inverse-edge
//! reconciler so relationship edges are materialised on both endpoints.
//!
//! What: [`KbStore::put_entity`] resolves the confined path, merges (or
//! overwrites) frontmatter + body, stamps `type`/`title`/`created`/`updated`
//! per OKF (only `type` is ever defaulted, to the collection's schema.org type
//! or `Note`), writes only on a real content change, and reconciles.
//!
//! Test: `put_is_byte_stable_on_repeat`, `merge_preserves_existing_keys`,
//! `updated_only_changes_on_content_change`, `merge_appends_body_once`.

use serde::Serialize;
use serde_yaml::{Mapping, Value};

use crate::entity::{Entity, deep_merge, slugify};
use crate::store::KbStore;

/// Result of a `kb_put_entity` call.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PutOutcome {
    /// Root-relative path of the entity file.
    pub path: String,
    /// The filename slug.
    pub slug: String,
    /// Whether the file was newly created.
    pub created: bool,
    /// Whether the entity file's content actually changed.
    pub changed: bool,
    /// Root-relative paths of target entities the reconciler updated with an
    /// inverse edge (empty when nothing was reconciled).
    pub reconciled: Vec<String>,
}

impl KbStore {
    /// Create or merge an entity, then reconcile its inverse edges.
    ///
    /// Why/What/Test: see the module doc.
    pub fn put_entity(
        &self,
        collection: &str,
        name: &str,
        incoming: Value,
        body: Option<&str>,
        merge: bool,
        now: &str,
    ) -> anyhow::Result<PutOutcome> {
        let slug = slugify(name);
        let path = self.entity_path(collection, name)?;
        let existing = self.read_entity_at(&path)?;

        // ── frontmatter ──────────────────────────────────────────────────────
        let mut fm = if merge {
            existing
                .as_ref()
                .map(|e| e.frontmatter.clone())
                .unwrap_or_else(empty_map)
        } else {
            empty_map()
        };
        deep_merge(&mut fm, normalise_mapping(incoming));
        let map = as_map_mut(&mut fm);
        // OKF: default `type` only (never invent other fields).
        if !map.contains_key("type") {
            let existing_type = existing.as_ref().and_then(|e| e.get_str("type"));
            let ty = existing_type
                .map(str::to_string)
                .unwrap_or_else(|| self.profile.default_type_for(collection));
            map.insert(key("type"), Value::String(ty));
        }
        // `title` defaults to the caller-supplied name (a real value, not a guess).
        if !map.contains_key("title") {
            map.insert(key("title"), Value::String(name.to_string()));
        }

        // ── body ─────────────────────────────────────────────────────────────
        let final_body = self.build_body(existing.as_ref(), body, merge, now);

        // ── created / updated (change-detected) ──────────────────────────────
        let created = existing
            .as_ref()
            .and_then(|e| e.get_str("created").map(str::to_string))
            .or_else(|| map_str(map, "created"))
            .unwrap_or_else(|| now.to_string());
        map.insert(key("created"), Value::String(created));
        map.remove("updated");

        let candidate = Entity {
            frontmatter: fm.clone(),
            body: final_body.clone(),
        };
        let changed = match &existing {
            Some(e) => content_signature(e) != content_signature(&candidate),
            None => true,
        };
        let updated = if changed {
            now.to_string()
        } else {
            existing
                .as_ref()
                .and_then(|e| e.get_str("updated").map(str::to_string))
                .unwrap_or_else(|| now.to_string())
        };
        as_map_mut(&mut fm).insert(key("updated"), Value::String(updated));

        let entity = Entity {
            frontmatter: fm,
            body: final_body,
        };
        let wrote = self.write_entity_at(&path, &entity)?;
        let reconciled = self.reconcile_entity(collection, name, &entity, now)?;

        Ok(PutOutcome {
            path: self.rel(&path),
            slug,
            created: existing.is_none(),
            changed: wrote || changed,
            reconciled,
        })
    }

    /// Build the merged/overwritten body, appending a dated section on merge.
    fn build_body(
        &self,
        existing: Option<&Entity>,
        body: Option<&str>,
        merge: bool,
        now: &str,
    ) -> String {
        let incoming = body.map(str::trim).filter(|b| !b.is_empty());
        if !merge {
            return incoming.unwrap_or("").to_string();
        }
        let mut current = existing.map(|e| e.body.clone()).unwrap_or_default();
        let Some(new_body) = incoming else {
            return current;
        };
        if current.trim().is_empty() {
            return new_body.to_string();
        }
        // Idempotent append: skip if this body text is already present.
        if current.contains(new_body) {
            return current;
        }
        let date = now.split('T').next().unwrap_or(now);
        if !current.ends_with('\n') {
            current.push('\n');
        }
        format!("{current}\n## {date}\n\n{new_body}\n")
    }

    /// Root-relative string form of an absolute in-tree path.
    pub fn rel(&self, path: &std::path::Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string()
    }
}

/// An empty frontmatter mapping value.
pub fn empty_map() -> Value {
    Value::Mapping(Mapping::new())
}

/// Coerce a value into a mapping (a non-mapping incoming payload is ignored).
fn normalise_mapping(v: Value) -> Value {
    match v {
        Value::Mapping(_) => v,
        _ => empty_map(),
    }
}

fn key(k: &str) -> Value {
    Value::String(k.to_string())
}

fn as_map_mut(v: &mut Value) -> &mut Mapping {
    match v {
        Value::Mapping(m) => m,
        _ => unreachable!("frontmatter is always a mapping here"),
    }
}

fn map_str(map: &Mapping, k: &str) -> Option<String> {
    map.get(k).and_then(Value::as_str).map(str::to_string)
}

/// Render an entity's content EXCLUDING the volatile `created`/`updated` stamps,
/// used to detect whether a put is a real content change.
fn content_signature(entity: &Entity) -> String {
    let mut fm = entity.frontmatter.clone();
    if let Value::Mapping(m) = &mut fm {
        m.remove("created");
        m.remove("updated");
    }
    crate::frontmatter::render(&fm, &entity.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Profile;

    fn store() -> (tempfile::TempDir, KbStore) {
        let tmp = tempfile::tempdir().unwrap();
        let s = KbStore::new(tmp.path().to_path_buf(), Profile::default_profile());
        (tmp, s)
    }

    fn fm(pairs: &str) -> Value {
        serde_yaml::from_str(pairs).unwrap()
    }

    /// Why: byte-stability on repeat is the primary determinism guarantee.
    /// What: puts the same entity twice with different wall-clock `now` values;
    /// asserts the file is byte-identical (updated is change-detected, so the
    /// second put reuses the first's timestamp).
    /// Test: self-contained.
    #[test]
    fn put_is_byte_stable_on_repeat() {
        let (_t, s) = store();
        s.put_entity(
            "people",
            "Ada Lovelace",
            fm("aliases: [Ada]\n"),
            Some("Body"),
            false,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        let path = s.entity_path("people", "Ada Lovelace").unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        s.put_entity(
            "people",
            "Ada Lovelace",
            fm("aliases: [Ada]\n"),
            Some("Body"),
            false,
            "2026-09-09T09:09:09Z",
        )
        .unwrap();
        let second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first, second, "repeat put must be byte-identical");
        assert!(first.contains("type: Person"));
        assert!(first.contains("title: Ada Lovelace"));
    }

    /// Why: OKF forbids stripping unknown keys on merge.
    /// What: creates an entity with a custom key, merges an overlay that omits
    /// it; asserts the custom key survives and the overlay key is added.
    /// Test: self-contained.
    #[test]
    fn merge_preserves_existing_keys() {
        let (_t, s) = store();
        s.put_entity(
            "people",
            "Ada",
            fm("custom_key: keepme\naliases: [Ada]\n"),
            None,
            false,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        s.put_entity(
            "people",
            "Ada",
            fm("description: Pioneer\naliases: [Countess]\n"),
            None,
            true,
            "2026-01-02T00:00:00Z",
        )
        .unwrap();
        let e = s.get_entity("people", "Ada").unwrap().unwrap();
        assert_eq!(e.get_str("custom_key"), Some("keepme"));
        assert_eq!(e.get_str("description"), Some("Pioneer"));
        let aliases = e.frontmatter.get("aliases").unwrap().as_sequence().unwrap();
        assert_eq!(aliases.len(), 2);
    }

    /// Why: `updated` must change only on a real content change (OKF), else
    /// idempotent re-runs would churn timestamps.
    /// What: puts, captures updated; re-puts identical content with a later
    /// clock (updated unchanged); then puts new content (updated advances).
    /// Test: self-contained.
    #[test]
    fn updated_only_changes_on_content_change() {
        let (_t, s) = store();
        s.put_entity(
            "people",
            "Ada",
            fm("description: A\n"),
            None,
            true,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        let u1 = s
            .get_entity("people", "Ada")
            .unwrap()
            .unwrap()
            .get_str("updated")
            .unwrap()
            .to_string();
        s.put_entity(
            "people",
            "Ada",
            fm("description: A\n"),
            None,
            true,
            "2026-05-05T00:00:00Z",
        )
        .unwrap();
        let u2 = s
            .get_entity("people", "Ada")
            .unwrap()
            .unwrap()
            .get_str("updated")
            .unwrap()
            .to_string();
        assert_eq!(u1, u2, "no content change -> updated stable");
        s.put_entity(
            "people",
            "Ada",
            fm("description: B\n"),
            None,
            true,
            "2026-05-05T00:00:00Z",
        )
        .unwrap();
        let u3 = s
            .get_entity("people", "Ada")
            .unwrap()
            .unwrap()
            .get_str("updated")
            .unwrap()
            .to_string();
        assert_ne!(u2, u3, "content change -> updated advances");
    }

    /// Why: merge body-append must be idempotent (a re-merge of the same body
    /// must not append a second dated section).
    /// What: merges a body twice; asserts the body text appears exactly once.
    /// Test: self-contained.
    #[test]
    fn merge_appends_body_once() {
        let (_t, s) = store();
        s.put_entity(
            "people",
            "Ada",
            fm("type: Person\n"),
            Some("First note"),
            true,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        s.put_entity(
            "people",
            "Ada",
            empty_map(),
            Some("Second note"),
            true,
            "2026-01-02T00:00:00Z",
        )
        .unwrap();
        s.put_entity(
            "people",
            "Ada",
            empty_map(),
            Some("Second note"),
            true,
            "2026-01-03T00:00:00Z",
        )
        .unwrap();
        let e = s.get_entity("people", "Ada").unwrap().unwrap();
        let count = e.body.matches("Second note").count();
        assert_eq!(count, 1, "duplicate body must not append twice");
        assert!(e.body.contains("First note"));
    }
}
