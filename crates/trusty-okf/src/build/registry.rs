//! Per-bundle source registry — the `_sources/registry.toml` a bundle owns.
//!
//! Why: a bundle is grown from N heterogeneous sources. Registering a NEW
//! source must be purely ADDITIVE — appending a row that never disturbs
//! another source's entries or ledger — and widening an EXISTING source must
//! be an in-place locator update that leaves the row order, and therefore the
//! file, otherwise byte-identical. Persisting the registry as human-readable
//! TOML next to the tree keeps it reviewable and safe to commit.
//!
//! What: [`SourceRegistry`] is the whole file (`sources = [...]`).
//! [`SourceSpec`] is one row; its [`Locator`] is written as a one-entry table
//! whose name IS the source kind (`[sources.locator.doc_store]`,
//! `[sources.locator.<kind>]`). #10746: trusty-okf knows one locator, the
//! local doc store; every other kind is the open variant
//! [`Locator::Connector`] (DOC-55 §5.3), which keeps the table name and its
//! keys in file order, so a row written by trusty-kb for a connector kind
//! loads and saves byte for byte (SPEC-OKF-01 §2.7.2, §2.9 item 10, B6).
//! [`SourceRegistry::upsert`] implements the append-or-update-in-place
//! contract, and [`SourceRegistry::save`] is write-if-changed.
//!
//! Test: `upsert_appends_without_disturbing_siblings`,
//! `upsert_updates_locator_in_place`, `roundtrip_through_toml`,
//! `source_id_is_slugged`, `golden_registry_loads_every_row`,
//! `golden_registry_saves_byte_for_byte`.
//!
//! # Spec References
//! - SPEC-OKF-01 §2.7.2 (registry row), §2.9 item 10, §6.2 criterion 13.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::OkfError;
use crate::bundle::paths::{atomic_write, confine, confined_file, io_err};
use crate::format::concept::slugify;

/// Directory, under a bundle root, holding the registry and every ledger.
pub const SOURCES_DIR: &str = "_sources";

/// Registry filename inside [`SOURCES_DIR`].
pub const REGISTRY_FILE: &str = "registry.toml";

/// Where an ingested item lands when a source does not name a collection.
pub const DEFAULT_COLLECTION: &str = "sources";

/// The table name of the one locator trusty-okf interprets.
pub const DOC_STORE_TAG: &str = "doc_store";

/// A connector locator's keys and values, in file order.
///
/// Why: a connector kind belongs to its adapter, not to trusty-okf (B6), so
/// trusty-okf cannot type its keys. Keeping them as an ordered list means a
/// load and save of an unchanged row writes the same bytes.
/// What: an ordered `(key, value)` list; serialises as a TOML table.
/// Test: `golden_registry_saves_byte_for_byte`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConnectorParams(Vec<(String, toml::Value)>);

impl ConnectorParams {
    /// An empty parameter table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `key`, replacing its value in place or appending it.
    pub fn insert(&mut self, key: &str, value: toml::Value) {
        match self.0.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) => *v = value,
            None => self.0.push((key.to_string(), value)),
        }
    }

    /// The value of `key`, if present.
    pub fn get(&self, key: &str) -> Option<&toml::Value> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// The keys and values in file order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &toml::Value)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Whether there are no keys.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for ConnectorParams {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ConnectorParams {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct ParamsVisitor;
        impl<'de> Visitor<'de> for ParamsVisitor {
            type Value = ConnectorParams;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a table of connector parameters")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Self::Value, A::Error> {
                let mut out = ConnectorParams::new();
                while let Some((k, v)) = m.next_entry::<String, toml::Value>()? {
                    out.0.push((k, v));
                }
                Ok(out)
            }
        }
        d.deserialize_map(ParamsVisitor)
    }
}

/// One source's addressable location, tagged by kind.
///
/// Why: the table name doubles as the `kind` discriminator, so a row can
/// never claim one kind while carrying another kind's keys.
/// What: [`Locator::DocStore`] is a directory of text files; every other
/// table name is [`Locator::Connector`], whose adapter lives outside
/// trusty-okf (#10746, SPEC-OKF-01 §2.7.2, DOC-55 §5.3).
/// Test: `roundtrip_through_toml`, `golden_registry_loads_every_row`.
#[derive(Debug, Clone, PartialEq)]
pub enum Locator {
    /// A directory of text files on the local filesystem.
    DocStore {
        /// Absolute or store-relative directory to walk.
        path: String,
        /// Lowercase extensions to accept. Empty = the built-in text set.
        extensions: Vec<String>,
        /// Walk subdirectories (default true).
        recursive: bool,
    },
    /// A connector-defined source; trusty-okf keeps but never interprets it.
    Connector {
        /// The TOML table name, for example `gmail` or `drive`.
        kind: String,
        /// The table's keys and values, in file order.
        params: ConnectorParams,
    },
}

