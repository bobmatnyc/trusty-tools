//! Palace-level alias map: redirect one palace name to another (issue #1939).
//!
//! Why: trusty-mpm pins a managed session's `TRUSTY_MEMORY_PALACE` to the
//! `owner-repo` slug that [`crate::derive_palace_id`] produces (e.g.
//! `bobmatnyc-trusty-tools`), but the pre-existing claude-mpm-era palace for a
//! repo is the BARE repo name (`trusty-tools`). When the `owner-repo` palace has
//! never been created, every memory tool call fails with "palace metadata
//! missing" while the real history lives under the bare name — memory is
//! split-brained. This module persists a small alias map so a lookup for a
//! non-existent palace can be transparently redirected to an existing target,
//! letting BOTH names resolve to the one on-disk store.
//!
//! What: a JSON file `<registry_dir>/palace_aliases.json` mapping
//! `alias_name -> target_palace`, with atomic (tmp + rename) writes and a
//! forgiving read (a missing/empty/corrupt file is treated as "no aliases").
//! Resolution is consulted by [`crate::memory_core::registry::PalaceRegistry`]
//! ONLY when the requested palace has no metadata on disk, so aliases never
//! shadow a real palace of the same name. This is DISTINCT from the term/KG
//! entity aliases exposed by the `add_alias`/`discover_aliases` MCP tools — those
//! live INSIDE a palace's knowledge graph; this is a PALACE-level redirect that
//! lives beside the per-palace subdirectories.
//!
//! Test: `crate::palace_alias::tests` covers round-trip register/resolve, the
//! missing-file default, idempotent re-registration, self-alias rejection, and
//! the `palace_registry_dir_from` subdir resolution.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Filename for the persisted palace-alias map (beside per-palace subdirs).
///
/// Why: the map must live in the SAME directory the daemon uses as its palace
/// registry root (`AppState::data_root`) so a single lookup finds it. Naming it
/// with a `.json` suffix (not a directory) means the palace-listing walker —
/// which only descends into subdirectories containing `palace.json` — skips it
/// automatically.
/// What: `"palace_aliases.json"`.
/// Test: `register_then_resolve_round_trips` writes/reads this path.
const PALACE_ALIASES_JSON: &str = "palace_aliases.json";

/// On-disk shape of the alias map (versioned for forward compatibility).
///
/// Why: wrapping the flat map in a struct with an explicit `version` lets the
/// schema evolve (e.g. add per-alias metadata) without breaking older readers.
/// What: `version` (defaulted to 1 for files written before it existed) plus the
/// `aliases` map. Serialised with `serde_json` pretty output.
/// Test: `register_then_resolve_round_trips`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct PalaceAliasesFile {
    /// Schema version; defaults to 1 when absent so pre-version files still load.
    #[serde(default = "default_alias_schema_version")]
    version: u32,
    /// `alias_name -> target_palace` redirects. `BTreeMap` keeps the on-disk
    /// order stable so diffs are deterministic.
    #[serde(default)]
    aliases: BTreeMap<String, String>,
}

fn default_alias_schema_version() -> u32 {
    1
}

/// Stateless namespace for palace-alias persistence.
///
/// Why: like [`crate::memory_core::store::palace_store::PalaceStore`], alias
/// persistence has no state of its own — every operation is a pure function over
/// a registry directory. Grouping under a unit struct gives a stable import path.
/// What: `load_aliases` / `register_alias` / `resolve_alias`.
/// Test: this module's `tests`.
pub struct PalaceAliasStore;

