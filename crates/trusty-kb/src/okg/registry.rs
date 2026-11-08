//! The registry view trusty-agents still uses, over trusty-okf's registry.
//!
//! Why: #10746 S4 moved `_sources/registry.toml` into
//! `trusty_okf::build::registry`, where boundary rule B6 bars connector
//! variants: every kind but the doc store is `Locator::Connector`. The
//! trusty-agents connector tools still construct and match the typed mail
//! and drive locators until slice S11 moves those kinds into trusty-agents
//! (SPEC-OKF-01 §2.7.2). This module keeps that typed view in trusty-kb, which
//! retires in S11, and stores every row through trusty-okf.
//! What: [`Locator`] adds the typed `Gmail` and `Drive` variants to the
//! doc store and the open `Connector`; [`SourceSpec`] is the row;
//! [`SourceRegistry`] wraps trusty-okf's registry, so a row this view never
//! touches is saved exactly as trusty-okf read it. A connector table maps to
//! a typed variant when its keys fit that variant (trusty-kb's own rule: any
//! key order, missing optional keys default); any other table stays
//! `Connector`, so no row is lost.
//! Test: `typed_rows_round_trip_through_trusty_okf`,
//! `untyped_connector_rows_are_kept`, plus `okg_artifacts.rs` and the
//! trusty-okf golden registry tests.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use trusty_okf::build::registry as okf;
pub use trusty_okf::build::registry::{
    ConnectorParams, DEFAULT_COLLECTION, REGISTRY_FILE, RegisterOutcome, SOURCES_DIR,
};

/// One source's location: the doc store, the two typed connector kinds
/// trusty-agents implements, or any other connector table.
#[derive(Debug, Clone, PartialEq)]
pub enum Locator {
    /// A directory of text files on the local filesystem.
    DocStore {
        /// Absolute or store-relative directory to walk.
        path: String,
        /// Lowercase extensions to accept. Empty = the built-in text set.
        extensions: Vec<String>,
        /// Walk subdirectories.
        recursive: bool,
    },
    /// A Gmail search query, optionally bounded by a date window.
    Gmail {
        /// Raw Gmail search DSL (e.g. `in:sent`), WITHOUT date operators.
        query: String,
        /// Inclusive lower bound, `YYYY/MM/DD`.
        after: Option<String>,
        /// Exclusive upper bound, `YYYY/MM/DD`.
        before: Option<String>,
    },
    /// A Google Drive folder.
    Drive {
        /// Drive folder id (`root` for My Drive).
        folder_id: String,
        /// Descend into subfolders.
        recursive: bool,
    },
    /// Any other connector table, kept as trusty-okf read it.
    Connector {
        /// The table name.
        kind: String,
        /// The table's keys and values, in file order.
        params: ConnectorParams,
    },
}

fn text(v: &str) -> toml::Value {
    toml::Value::String(v.to_string())
}