impl Locator {
    /// The source kind as a stable lowercase string, for reports and JSON.
    ///
    /// What: `docstore` for a doc store (trusty-kb's spelling, kept), else
    /// the connector's table name.
    pub fn kind(&self) -> &str {
        match self {
            Locator::DocStore { .. } => "docstore",
            Locator::Connector { kind, .. } => kind,
        }
    }
}

fn default_true() -> bool {
    true
}

/// The doc-store table, field order as trusty-kb wrote it.
#[derive(Serialize, Deserialize)]
struct DocStoreTable {
    path: String,
    #[serde(default)]
    extensions: Vec<String>,
    #[serde(default = "default_true")]
    recursive: bool,
}

impl Serialize for Locator {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(1))?;
        match self {
            Locator::DocStore {
                path,
                extensions,
                recursive,
            } => {
                let table = DocStoreTable {
                    path: path.clone(),
                    extensions: extensions.clone(),
                    recursive: *recursive,
                };
                map.serialize_entry(DOC_STORE_TAG, &table)?;
            }
            Locator::Connector { kind, params } => {
                // #10746: a connector named like the doc store would reload as
                // a doc store and lose its keys; refuse to write it.
                if kind == DOC_STORE_TAG || kind.is_empty() {
                    return Err(serde::ser::Error::custom(format!(
                        "connector kind {kind:?} is reserved or empty"
                    )));
                }
                map.serialize_entry(kind, params)?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Locator {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct LocatorVisitor;
        impl<'de> Visitor<'de> for LocatorVisitor {
            type Value = Locator;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a table with exactly one locator kind")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Self::Value, A::Error> {
                let Some(tag) = m.next_key::<String>()? else {
                    return Err(de::Error::custom("locator names no kind"));
                };
                let locator = if tag == DOC_STORE_TAG {
                    let t: DocStoreTable = m.next_value()?;
                    Locator::DocStore {
                        path: t.path,
                        extensions: t.extensions,
                        recursive: t.recursive,
                    }
                } else {
                    Locator::Connector {
                        kind: tag,
                        params: m.next_value()?,
                    }
                };
                if let Some(extra) = m.next_key::<String>()? {
                    return Err(de::Error::custom(format!(
                        "locator names a second kind {extra:?}"
                    )));
                }
                Ok(locator)
            }
        }
        d.deserialize_map(LocatorVisitor)
    }
}