impl PalaceAliasStore {
    /// Load the full alias map for a registry directory.
    ///
    /// Why: callers that want the whole map (diagnostics, bulk resolution) get it
    /// in one read. A missing file is the common case (no aliases registered yet)
    /// and must NOT be an error, so a fresh install behaves like an empty map.
    /// What: reads `<registry_dir>/palace_aliases.json`. Returns an empty map when
    /// the file is absent, empty, or fails to parse (corruption is logged and
    /// treated as "no aliases" so a bad file can never wedge palace resolution).
    /// Test: `load_missing_is_empty`, `register_then_resolve_round_trips`.
    pub fn load_aliases(registry_dir: &Path) -> Result<BTreeMap<String, String>> {
        let path = registry_dir.join(PALACE_ALIASES_JSON);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => {
                return Err(e).with_context(|| format!("read palace aliases at {}", path.display()));
            }
        };
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(BTreeMap::new());
        }
        match serde_json::from_slice::<PalaceAliasesFile>(&bytes) {
            Ok(file) => Ok(file.aliases),
            Err(e) => {
                // A corrupt alias file must not break palace resolution — the
                // authoritative data is the palaces themselves. Log and degrade
                // to "no aliases".
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "palace alias file is unparseable; ignoring (treating as no aliases)"
                );
                Ok(BTreeMap::new())
            }
        }
    }

    /// Register (or overwrite) an alias `alias_name -> target`, persisting it.
    ///
    /// Why: trusty-mpm calls this at session launch to bind a derived
    /// `owner-repo` name to an existing bare-repo palace so the split-brain
    /// resolves. It must be idempotent — relaunching the same session repeatedly
    /// must converge on one entry, not error or duplicate.
    /// What: rejects empty operands and a self-alias (`alias == target`, which
    /// would be a useless no-op / cycle). Otherwise loads the current map,
    /// inserts/updates the entry, and atomically writes it back (tmp + rename) so
    /// a crash mid-write cannot leave a half-written map. Creating an alias that
    /// already maps to the same target is a cheap no-op write. Returns `Ok(())`.
    /// Test: `register_then_resolve_round_trips`, `register_is_idempotent`,
    /// `register_self_alias_is_rejected`, `register_rejects_empty`.
    pub fn register_alias(registry_dir: &Path, alias: &str, target: &str) -> Result<()> {
        let alias = alias.trim();
        let target = target.trim();
        if alias.is_empty() || target.is_empty() {
            anyhow::bail!("palace alias and target must both be non-empty (alias={alias:?}, target={target:?})");
        }
        if alias == target {
            anyhow::bail!("refusing to register a self-referential palace alias {alias:?} -> {target:?}");
        }

        std::fs::create_dir_all(registry_dir)
            .with_context(|| format!("create registry dir {}", registry_dir.display()))?;

        let mut aliases = Self::load_aliases(registry_dir)?;
        aliases.insert(alias.to_string(), target.to_string());

        let file = PalaceAliasesFile {
            version: default_alias_schema_version(),
            aliases,
        };
        let bytes = serde_json::to_vec_pretty(&file).context("serialize palace aliases")?;

        let target_path = registry_dir.join(PALACE_ALIASES_JSON);
        let tmp_path = registry_dir.join(format!("{PALACE_ALIASES_JSON}.tmp"));
        std::fs::write(&tmp_path, &bytes)
            .with_context(|| format!("write palace aliases tmp {}", tmp_path.display()))?;
        std::fs::rename(&tmp_path, &target_path)
            .with_context(|| format!("rename palace aliases into {}", target_path.display()))?;
        Ok(())
    }

    /// Resolve a single alias to its target palace name, if one is registered.
    ///
    /// Why: the palace registry consults this when a requested palace has no
    /// on-disk metadata — a hit redirects the open to the target's store.
    /// What: loads the map and returns `Some(target)` when `alias` is present,
    /// `None` otherwise. A missing map yields `None`.
    /// Test: `register_then_resolve_round_trips`, `resolve_unknown_is_none`.
    pub fn resolve_alias(registry_dir: &Path, alias: &str) -> Result<Option<String>> {
        Ok(Self::load_aliases(registry_dir)?.get(alias.trim()).cloned())
    }
}

/// Resolve the directory that holds the per-palace subdirectories for a data dir.
///
/// Why: two on-disk layouts exist in the wild. The current code treats the data
/// dir itself as the parent of per-palace dirs (`<data_dir>/<id>/palace.json`);
/// legacy standalone installs nest everything under a `palaces/` subdirectory
/// (`<data_dir>/palaces/<id>/palace.json`), which is where real installs' data
/// lives. trusty-mpm (which registers aliases) and trusty-memory (which resolves
/// them) MUST agree on this directory or the alias file lands somewhere the
/// daemon never reads. Centralising the choice here keeps the two in lockstep —
/// trusty-memory's `resolve_palace_registry_dir` delegates to this.
/// What: returns `<data_dir>/palaces` when that subdirectory exists, else
/// `<data_dir>` itself.
/// Test: `palace_registry_dir_prefers_palaces_subdir`,
/// `palace_registry_dir_falls_back_to_data_dir`.
pub fn palace_registry_dir_from(data_dir: PathBuf) -> PathBuf {
    let nested = data_dir.join("palaces");
    if nested.is_dir() { nested } else { data_dir }
}

