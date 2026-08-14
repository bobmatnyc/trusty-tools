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

    // 2. #767: record the per-root SETTINGS on an allowlist entry that already
    //    exists — never create one.
    //
    //    Indexing is not approving. This used to write an entry unconditionally
    //    after a successful registration, which promoted every DERIVED approval
    //    into a standing hand-file one: a root approved only because it is a
    //    provisioned worktree, a registered project, or inside an approved root
    //    became permanently `Explicit`. Two consequences, both bad. Removing the
    //    parent stopped stopping the child, weakening #767's own "removing it
    //    stops indexing" criterion. And every ephemeral
    //    `.claude/worktrees/<uuid>` left a permanent row behind, so the file an
    //    operator is supposed to read at a glance filled with dead paths.
    //
    //    `trusty-search index add` is the verb that approves. This only carries
    //    `exclude`/`extensions`/`skip_kg` onto an entry the operator already
    //    wrote, which is the one thing that would otherwise be lost.
    update_existing_allowlist_settings(index_name, project_path, filters, None);
}

/// Update an EXISTING allowlist entry's per-root settings; never create one.
///
/// Why: see the call site — creating an entry here launders a derived approval
/// into a standing one.
/// What: loads the allowlist, returns early unless `project_path` is already an
/// entry, then upserts that entry with this registration's filters. A denied
/// path is skipped defensively (the daemon already refused it).
/// `allowlist_path` is injectable for tests; `None` uses the real XDG path.
/// Test: `indexing_does_not_create_an_allowlist_entry`,
/// `indexing_updates_settings_on_an_existing_entry`.
fn update_existing_allowlist_settings(
    index_name: &str,
    project_path: &std::path::Path,
    filters: &RegisterFilters,
    allowlist_path: Option<&std::path::Path>,
) {
    if crate::allowlist::is_denied(project_path).is_some() {
        return;
    }
    let file = match allowlist_path {
        Some(p) => p.to_path_buf(),
        None => crate::allowlist::AllowlistConfig::default_path(),
    };
    let mut cfg = match crate::allowlist::AllowlistConfig::load_from(&file) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("could not load allowlist to record '{index_name}': {e:#}");
            return;
        }
    };
    if !cfg.contains(project_path) {
        return;
    }
    cfg.upsert(crate::allowlist::AllowlistEntry {
        path: project_path.to_path_buf(),
        name: Some(index_name.to_string()),
        exclude: filters.exclude_globs.clone(),
        extensions: filters.extensions.clone(),
        skip_kg: filters.skip_kg,
    });
    if let Err(e) = cfg.save_to(&file) {
        tracing::warn!("could not save allowlist after registering '{index_name}': {e:#}");
    }
}

#[cfg(test)]
mod tests_767 {
    use super::*;
    use crate::allowlist::{AllowlistConfig, AllowlistEntry};

    fn filters() -> RegisterFilters {
        RegisterFilters {
            exclude_globs: vec!["target/".to_string()],
            extensions: vec!["rs".to_string()],
            skip_kg: true,
            ..RegisterFilters::default()
        }
    }

    /// Indexing must NOT create an allowlist entry.
    ///
    /// Why (#767): this used to write one unconditionally after a successful
    /// registration, promoting every DERIVED approval — provisioned worktree,
    /// registered project, inside-an-approved-root — into a standing hand-file
    /// one. Removing the parent then stopped stopping the child, and every
    /// ephemeral `.claude/worktrees/<uuid>` left a permanent row behind.
    #[test]
    fn indexing_does_not_create_an_allowlist_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("allowlist.toml");
        let derived = std::path::PathBuf::from("/srv/project/.claude/worktrees/agent-x");

        update_existing_allowlist_settings("wt", &derived, &filters(), Some(&file));

        assert!(
            !file.exists(),
            "a derived approval must not create an allowlist file"
        );
    }

    /// An entry the operator already wrote DOES get this registration's
    /// per-root settings — that is the one thing that would otherwise be lost.
    #[test]
    fn indexing_updates_settings_on_an_existing_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("allowlist.toml");
        let approved = std::path::PathBuf::from("/srv/project");
        let mut cfg = AllowlistConfig::default();
        cfg.upsert(AllowlistEntry {
            path: approved.clone(),
            name: None,
            exclude: Vec::new(),
            extensions: Vec::new(),
            skip_kg: false,
        });
        cfg.save_to(&file).expect("seed");

        update_existing_allowlist_settings("proj", &approved, &filters(), Some(&file));

        let cfg = AllowlistConfig::load_from(&file).expect("load");
        assert_eq!(cfg.entries.len(), 1, "{cfg:?}");
        assert_eq!(cfg.entries[0].name.as_deref(), Some("proj"));
        assert_eq!(cfg.entries[0].exclude, vec!["target/".to_string()]);
        assert!(cfg.entries[0].skip_kg);
    }
}
