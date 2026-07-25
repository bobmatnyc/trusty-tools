//! Per-store source registry — the `_sources/registry.toml` a KB tree owns.
//!
//! Why: an OKG store is grown from N heterogeneous sources (a doc directory, a
//! Gmail query window, a Drive folder). Registering a NEW source must be purely
//! ADDITIVE — appending a row that never disturbs another source's entries or
//! ledger — and widening an EXISTING source (e.g. pushing a Gmail window
//! further back in time) must be an in-place locator update that leaves the row
//! order, and therefore the file, otherwise byte-identical. Persisting the
//! registry as human-readable TOML next to the tree keeps it reviewable and
//! safe to commit alongside the markdown.
//!
//! What: [`SourceRegistry`] is the whole file (`sources = [...]`).
//! [`SourceSpec`] is one row; its [`Locator`] is an externally tagged enum, so
//! the TOML sub-table name (`doc_store` / `gmail` / `drive`) IS the source kind
//! and the two can never disagree. [`SourceRegistry::upsert`] implements the
//! append-or-update-in-place contract, and [`SourceRegistry::save`] is
//! write-if-changed so a no-op registration does not touch the file.
//!
//! Test: `upsert_appends_without_disturbing_siblings`,
//! `upsert_updates_locator_in_place`, `roundtrip_through_toml`,
//! `source_id_is_slugged`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::entity::slugify;
use crate::roots::{assert_within, confine};

/// Directory, under a KB root, holding the registry and every source ledger.
pub const SOURCES_DIR: &str = "_sources";

/// Registry filename inside [`SOURCES_DIR`].
pub const REGISTRY_FILE: &str = "registry.toml";

/// Where an ingested item lands when a source does not name a collection.
pub const DEFAULT_COLLECTION: &str = "sources";

/// One source's addressable location, externally tagged by kind.
///
/// Why: the tag doubles as the `kind` discriminator, so a row can never claim
/// `kind = "gmail"` while carrying a filesystem path.
/// What: `doc_store` = a directory of text files; `gmail` = a query plus an
/// optional `after`/`before` window; `drive` = a folder id.
/// Test: `roundtrip_through_toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Locator {
    /// A directory of text files on the local filesystem.
    DocStore {
        /// Absolute or store-relative directory to walk.
        path: String,
        /// Lowercase extensions to accept. Empty = the built-in text set.
        #[serde(default)]
        extensions: Vec<String>,
        /// Walk subdirectories (default true).
        #[serde(default = "default_true")]
        recursive: bool,
    },
    /// A Gmail search query, optionally bounded by a date window.
    Gmail {
        /// Raw Gmail search DSL (e.g. `in:sent`), WITHOUT date operators.
        query: String,
        /// Inclusive lower bound, `YYYY/MM/DD`. Widening this backwards is the
        /// additive time-extension case.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<String>,
        /// Exclusive upper bound, `YYYY/MM/DD`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<String>,
    },
    /// A Google Drive folder.
    Drive {
        /// Drive folder id (`root` for My Drive).
        folder_id: String,
        /// Descend into subfolders (default false).
        #[serde(default)]
        recursive: bool,
    },
}

impl Locator {
    /// The source kind as a stable lowercase string, for reports and tool JSON.
    pub fn kind(&self) -> &'static str {
        match self {
            Locator::DocStore { .. } => "docstore",
            Locator::Gmail { .. } => "gmail",
            Locator::Drive { .. } => "drive",
        }
    }
}

fn default_true() -> bool {
    true
}

/// One registered source: identity, destination collection, and locator.
///
/// Field order matters — `serde`'s TOML serializer emits fields in declaration
/// order and TOML requires scalars before sub-tables, so `locator` is last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpec {
    /// Stable slug identifying this source within the store. Names its ledger.
    pub id: String,
    /// KB collection ingested entities are written into.
    pub collection: String,
    /// Whether ingestion runs for this source.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Mark entities whose source item vanished instead of leaving them stale.
    /// Only ever consulted for sources that enumerate their full corpus
    /// (doc stores); a windowed Gmail pull can never imply deletion.
    #[serde(default)]
    pub tombstone_deleted: bool,
    /// ISO-8601 timestamp of first registration. Never rewritten on update.
    pub added_at: String,
    /// Where this source's items come from.
    pub locator: Locator,
}

impl SourceSpec {
    /// Build a spec, slugging `id` so it can only ever be one path segment.
    ///
    /// Why: `id` names a ledger file; an unslugged `../../etc` would be a
    /// traversal vector, and a slug also keeps the TOML stable.
    /// What: slugs the id, defaults an empty collection to [`DEFAULT_COLLECTION`].
    /// Test: `source_id_is_slugged`.
    pub fn new(id: &str, collection: Option<&str>, locator: Locator, added_at: &str) -> Self {
        let collection = collection
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .unwrap_or(DEFAULT_COLLECTION)
            .to_string();
        Self {
            id: slugify(id),
            collection,
            enabled: true,
            tombstone_deleted: false,
            added_at: added_at.to_string(),
            locator,
        }
    }

