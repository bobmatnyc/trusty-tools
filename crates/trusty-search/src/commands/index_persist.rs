//! Persistence helpers for `trusty-search index`: writing registrations to the
//! global YAML config and the TOML allowlist.
//!
//! Why: extracted from `index.rs` to keep it under the 500-line cap. These
//! helpers perform best-effort I/O (failures are logged, not fatal) and have
//! no external callers beyond `index_one_with_filters`.
//! What: `persist_collection_to_global_config` writes/updates both the legacy
//! `config.yaml` and the TOML allowlist (`indexes.toml`) whenever a successful
//! `trusty-search index` invocation registers a new index.
//! Test: covered indirectly by `config::tests::roundtrip_preserves_fields`
//! (round-trip) and `config::tests::upsert_replaces_by_name` (idempotency).

use crate::commands::reindex_engine::RegisterFilters;
use crate::config::{CollectionConfig, GlobalConfig};

/// Write (or update) entries in the YAML config and the opt-in allowlist.
///
/// Why: issue #40 — the YAML config is the user-facing source of truth for
/// indexed projects. Every successful `trusty-search index` invocation must
/// add/update its matching `collections:` entry so a daemon restart preserves
/// the registration and `index remove` has a row to drop. Failures here are
/// non-fatal because the daemon-side registration already succeeded.
/// Issue #767: also write to the TOML allowlist (`indexes.toml`). Running
/// `trusty-search index <path>` is an explicit user gesture that implies
/// approval; persisting it to the allowlist makes the approval durable across
/// daemon restarts without requiring a separate `index add` invocation.
/// What: loads both config files, upserts entries, and saves atomically.
/// Warnings are emitted via `tracing::warn!` so daemon logs surface them
/// without polluting stdout.
/// Test: covered indirectly by `config::tests::roundtrip_preserves_fields`
/// (round-trip) and `config::tests::upsert_replaces_by_name` (idempotency);
/// CLI smoke tested by running `trusty-search index` twice and inspecting both
/// the resulting YAML and TOML files.
pub(super) fn persist_collection_to_global_config(
    index_name: &str,
    project_path: &std::path::Path,
    filters: &RegisterFilters,
) {
    // 1. Legacy YAML config (config.yaml).
    let mut cfg = match GlobalConfig::load() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("could not load global config to record index '{index_name}': {e:#}");
            return;
        }
    };
    cfg.upsert_collection(CollectionConfig {
        name: index_name.to_string(),
        path: project_path.to_path_buf(),
        extensions: filters.extensions.clone(),
        exclude: filters.exclude_globs.clone(),
        domain_terms: filters.domain_terms.clone(),
    });
    if let Err(e) = cfg.save() {
        tracing::warn!("could not save global config after registering '{index_name}': {e:#}");
    }

    // 2. Issue #767: also write to the TOML allowlist (indexes.toml).
    // Skip paths blocked by the denylist (shouldn't be reachable here because
    // the daemon already validated them, but be defensive).
    if crate::allowlist::is_denied(project_path).is_none() {
        let entry = crate::allowlist::AllowlistEntry {
            path: project_path.to_path_buf(),
            name: Some(index_name.to_string()),
            exclude: filters.exclude_globs.clone(),
            extensions: filters.extensions.clone(),
            skip_kg: filters.skip_kg,
        };
        if let Err(e) = crate::allowlist::add_to_allowlist(entry, None) {
            tracing::warn!("could not write allowlist entry for '{index_name}': {e:#}");
        }
    }
}
