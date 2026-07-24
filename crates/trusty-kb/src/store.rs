//! [`KbStore`] — the deterministic file-operation engine over one KB root.
//!
//! Why: every tool operates on a single resolved root; centralising the path
//! math, entity read/write, and directory scanning here keeps confinement and
//! determinism in one place, and lets the per-tool modules ([`crate::put`],
//! [`crate::structure`], [`crate::validate`], [`crate::convert`]) add inherent
//! `impl KbStore` blocks without duplicating fs plumbing.
//!
//! What: this file owns the struct + the read side — [`KbStore::status`],
//! [`KbStore::list`], [`KbStore::get_entity`] — plus the shared helpers
//! ([`KbStore::entity_path`], [`KbStore::read_entity_at`],
//! [`KbStore::write_entity_at`], [`KbStore::entity_files`]). The write-side tools
//! live in sibling modules.
//!
//! Test: `status_counts_entities_and_index`, `get_entity_reads_frontmatter`,
//! `list_filters_and_summarises`.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_yaml::Value;

use crate::entity::{Entity, slugify};
use crate::roots::{assert_within, confine};
use crate::schema::{Profile, is_reserved_file};

/// Deterministic KB operations rooted at one tree directory.
#[derive(Debug, Clone)]
pub struct KbStore {
    /// The resolved, confinement-anchor root directory of this tree.
    pub root: PathBuf,
    /// The active schema profile.
    pub profile: Profile,
}

/// Per-collection status row.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CollectionStatus {
    /// Directory name.
    pub name: String,
    /// Whether the profile knows this collection (vs. a free topic dir).
    pub known: bool,
    /// Number of entity files (reserved files excluded).
    pub entity_count: usize,
    /// Whether the generated `index.md` README is present.
    pub has_index: bool,
}

/// Full-tree status overview.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StatusReport {
    /// The resolved root path (as a string for JSON output).
    pub root: String,
    /// Whether the root directory exists on disk yet.
    pub exists: bool,
    /// Whether the root `index.md` topic map is present.
    pub has_root_index: bool,
    /// One row per collection directory found (known + free), name-sorted.
    pub collections: Vec<CollectionStatus>,
    /// Relative paths of entity files that fail frontmatter validation
    /// (parse error or missing the required `type`).
    pub invalid: Vec<String>,
}

/// A one-line entity summary for `kb_list`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EntitySummary {
    /// The collection (directory) the entity lives in.
    pub collection: String,
    /// The filename slug (without `.md`).
    pub slug: String,
    /// The `title` frontmatter field, if any.
    pub title: Option<String>,
    /// The `type` frontmatter field, if any.
    pub type_: Option<String>,
    /// The `description` field, else the first non-blank body line.
    pub summary: Option<String>,
}

impl KbStore {
    /// Build a store over a resolved root with the given profile.
    pub fn new(root: PathBuf, profile: Profile) -> Self {
        Self { root, profile }
    }

    /// The confined, boundary-checked path of an entity file.
    ///
    /// Why: `collection`/`name` are caller input — this is the one place they
    /// become a filesystem path, so confinement + the symlink backstop live
    /// here.
    /// What: slugs the name, joins `<root>/<collection>/<slug>.md` via
    /// [`confine`] (rejecting traversal), then [`assert_within`] verifies it does
    /// not escape via a symlink.
    /// Test: exercised by `get_entity_reads_frontmatter` and the put tests.
    pub fn entity_path(&self, collection: &str, name: &str) -> anyhow::Result<PathBuf> {
        let slug = slugify(name);
        let file = format!("{slug}.md");
        let path = confine(&self.root, &[collection, &file])?;
        assert_within(&self.root, &path)?;
        Ok(path)
    }

    /// The confined path of a collection directory.
    pub fn collection_dir(&self, collection: &str) -> anyhow::Result<PathBuf> {
        let path = confine(&self.root, &[collection])?;
        assert_within(&self.root, &path)?;
        Ok(path)
    }

    /// Read and parse the entity at `path`, or `None` if it does not exist.
    ///
    /// Test: `get_entity_reads_frontmatter`.
    pub fn read_entity_at(&self, path: &Path) -> anyhow::Result<Option<Entity>> {
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(path)?;
        Ok(Some(Entity::from_content(&content)?))
    }