/// Resolve the default trusty-memory palace registry directory for this machine.
///
/// Why: trusty-mpm's session-launch alias registration must write the alias file
/// to the exact directory the trusty-memory daemon uses as its palace registry
/// root, WITHOUT depending on the `trusty-memory` crate. Reusing
/// [`crate::resolve_data_dir`] (which honours `TRUSTY_DATA_DIR_OVERRIDE`) keeps
/// path derivation identical to the daemon's, so tests and production agree.
/// What: resolves `resolve_data_dir("trusty-memory")` (the app data dir, created
/// if absent) and applies [`palace_registry_dir_from`] to pick the `palaces/`
/// subdir when present. Returns the registry directory.
/// Test: side-effect-bearing (creates the app data dir); the pure subdir choice
/// it composes is covered by `palace_registry_dir_*`.
pub fn default_palace_registry_dir() -> Result<PathBuf> {
    let data_dir = crate::resolve_data_dir("trusty-memory")?;
    Ok(palace_registry_dir_from(data_dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Why: a fresh install has no alias file; load must return an empty map,
    /// never an error, so palace resolution proceeds normally.
    /// Test: itself.
    #[test]
    fn load_missing_is_empty() {
        let tmp = tempdir().unwrap();
        let aliases = PalaceAliasStore::load_aliases(tmp.path()).expect("load");
        assert!(aliases.is_empty());
    }

    /// Why: the core contract — a registered alias must be readable back both via
    /// the full map and the single-key resolver, and it must survive as an
    /// on-disk file (a fresh read from the same dir).
    /// Test: itself.
    #[test]
    fn register_then_resolve_round_trips() {
        let tmp = tempdir().unwrap();
        PalaceAliasStore::register_alias(tmp.path(), "bobmatnyc-trusty-tools", "trusty-tools")
            .expect("register");

        // Single-key resolve.
        assert_eq!(
            PalaceAliasStore::resolve_alias(tmp.path(), "bobmatnyc-trusty-tools")
                .expect("resolve")
                .as_deref(),
            Some("trusty-tools")
        );
        // Full-map load reflects it too.
        let all = PalaceAliasStore::load_aliases(tmp.path()).expect("load");
        assert_eq!(all.get("bobmatnyc-trusty-tools").map(String::as_str), Some("trusty-tools"));
        // File actually exists on disk.
        assert!(tmp.path().join(PALACE_ALIASES_JSON).exists());
    }

    /// Why: relaunching the same managed session repeatedly must not duplicate or
    /// error — re-registering the same pair converges on one entry, and pointing
    /// an alias at a new target overwrites in place.
    /// Test: itself.
    #[test]
    fn register_is_idempotent() {
        let tmp = tempdir().unwrap();
        PalaceAliasStore::register_alias(tmp.path(), "a", "b").unwrap();
        PalaceAliasStore::register_alias(tmp.path(), "a", "b").unwrap();
        let all = PalaceAliasStore::load_aliases(tmp.path()).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all.get("a").map(String::as_str), Some("b"));

        // Overwrite to a new target.
        PalaceAliasStore::register_alias(tmp.path(), "a", "c").unwrap();
        let all = PalaceAliasStore::load_aliases(tmp.path()).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all.get("a").map(String::as_str), Some("c"));
    }

    /// Why: a self-alias is a useless no-op / potential cycle; it must be
    /// rejected so a mis-derived slug can never point a palace at itself.
    /// Test: itself.
    #[test]
    fn register_self_alias_is_rejected() {
        let tmp = tempdir().unwrap();
        assert!(PalaceAliasStore::register_alias(tmp.path(), "same", "same").is_err());
        assert!(PalaceAliasStore::register_alias(tmp.path(), "same", " same ").is_err());
    }

    /// Why: empty operands would create a meaningless map entry; guard against
    /// them so a caller passing a blank slug fails loudly rather than silently.
    /// Test: itself.
    #[test]
    fn register_rejects_empty() {
        let tmp = tempdir().unwrap();
        assert!(PalaceAliasStore::register_alias(tmp.path(), "", "b").is_err());
        assert!(PalaceAliasStore::register_alias(tmp.path(), "a", "  ").is_err());
    }

    /// Why: an alias that was never registered must resolve to `None` so the
    /// registry surfaces the normal "metadata missing" error.
    /// Test: itself.
    #[test]
    fn resolve_unknown_is_none() {
        let tmp = tempdir().unwrap();
        PalaceAliasStore::register_alias(tmp.path(), "a", "b").unwrap();
        assert_eq!(PalaceAliasStore::resolve_alias(tmp.path(), "zzz").unwrap(), None);
    }

    /// Why: a corrupt alias file must degrade to "no aliases", never panic or
    /// error, so it cannot wedge palace resolution.
    /// Test: itself.
    #[test]
    fn corrupt_file_degrades_to_empty() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join(PALACE_ALIASES_JSON), b"{ not json ]").unwrap();
        let all = PalaceAliasStore::load_aliases(tmp.path()).expect("load never errors on corruption");
        assert!(all.is_empty());
    }

    /// Why: when a `palaces/` subdir exists (the real-install layout) the
    /// registry dir must point INTO it so aliases sit beside the palaces.
    /// Test: itself.
    #[test]
    fn palace_registry_dir_prefers_palaces_subdir() {
        let tmp = tempdir().unwrap();
        let nested = tmp.path().join("palaces");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(palace_registry_dir_from(tmp.path().to_path_buf()), nested);
    }

    /// Why: absent a `palaces/` subdir the data dir itself is the registry root
    /// (the monorepo layout), matching trusty-memory's fallback.
    /// Test: itself.
    #[test]
    fn palace_registry_dir_falls_back_to_data_dir() {
        let tmp = tempdir().unwrap();
        assert_eq!(
            palace_registry_dir_from(tmp.path().to_path_buf()),
            tmp.path().to_path_buf()
        );
    }
}