    /// The source kind string (delegates to the locator tag).
    pub fn kind(&self) -> &'static str {
        self.locator.kind()
    }
}

/// Outcome of registering (or re-registering) one source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegisterOutcome {
    /// The canonical (slugged) source id.
    pub id: String,
    /// True when this id had no prior row — a pure append.
    pub created: bool,
    /// True when the stored row differs from what was there before.
    pub changed: bool,
    /// Root-relative registry path.
    pub path: String,
}

/// The whole `_sources/registry.toml` document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRegistry {
    /// Registered sources, in registration order. Order is never rewritten.
    #[serde(default)]
    pub sources: Vec<SourceSpec>,
}

impl SourceRegistry {
    /// The confined registry path under a KB root.
    pub fn path(root: &Path) -> anyhow::Result<PathBuf> {
        let path = confine(root, &[SOURCES_DIR, REGISTRY_FILE])?;
        assert_within(root, &path)?;
        Ok(path)
    }

    /// Load the registry, treating an absent file as an empty registry.
    ///
    /// Why: a store with no sources yet is the normal starting state, not an
    /// error — `okg_sources` on a fresh tree must return `[]`.
    /// What: reads and parses the TOML; a missing file yields the default.
    /// Test: `roundtrip_through_toml`.
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let path = Self::path(root)?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)?;
        let parsed: Self = toml::from_str(&text).map_err(|e| {
            anyhow::anyhow!("{} is not a valid source registry: {e}", path.display())
        })?;
        Ok(parsed)
    }

    /// Render and write the registry, only when the bytes actually change.
    ///
    /// Why: a re-registration that changes nothing must leave the file (and its
    /// mtime) untouched, so "additive" is provable by inspection.
    /// What: serialises to TOML with a generated-file header, compares, writes.
    /// Returns whether the file changed.
    /// Test: `upsert_appends_without_disturbing_siblings`.
    pub fn save(&self, root: &Path) -> anyhow::Result<bool> {
        let path = Self::path(root)?;
        let body = toml::to_string_pretty(self)?;
        let rendered = format!("{HEADER}{body}");
        if let Ok(existing) = std::fs::read_to_string(&path)
            && existing == rendered
        {
            return Ok(false);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, rendered)?;
        Ok(true)
    }

    /// Look up a source by its canonical (slugged) id.
    pub fn get(&self, id: &str) -> Option<&SourceSpec> {
        let slug = slugify(id);
        self.sources.iter().find(|s| s.id == slug)
    }

    /// Append a new source, or update an existing one in place.
    ///
    /// Why: this is the additivity contract. A brand-new id appends to the end,
    /// leaving every sibling row byte-identical; a known id has ONLY its
    /// mutable fields (collection, locator, flags) replaced, keeping its
    /// position and its original `added_at` so its ledger stays meaningful.
    /// Widening a Gmail window backwards takes this path.
    /// What: mutates `self` and reports whether the row was created/changed.
    /// Test: `upsert_appends_without_disturbing_siblings`,
    /// `upsert_updates_locator_in_place`.
    pub fn upsert(&mut self, incoming: SourceSpec) -> (bool, bool) {
        match self.sources.iter_mut().find(|s| s.id == incoming.id) {
            Some(existing) => {
                let before = existing.clone();
                existing.collection = incoming.collection;
                existing.enabled = incoming.enabled;
                existing.tombstone_deleted = incoming.tombstone_deleted;
                existing.locator = incoming.locator;
                (false, *existing != before)
            }
            None => {
                self.sources.push(incoming);
                (true, true)
            }
        }
    }
}

/// Header comment written above the generated TOML body.
const HEADER: &str = "# OKG source registry — maintained by the okg_ingest_* tools.\n\
                      # Hand edits are preserved; re-registering a source updates its row in place.\n\n";

#[cfg(test)]
mod tests {
    use super::*;

    fn gmail(query: &str, after: Option<&str>) -> Locator {
        Locator::Gmail {
            query: query.to_string(),
            after: after.map(str::to_string),
            before: None,
        }
    }

    /// Why: `id` names a ledger file, so an unslugged traversal string must not
    /// survive registration.
    /// What: asserts a hostile id is reduced to a single safe segment and an
    /// empty collection falls back to the default.
    /// Test: self-contained.
    #[test]
    fn source_id_is_slugged() {
        let spec = SourceSpec::new(
            "../../Etc Passwd",
            None,
            gmail("in:sent", None),
            "2026-01-01",
        );
        assert_eq!(spec.id, "etc-passwd");
        assert_eq!(spec.collection, DEFAULT_COLLECTION);
        assert_eq!(spec.kind(), "gmail");
    }