/// The table's string value at `key`, `Ok(None)` when absent, `Err` when not
/// a string.
fn opt_str(p: &ConnectorParams, key: &str) -> Result<Option<String>, ()> {
    match p.get(key) {
        None => Ok(None),
        Some(toml::Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(()),
    }
}

fn only_keys(p: &ConnectorParams, allowed: &[&str]) -> bool {
    p.iter().all(|(k, _)| allowed.contains(&k))
}

impl Locator {
    /// The source kind as a stable lowercase string.
    pub fn kind(&self) -> &str {
        match self {
            Locator::DocStore { .. } => "docstore",
            Locator::Gmail { .. } => "gmail",
            Locator::Drive { .. } => "drive",
            Locator::Connector { kind, .. } => kind,
        }
    }

    /// The trusty-okf locator this row is stored as. Optional values are
    /// written in trusty-kb's field order and omitted when `None`.
    pub fn to_okf(&self) -> okf::Locator {
        match self {
            Locator::DocStore {
                path,
                extensions,
                recursive,
            } => okf::Locator::DocStore {
                path: path.clone(),
                extensions: extensions.clone(),
                recursive: *recursive,
            },
            Locator::Gmail {
                query,
                after,
                before,
            } => {
                let mut p = ConnectorParams::new();
                p.insert("query", text(query));
                if let Some(a) = after {
                    p.insert("after", text(a));
                }
                if let Some(b) = before {
                    p.insert("before", text(b));
                }
                okf::Locator::Connector {
                    kind: "gmail".into(),
                    params: p,
                }
            }
            Locator::Drive {
                folder_id,
                recursive,
            } => {
                let mut p = ConnectorParams::new();
                p.insert("folder_id", text(folder_id));
                p.insert("recursive", toml::Value::Boolean(*recursive));
                okf::Locator::Connector {
                    kind: "drive".into(),
                    params: p,
                }
            }
            Locator::Connector { kind, params } => okf::Locator::Connector {
                kind: kind.clone(),
                params: params.clone(),
            },
        }
    }

    /// The typed view of a trusty-okf locator; a table that does not fit a
    /// typed variant stays `Connector`.
    pub fn from_okf(l: okf::Locator) -> Self {
        match l {
            okf::Locator::DocStore {
                path,
                extensions,
                recursive,
            } => Locator::DocStore {
                path,
                extensions,
                recursive,
            },
            okf::Locator::Connector { kind, params } => match typed(&kind, &params) {
                Some(t) => t,
                None => Locator::Connector { kind, params },
            },
        }
    }
}

/// trusty-kb's serde rule for the two typed kinds: required keys present and
/// strings, optional keys default, no other key.
fn typed(kind: &str, p: &ConnectorParams) -> Option<Locator> {
    match kind {
        "gmail" if only_keys(p, &["query", "after", "before"]) => Some(Locator::Gmail {
            query: opt_str(p, "query").ok()??,
            after: opt_str(p, "after").ok()?,
            before: opt_str(p, "before").ok()?,
        }),
        "drive" if only_keys(p, &["folder_id", "recursive"]) => Some(Locator::Drive {
            folder_id: opt_str(p, "folder_id").ok()??,
            recursive: match p.get("recursive") {
                None => false,
                Some(toml::Value::Boolean(b)) => *b,
                Some(_) => return None,
            },
        }),
        _ => None,
    }
}

impl Serialize for Locator {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_okf().serialize(s)
    }
}

impl<'de> Deserialize<'de> for Locator {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        okf::Locator::deserialize(d).map(Locator::from_okf)
    }
}

/// One registered source, with the typed locator view.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceSpec {
    /// Stable slug identifying this source within the store.
    pub id: String,
    /// Collection ingested entities are written into.
    pub collection: String,
    /// Whether ingestion runs for this source.
    pub enabled: bool,
    /// Mark entities whose source item vanished instead of leaving them stale.
    pub tombstone_deleted: bool,
    /// Operator designation that the corpus is user-authored (#4532).
    pub user_authored: bool,
    /// ISO-8601 timestamp of first registration.
    pub added_at: String,
    /// Where this source's items come from.
    pub locator: Locator,
}

impl SourceSpec {
    /// Build a spec; delegates the id slug and collection default to
    /// trusty-okf.
    pub fn new(id: &str, collection: Option<&str>, locator: Locator, added_at: &str) -> Self {
        let base = okf::SourceSpec::new(id, collection, locator.to_okf(), added_at);
        Self {
            locator,
            ..Self::from_okf(base)
        }
    }

    /// Designate this source's corpus user-authored (#4532).
    pub fn with_user_authored(mut self, user_authored: bool) -> Self {
        self.user_authored = user_authored;
        self
    }

    /// The source kind string.
    pub fn kind(&self) -> &str {
        self.locator.kind()
    }

    /// The trusty-okf row.
    pub fn to_okf(&self) -> okf::SourceSpec {
        okf::SourceSpec {
            id: self.id.clone(),
            collection: self.collection.clone(),
            enabled: self.enabled,
            tombstone_deleted: self.tombstone_deleted,
            user_authored: self.user_authored,
            added_at: self.added_at.clone(),
            locator: self.locator.to_okf(),
        }
    }

    /// The typed view of a trusty-okf row.
    pub fn from_okf(s: okf::SourceSpec) -> Self {
        Self {
            id: s.id,
            collection: s.collection,
            enabled: s.enabled,
            tombstone_deleted: s.tombstone_deleted,
            user_authored: s.user_authored,
            added_at: s.added_at,
            locator: Locator::from_okf(s.locator),
        }
    }
}