    /// Write `entity` to `path`, creating parent directories, only if the
    /// canonical bytes differ from what is already there.
    ///
    /// Why: determinism + idempotency — a no-op write must not touch the file
    /// (so a second identical pass is provably byte-stable and mtime-stable).
    /// What: renders canonical content, compares against the current file, and
    /// writes only on a real difference. Returns `true` if the file changed.
    /// Test: exercised by the put idempotency tests.
    pub fn write_entity_at(&self, path: &Path, entity: &Entity) -> anyhow::Result<bool> {
        let rendered = entity.to_content();
        if let Ok(existing) = std::fs::read_to_string(path)
            && existing == rendered
        {
            return Ok(false);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, rendered)?;
        Ok(true)
    }

    /// Entity filenames (slug, path) directly under a collection dir, sorted,
    /// with OKF reserved files excluded.
    ///
    /// Test: `status_counts_entities_and_index`.
    pub fn entity_files(&self, collection: &str) -> anyhow::Result<Vec<(String, PathBuf)>> {
        let dir = self.collection_dir(collection)?;
        let mut out = Vec::new();
        if !dir.is_dir() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if !path.is_file() || !name.ends_with(".md") || is_reserved_file(&name) {
                continue;
            }
            let slug = name.trim_end_matches(".md").to_string();
            out.push((slug, path));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// Directory names directly under the root that look like collections
    /// (any subdirectory), sorted.
    pub fn collection_dirs_on_disk(&self) -> anyhow::Result<Vec<String>> {
        let mut out = Vec::new();
        if !self.root.is_dir() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.path().is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.starts_with('.') && name != "_state" {
                    out.push(name);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Full-tree status overview.
    ///
    /// Why: the first tool a client calls to understand a tree.
    /// What: enumerates known + free collection dirs, counts entities, notes
    /// index presence, and lists entity files failing validation.
    /// Test: `status_counts_entities_and_index`.
    pub fn status(&self) -> anyhow::Result<StatusReport> {
        let exists = self.root.is_dir();
        let mut names = self.collection_dirs_on_disk()?;
        // Always report known collections even when their dir is absent.
        for known in self.profile.collection_names() {
            if !names.iter().any(|n| n == known) {
                names.push(known.to_string());
            }
        }
        names.sort();
        names.dedup();

        let mut collections = Vec::new();
        let mut invalid = Vec::new();
        for name in &names {
            let files = self.entity_files(name)?;
            let index = self.collection_dir(name)?.join("index.md");
            for (_slug, path) in &files {
                if self.entity_invalid(path)
                    && let Ok(rel) = path.strip_prefix(&self.root)
                {
                    invalid.push(rel.to_string_lossy().to_string());
                }
            }
            collections.push(CollectionStatus {
                name: name.clone(),
                known: self.profile.is_known_collection(name),
                entity_count: files.len(),
                has_index: index.is_file(),
            });
        }
        invalid.sort();
        Ok(StatusReport {
            root: self.root.to_string_lossy().to_string(),
            exists,
            has_root_index: self.root.join("index.md").is_file(),
            collections,
            invalid,
        })
    }

    /// Whether an entity file fails basic frontmatter validation (unparseable or
    /// missing the required `type`).
    fn entity_invalid(&self, path: &Path) -> bool {
        match self.read_entity_at(path) {
            Ok(Some(e)) => e.get_str("type").is_none(),
            Ok(None) => false,
            Err(_) => true,
        }
    }

    /// Read one entity by collection + name.
    ///
    /// Test: `get_entity_reads_frontmatter`.
    pub fn get_entity(&self, collection: &str, name: &str) -> anyhow::Result<Option<Entity>> {
        let path = self.entity_path(collection, name)?;
        self.read_entity_at(&path)
    }

    /// List entities, optionally scoped to one collection and/or substring
    /// filtered on title/slug.
    ///
    /// Why: the roster view a client uses to browse a tree.
    /// What: iterates collections (all, or the one named), summarising each
    /// entity from its frontmatter/body. `filter` is a case-insensitive
    /// substring matched against title and slug. Output is collection- then
    /// slug-sorted (deterministic).
    /// Test: `list_filters_and_summarises`.
    pub fn list(
        &self,
        collection: Option<&str>,
        filter: Option<&str>,
    ) -> anyhow::Result<Vec<EntitySummary>> {
        let collections: Vec<String> = match collection {
            Some(c) => vec![c.to_string()],
            None => {
                let mut names = self.collection_dirs_on_disk()?;
                names.sort();
                names
            }
        };
        let needle = filter.map(|f| f.to_lowercase());
        let mut out = Vec::new();
        for coll in &collections {
            for (slug, path) in self.entity_files(coll)? {
                // Fail-open: skip a malformed file rather than abort the whole
                // listing (kb_validate surfaces the parse error separately).
                let Ok(Some(entity)) = self.read_entity_at(&path) else {
                    continue;
                };
                let title = entity.get_str("title").map(str::to_string);
                if let Some(n) = &needle {
                    let hay =
                        format!("{} {}", title.clone().unwrap_or_default(), slug).to_lowercase();
                    if !hay.contains(n) {
                        continue;
                    }
                }
                out.push(EntitySummary {
                    collection: coll.clone(),
                    slug,
                    title,
                    type_: entity.get_str("type").map(str::to_string),
                    summary: summarise(&entity),
                });
            }
        }
        Ok(out)
    }
}

/// A one-line summary for an entity: its `description`, else its first non-blank
/// body line, trimmed.
pub fn summarise(entity: &Entity) -> Option<String> {
    if let Some(Value::String(desc)) = entity.frontmatter.get("description") {
        let d = desc.trim();
        if !d.is_empty() {
            return Some(d.to_string());
        }
    }
    entity
        .body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("##"))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, KbStore) {
        let tmp = tempfile::tempdir().unwrap();
        let s = KbStore::new(tmp.path().to_path_buf(), Profile::default_profile());
        (tmp, s)
    }

    /// Why: status is the primary read tool; counts + index detection + invalid
    /// reporting are its whole contract.
    /// What: writes a valid entity, an invalid (no `type`) entity, and an
    /// index.md; asserts counts, `has_index`, and the invalid list.
    /// Test: self-contained.
    #[test]
    fn status_counts_entities_and_index() {
        let (_t, s) = store();
        std::fs::create_dir_all(s.root.join("people")).unwrap();
        std::fs::write(
            s.root.join("people/ada.md"),
            "---\ntype: Person\n---\n\nx\n",
        )
        .unwrap();
        std::fs::write(
            s.root.join("people/bad.md"),
            "---\ntitle: NoType\n---\n\ny\n",
        )
        .unwrap();
        std::fs::write(s.root.join("people/index.md"), "# People\n").unwrap();

        let report = s.status().unwrap();
        let people = report
            .collections
            .iter()
            .find(|c| c.name == "people")
            .unwrap();
        assert_eq!(people.entity_count, 2);
        assert!(people.has_index);
        assert!(report.invalid.iter().any(|p| p.ends_with("bad.md")));
    }

    /// Why: get_entity backs both kb_get_entity and merge reads.
    /// What: writes an entity, reads it back, asserts frontmatter + body.
    /// Test: self-contained.
    #[test]
    fn get_entity_reads_frontmatter() {
        let (_t, s) = store();
        std::fs::create_dir_all(s.root.join("people")).unwrap();
        std::fs::write(
            s.root.join("people/ada-lovelace.md"),
            "---\ntype: Person\ntitle: Ada Lovelace\n---\n\nMathematician.\n",
        )
        .unwrap();
        let e = s.get_entity("people", "Ada Lovelace").unwrap().unwrap();
        assert_eq!(e.get_str("title"), Some("Ada Lovelace"));
        assert_eq!(e.body.trim(), "Mathematician.");
        assert!(s.get_entity("people", "Nobody").unwrap().is_none());
    }

    /// Why: list's filter + summary + ordering are its contract.
    /// What: writes two entities, asserts filtering by substring and the
    /// description-first summary.
    /// Test: self-contained.
    #[test]
    fn list_filters_and_summarises() {
        let (_t, s) = store();
        std::fs::create_dir_all(s.root.join("people")).unwrap();
        std::fs::write(
            s.root.join("people/ada.md"),
            "---\ntype: Person\ntitle: Ada\ndescription: First programmer\n---\n\nbody\n",
        )
        .unwrap();
        std::fs::write(
            s.root.join("people/grace.md"),
            "---\ntype: Person\ntitle: Grace\n---\n\nCompiler pioneer\n",
        )
        .unwrap();

        let all = s.list(Some("people"), None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].slug, "ada");
        assert_eq!(all[0].summary.as_deref(), Some("First programmer"));
        assert_eq!(all[1].summary.as_deref(), Some("Compiler pioneer"));

        let filtered = s.list(Some("people"), Some("grace")).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].slug, "grace");
    }
}