    /// Why: registering a NEW source must never rebuild or disturb the rows
    /// already present — that is the whole additivity promise.
    /// What: registers two sources, snapshots the file, registers a third, and
    /// asserts the first two rows are unchanged and still first.
    /// Test: self-contained.
    #[test]
    fn upsert_appends_without_disturbing_siblings() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let mut reg = SourceRegistry::default();
        reg.upsert(SourceSpec::new(
            "alpha",
            Some("mail"),
            gmail("in:sent", None),
            "t0",
        ));
        reg.upsert(SourceSpec::new(
            "beta",
            None,
            Locator::Drive {
                folder_id: "F1".into(),
                recursive: false,
            },
            "t0",
        ));
        assert!(reg.save(root).unwrap());
        let before = std::fs::read_to_string(SourceRegistry::path(root).unwrap()).unwrap();
        let first_two = reg.sources.clone();

        let (created, changed) = reg.upsert(SourceSpec::new(
            "gamma",
            None,
            Locator::DocStore {
                path: "/docs".into(),
                extensions: vec![],
                recursive: true,
            },
            "t1",
        ));
        assert!(created && changed);
        assert_eq!(reg.sources[..2], first_two[..], "existing rows untouched");
        assert_eq!(reg.sources.len(), 3);
        assert!(reg.save(root).unwrap());

        let after = std::fs::read_to_string(SourceRegistry::path(root).unwrap()).unwrap();
        assert!(
            after.starts_with(before.trim_end()),
            "append must extend the file, not rewrite it:\nbefore={before}\nafter={after}"
        );

        // Saving again with no change must not rewrite the file.
        assert!(!reg.save(root).unwrap(), "no-op save must be a no-op");
    }

    /// Why: widening a Gmail window backwards re-registers the SAME id; it must
    /// update that row in place, keep its position and `added_at`, and not
    /// duplicate the source.
    /// What: registers `mail`, then re-registers it with an earlier `after`.
    /// Test: self-contained.
    #[test]
    fn upsert_updates_locator_in_place() {
        let mut reg = SourceRegistry::default();
        reg.upsert(SourceSpec::new(
            "mail",
            None,
            gmail("in:sent", Some("2026/06/01")),
            "t0",
        ));
        reg.upsert(SourceSpec::new(
            "other",
            None,
            gmail("in:inbox", None),
            "t0",
        ));

        let (created, changed) = reg.upsert(SourceSpec::new(
            "mail",
            None,
            gmail("in:sent", Some("2024/01/01")),
            "t9",
        ));
        assert!(!created, "same id must not append a second row");
        assert!(changed, "a widened window is a real change");
        assert_eq!(reg.sources.len(), 2);
        assert_eq!(reg.sources[0].id, "mail", "position preserved");
        assert_eq!(reg.sources[0].added_at, "t0", "added_at never rewritten");
        assert_eq!(
            reg.sources[0].locator,
            gmail("in:sent", Some("2024/01/01")),
            "locator widened"
        );

        // Re-registering the identical spec is a no-op.
        let (created, changed) = reg.upsert(SourceSpec::new(
            "mail",
            None,
            gmail("in:sent", Some("2024/01/01")),
            "t9",
        ));
        assert!(!created && !changed);
    }

    /// Why: the registry is the durable contract between runs; it must survive a
    /// full write/read cycle with every locator variant intact.
    /// What: saves all three kinds and reloads, asserting equality.
    /// Test: self-contained.
    #[test]
    fn roundtrip_through_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        assert_eq!(
            SourceRegistry::load(root).unwrap(),
            SourceRegistry::default(),
            "absent registry loads empty"
        );

        let mut reg = SourceRegistry::default();
        reg.upsert(SourceSpec::new(
            "docs",
            Some("notes"),
            Locator::DocStore {
                path: "/tmp/docs".into(),
                extensions: vec!["md".into()],
                recursive: true,
            },
            "t0",
        ));
        reg.upsert(SourceSpec::new(
            "mail",
            None,
            gmail("in:sent", Some("2026/01/01")),
            "t0",
        ));
        reg.upsert(SourceSpec::new(
            "drive",
            None,
            Locator::Drive {
                folder_id: "abc".into(),
                recursive: true,
            },
            "t0",
        ));
        reg.save(root).unwrap();

        let loaded = SourceRegistry::load(root).unwrap();
        assert_eq!(loaded, reg);
        assert_eq!(loaded.get("Docs").map(|s| s.kind()), Some("docstore"));
    }
}
