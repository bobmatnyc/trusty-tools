//! One-time grandfather pass: seed `allowlist.toml` from the roots the daemon
//! is already serving.
//!
//! Why: #767's gate is default-deny, and on the box that motivated it the
//! allowlist file did not exist at all while seven roots were registered and
//! being served. Switching the gate on without this pass would have stopped
//! indexing all seven the moment the daemon restarted — a silent break of a
//! working setup, which is the one outcome the gate must not cause. Seeding
//! preserves exactly what was already there and nothing more: the pass adds no
//! root the daemon was not already serving.
//!
//! What: [`grandfather_existing_indexes`] runs ONLY when `allowlist.toml` is
//! absent — the operator's own file, including one they emptied on purpose, is
//! never rewritten and a removed root is never resurrected. Each registered
//! root is re-checked against the hard denylist before it is written, so a
//! sensitive root that slipped in before the gate existed is dropped here
//! rather than laundered into an approval. Every decision is logged: approvals
//! at `info`, denials at `warn`.
//!
//! Test: `grandfather_tests.rs`.

use std::path::Path;

use super::sources::AllowlistPaths;
use super::{AllowlistConfig, AllowlistEntry};

/// Outcome of one grandfather pass, for logging and tests.
///
/// Why: the caller reports what happened to the operator, and the tests assert
/// on it rather than on log output.
/// What: the roots written, and the roots refused by the denylist with reasons.
/// Test: `grandfather_seeds_registered_roots`, `grandfather_skips_denied_roots`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct GrandfatherOutcome {
    /// Roots written into the freshly created allowlist.
    pub seeded: Vec<std::path::PathBuf>,
    /// Roots refused by the hard denylist, with the denial reason.
    pub denied: Vec<(std::path::PathBuf, String)>,
    /// True when the pass did not run because `allowlist.toml` already exists.
    pub skipped_existing: bool,
}

/// Seed a missing `allowlist.toml` from the daemon's own index registry.
///
/// Why: see the module doc — this is what keeps switching on default-deny from
/// silently un-indexing a working install.
/// What: no-op (with `skipped_existing`) when the allowlist file already
/// exists. Otherwise reads `registry_path` (`indexes.toml`), drops roots the
/// hard denylist refuses, drops roots the project registry already approves
/// (writing them would duplicate an approval that has its own lifecycle), and
/// writes the remainder as explicit entries. Writes nothing when the registry
/// is empty, so a genuinely fresh install stays at default-deny with no file.
/// Test: `grandfather_seeds_registered_roots`, `grandfather_skips_denied_roots`,
/// `grandfather_noop_when_allowlist_exists`, `grandfather_noop_on_fresh_install`.
pub fn grandfather_existing_indexes(
    paths: &AllowlistPaths,
    registry_path: &Path,
) -> anyhow::Result<GrandfatherOutcome> {
    let allowlist_file = paths.allowlist_file();
    if allowlist_file.exists() {
        return Ok(GrandfatherOutcome {
            skipped_existing: true,
            ..Default::default()
        });
    }

    let entries =
        crate::service::persistence::load_index_registry_at(registry_path).unwrap_or_default();
    if entries.is_empty() {
        return Ok(GrandfatherOutcome::default());
    }

    let project_roots: Vec<std::path::PathBuf> =
        super::sources::project_roots(&paths.project_paths_file())
            .iter()
            .map(|p| super::canonicalise(p))
            .collect();

    let mut outcome = GrandfatherOutcome::default();
    let mut cfg = AllowlistConfig::default();
    for entry in entries {
        let root = super::canonicalise(&entry.root_path);
        // #767: a root registered before the gate existed is still re-checked —
        // grandfathering must never launder a sensitive path into an approval.
        if let Some(reason) = super::is_denied(&root) {
            outcome.denied.push((root, reason));
            continue;
        }
        if project_roots.contains(&root) {
            continue;
        }
        if cfg.contains(&root) {
            continue;
        }
        cfg.upsert(AllowlistEntry {
            path: root.clone(),
            name: Some(entry.id.clone()),
            exclude: Vec::new(),
            extensions: Vec::new(),
            skip_kg: false,
        });
        outcome.seeded.push(root);
    }

    cfg.save_to(&allowlist_file)?;

    for root in &outcome.seeded {
        tracing::info!(
            "allowlist: grandfathered already-registered root {} into {} (#767)",
            root.display(),
            allowlist_file.display()
        );
    }
    for (root, reason) in &outcome.denied {
        tracing::warn!(
            "allowlist: refusing to grandfather {} — {reason}; this root will \
             STOP being indexed (#767)",
            root.display()
        );
    }
    tracing::info!(
        "allowlist: first-run grandfather pass wrote {} entr{} to {} \
         ({} refused by the sensitive-path denylist). Review it with \
         `trusty-search index list` and prune what you do not want indexed.",
        outcome.seeded.len(),
        if outcome.seeded.len() == 1 {
            "y"
        } else {
            "ies"
        },
        allowlist_file.display(),
        outcome.denied.len(),
    );
    Ok(outcome)
}