impl Serialize for SourceSpec {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_okf().serialize(s)
    }
}

impl<'de> Deserialize<'de> for SourceSpec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        okf::SourceSpec::deserialize(d).map(SourceSpec::from_okf)
    }
}

/// The registry, stored by trusty-okf.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceRegistry {
    inner: okf::SourceRegistry,
}

impl SourceRegistry {
    /// The confined registry path under a KB root.
    pub fn path(root: &Path) -> anyhow::Result<PathBuf> {
        Ok(okf::SourceRegistry::path(root)?)
    }

    /// Load the registry; an absent file is empty.
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            inner: okf::SourceRegistry::load(root)?,
        })
    }

    /// Write the registry when its bytes change; returns whether they did.
    pub fn save(&self, root: &Path) -> anyhow::Result<bool> {
        Ok(self.inner.save(root)?)
    }

    /// Every row, in registration order.
    pub fn sources(&self) -> Vec<SourceSpec> {
        self.inner
            .sources
            .iter()
            .cloned()
            .map(SourceSpec::from_okf)
            .collect()
    }

    /// A source by its canonical (slugged) id.
    pub fn get(&self, id: &str) -> Option<SourceSpec> {
        self.inner.get(id).cloned().map(SourceSpec::from_okf)
    }

    /// Append a new source, or update an existing one in place; see
    /// `trusty_okf::build::registry::SourceRegistry::upsert`.
    pub fn upsert(&mut self, incoming: SourceSpec) -> (bool, bool) {
        self.inner.upsert(incoming.to_okf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the trusty-agents tools build and match typed rows; stored
    /// through trusty-okf, they must read back as the same typed rows.
    /// What: a mail row with a window and a drive row survive save and load.
    /// Test: self-contained.
    #[test]
    fn typed_rows_round_trip_through_trusty_okf() {
        let tmp = tempfile::tempdir().unwrap();
        let mut reg = SourceRegistry::default();
        let mail = SourceSpec::new(
            "mail",
            Some("mail"),
            Locator::Gmail {
                query: "in:sent".into(),
                after: Some("2026/01/01".into()),
                before: None,
            },
            "t0",
        );
        let drive = SourceSpec::new(
            "drive",
            None,
            Locator::Drive {
                folder_id: "root".into(),
                recursive: true,
            },
            "t0",
        );
        reg.upsert(mail.clone());
        reg.upsert(drive.clone());
        reg.save(tmp.path()).unwrap();
        let back = SourceRegistry::load(tmp.path()).unwrap();
        assert_eq!(back.sources(), vec![mail, drive]);
    }

    /// Why: §2.9 item 10 — a row the typed view cannot interpret is kept
    /// and saved unchanged, never dropped.
    /// What: a mail table with an extra key and an unknown kind stay
    /// `Connector` and save byte for byte.
    /// Test: self-contained.
    #[test]
    fn untyped_connector_rows_are_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("_sources/registry.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let toml = "# OKG source registry — maintained by the okg_ingest_* tools.\n\
                    # Hand edits are preserved; re-registering a source updates its row in place.\n\n\
                    [[sources]]\nid = \"m\"\ncollection = \"mail\"\nenabled = true\n\
                    tombstone_deleted = false\nuser_authored = false\nadded_at = \"t0\"\n\n\
                    [sources.locator.gmail]\nquery = \"in:sent\"\nlabel = \"x\"\n\n\
                    [[sources]]\nid = \"s\"\ncollection = \"chat\"\nenabled = true\n\
                    tombstone_deleted = false\nuser_authored = false\nadded_at = \"t0\"\n\n\
                    [sources.locator.slack]\nchannel = \"C1\"\n";
        std::fs::write(&path, toml).unwrap();
        let reg = SourceRegistry::load(tmp.path()).unwrap();
        let kinds: Vec<String> = reg.sources().iter().map(|s| s.kind().to_string()).collect();
        assert_eq!(kinds, ["gmail", "slack"]);
        assert!(matches!(
            reg.sources()[0].locator,
            Locator::Connector { .. }
        ));
        assert!(
            !reg.save(tmp.path()).unwrap(),
            "unchanged rows write nothing"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), toml);
    }
}
