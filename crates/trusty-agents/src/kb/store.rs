//! [`KbStore`] — one KB tree root plus its profile, for the agent's OKG tools.
//!
//! Why: every OKG tool operates on a single resolved root; centralising the
//! confined path math, entity read/write and directory scanning here keeps
//! confinement in one place, and lets [`crate::kb::okg`] add inherent
//! `impl KbStore` blocks without duplicating fs plumbing. #10746 S11 moved it
//! here from the deleted trusty-kb; trusty-okf's `Bundle` is the generic
//! bundle type (SPEC-OKF-01 §2.7.2), and this struct keeps the agent's entry
//! points working on the same files.
//!
//! What: the struct plus the shared helpers — [`KbStore::entity_path`],
//! [`KbStore::read_entity_at`], [`KbStore::write_entity_at`],
//! [`KbStore::entity_files`], [`KbStore::get_entity`] and [`KbStore::rel`].
//!
//! Test: `get_entity_reads_frontmatter`.

use std::path::{Path, PathBuf};

use trusty_okf::format::concept::{Entity, slugify};
use trusty_okf::format::container::is_reserved_file;
use trusty_okf::profile::Profile;

use crate::kb::roots::{assert_within, confine};

/// Deterministic KB operations rooted at one tree directory.
#[derive(Debug, Clone)]
pub struct KbStore {
    /// The resolved, confinement-anchor root directory of this tree.
    pub root: PathBuf,
    /// The active schema profile.
    pub profile: Profile,
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
    /// Test: `reconcile_materialises_pair` (link resolution walks it).
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
    ///
    /// Underscore-prefixed directories are store MACHINERY, not collections:
    /// `_state` holds the conversion log and `_sources` holds the OKG source
    /// registry + per-source ledgers. Excluding the whole `_` prefix (rather
    /// than naming each one) keeps link resolution from ever treating a future
    /// bookkeeping dir as a free topic collection.
    pub fn collection_dirs_on_disk(&self) -> anyhow::Result<Vec<String>> {
        let mut out = Vec::new();
        if !self.root.is_dir() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.path().is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.starts_with('.') && !name.starts_with('_') {
                    out.push(name);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Read one entity by collection + name.
    ///
    /// Test: `get_entity_reads_frontmatter`.
    pub fn get_entity(&self, collection: &str, name: &str) -> anyhow::Result<Option<Entity>> {
        let path = self.entity_path(collection, name)?;
        self.read_entity_at(&path)
    }

    /// Root-relative string form of an absolute in-tree path.
    pub fn rel(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, KbStore) {
        let tmp = tempfile::tempdir().unwrap();
        let s = KbStore::new(tmp.path().to_path_buf(), Profile::default_profile());
        (tmp, s)
    }

    /// Why: get_entity is the confined read the reconcile tests rely on.
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
}