/// One registered source: identity, destination collection, and locator.
///
/// Field order matters — `serde`'s TOML serializer emits fields in declaration
/// order and TOML requires scalars before sub-tables, so `locator` is last.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceSpec {
    /// Stable slug identifying this source within the bundle. Names its ledger.
    pub id: String,
    /// Collection ingested concepts are written into.
    pub collection: String,
    /// Whether ingestion runs for this source.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Mark concepts whose source item vanished instead of leaving them stale.
    /// Only ever consulted for sources that enumerate their full corpus
    /// (doc stores); a windowed connector pull can never imply deletion.
    #[serde(default)]
    pub tombstone_deleted: bool,
    /// Operator designation that this source's corpus was authored by the
    /// user, making it the single carve-out from the untrusted default
    /// (#4532, DOC-63 §6.3 `S-4.3`).
    ///
    /// Consulted ONLY for a [`Locator::DocStore`]; on a connector it is
    /// ignored rather than honoured, because no remote corpus has an
    /// enforceable author constraint (DOC-63 §6.4 `S-4.8`/`S-4.9`). Defaults
    /// to `false`, so every registry row written before this field existed
    /// loads as untrusted — the fail-closed direction.
    /// See [`super::trust::TrustLabel::for_source`].
    #[serde(default)]
    pub user_authored: bool,
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
            user_authored: false,
            added_at: added_at.to_string(),
            locator,
        }
    }

    /// Designate this source's corpus user-authored (#4532).
    ///
    /// Why: the designation is an OPERATOR act, so it needs an explicit,
    /// greppable call site rather than a struct-literal field that a future
    /// connector could set while assembling a spec from fetched data.
    /// What: sets [`Self::user_authored`]. Whether it changes anything is
    /// [`super::trust::TrustLabel::for_source`]'s decision, not this
    /// setter's — a connector keeps the untrusted label regardless.
    /// Test: `only_a_designated_directory_is_user_authored`,
    /// `remote_kinds_are_always_untrusted`.
    pub fn with_user_authored(mut self, user_authored: bool) -> Self {
        self.user_authored = user_authored;
        self
    }

    /// The source kind string (delegates to the locator tag).
    pub fn kind(&self) -> &str {
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceRegistry {
    /// Registered sources, in registration order. Order is never rewritten.
    #[serde(default)]
    pub sources: Vec<SourceSpec>,
}

/// The path of `_sources/<file>` under `root`.
///
/// #10746: lexical confinement always; when the root exists, the §2.11 link
/// check too. A missing root is a fresh tree, which reads as empty.
pub(crate) fn sources_file(root: &Path, file: &str) -> Result<PathBuf, OkfError> {
    if root.exists() {
        confined_file(root, &format!("{SOURCES_DIR}/{file}"))
    } else {
        confine(root, &[SOURCES_DIR, file])
    }
}

impl SourceRegistry {
    /// The confined registry path under a bundle root.
    pub fn path(root: &Path) -> Result<PathBuf, OkfError> {
        sources_file(root, REGISTRY_FILE)
    }

    /// Load the registry, treating an absent file as an empty registry.
    ///
    /// Why: a bundle with no sources yet is the normal starting state, not an
    /// error — a status view on a fresh tree must return `[]`.
    /// What: reads and parses the TOML; a missing file yields the default; an
    /// unparseable file is [`OkfError::Parse`] and nothing is dropped.
    /// Test: `roundtrip_through_toml`, `golden_registry_loads_every_row`.
    pub fn load(root: &Path) -> Result<Self, OkfError> {
        let path = Self::path(root)?;
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(io_err(&path, e)),
        };
        toml::from_str(&text).map_err(|e| OkfError::Parse {
            path: path.clone(),
            line: None,
            message: format!("not a valid source registry: {e}"),
        })
    }

    /// Render and write the registry, only when the bytes actually change.
    ///
    /// Why: a re-registration that changes nothing must leave the file (and its
    /// mtime) untouched, so "additive" is provable by inspection.
    /// What: serialises to TOML with the trusty-kb header, compares, and
    /// replaces the file atomically (§2.11). Returns whether it changed.
    /// Test: `upsert_appends_without_disturbing_siblings`,
    /// `golden_registry_saves_byte_for_byte`.
    pub fn save(&self, root: &Path) -> Result<bool, OkfError> {
        let path = Self::path(root)?;
        let body = toml::to_string_pretty(self).map_err(|e| OkfError::Parse {
            path: path.clone(),
            line: None,
            message: format!("cannot render the source registry: {e}"),
        })?;
        let rendered = format!("{HEADER}{body}");
        if let Ok(existing) = std::fs::read_to_string(&path)
            && existing == rendered
        {
            return Ok(false);
        }
        atomic_write(&path, rendered.as_bytes())?;
        Ok(true)
    }

    /// Load, upsert and save under the bundle lock (SPEC-OKF-01 §4.4).
    ///
    /// Why: #10746 S5 — two registrations that each load, upsert and save
    /// drop one another's row. What: creates `_sources/` as `save` would (a
    /// first registration creates the tree), takes the bundle lock, then
    /// [`load`](Self::load), [`upsert`](Self::upsert) and
    /// [`save`](Self::save). Returns `(created, changed, wrote)`.
    /// Test: `lock_prevents_lost_log_and_registry_updates`.
    pub fn upsert_at(root: &Path, incoming: SourceSpec) -> Result<(bool, bool, bool), OkfError> {
        let path = Self::path(root)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
        }
        let canon = crate::bundle::paths::canonical_root(root)?;
        let _lock = crate::bundle::lock::BundleLock::acquire(&canon)?;
        let mut reg = Self::load(root)?;
        let (created, changed) = reg.upsert(incoming);
        let wrote = reg.save(root)?;
        Ok((created, changed, wrote))
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

/// Header comment written above the generated TOML body. #10746: unchanged
/// from trusty-kb, so an existing registry saves byte for byte.
const HEADER: &str = "# OKG source registry — maintained by the okg_ingest_* tools.\n\
                      # Hand edits are preserved; re-registering a source updates its row in place.\n\n";

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
