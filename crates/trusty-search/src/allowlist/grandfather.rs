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
    let stamp = stamp_path(&allowlist_file);
    // #767: "the allowlist file is missing" is NOT on its own evidence that the
    // pass has never run. Deleting `allowlist.toml` is a plausible "reset to
    // default-deny" gesture, and without a durable stamp the next start would
    // re-seed every registered root as a standing approval — undoing exactly
    // what the operator just did. Both conditions must hold.
    if allowlist_file.exists() || stamp.exists() {
        return Ok(GrandfatherOutcome {
            skipped_existing: true,
            ..Default::default()
        });
    }

    let entries = match crate::service::persistence::load_index_registry_at(registry_path) {
        Ok(e) => e,
        Err(e) => {
            // Do NOT stamp here. The stamp means "the pass has had its turn",
            // and this boot never got one — the registry was unreadable or the
            // path was wrong (a misdirected `TRUSTY_DATA_DIR` is the likely
            // cause, and it is transient). Stamping would permanently burn the
            // one-time pass on a boot that grandfathered nothing, and the next
            // GOOD boot would then seed nothing while warm-boot silently
            // dropped every previously-served root. Return without stamping so
            // the next boot retries.
            tracing::warn!(
                "allowlist: could not read the index registry at {} ({e:#}) — \
                 nothing was grandfathered this boot and the one-time pass is \
                 NOT marked done, so it retries on the next start. If roots stop \
                 being indexed, approve them with `trusty-search index add` (#767)",
                registry_path.display()
            );
            return Ok(GrandfatherOutcome::default());
        }
    };
    if entries.is_empty() {
        // A successfully-read but EMPTY registry is a real decision: a fresh
        // install has nothing to grandfather, and the pass is done. Stamping
        // here is what stops a later `index add` plus a file deletion from
        // resurrecting a seed pass.
        write_stamp(&stamp);
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

    // #767: create-new, not save-over. `exists()` above and a plain write here
    // are a check-then-write: an `index add` racing this pass between the two
    // would be clobbered by the seed. `create_new(true)` makes the concurrent
    // writer win — we lose the seed rather than lose their approval, and the
    // stamp below still records that the pass has had its turn.
    match create_new_toml(&allowlist_file, &cfg) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            tracing::info!(
                "allowlist: {} appeared while the grandfather pass was running — \
                 keeping that file and discarding the seed (#767)",
                allowlist_file.display()
            );
            write_stamp(&stamp);
            return Ok(GrandfatherOutcome {
                skipped_existing: true,
                ..Default::default()
            });
        }
        Err(e) => return Err(e.into()),
    }
    write_stamp(&stamp);

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

/// Path of the durable "the grandfather pass has run" stamp.
///
/// Why: the pass must be one-time in fact, not merely one-time while the file it
/// writes survives — see [`grandfather_existing_indexes`]. A sibling of
/// `allowlist.toml` keeps the two together, so moving a config directory moves
/// both and a genuinely fresh install has neither.
/// What: `<allowlist.toml's dir>/.grandfathered`.
/// Test: `grandfather_does_not_reseed_after_the_allowlist_is_deleted`.
fn stamp_path(allowlist_file: &Path) -> std::path::PathBuf {
    match allowlist_file.parent() {
        Some(dir) => dir.join(".grandfathered"),
        None => std::path::PathBuf::from(".grandfathered"),
    }
}

/// Record that the pass has run.
///
/// Best-effort: failing to write the stamp must not fail a daemon start, and the
/// worst case is one extra pass that `allowlist.toml`'s own existence blocks.
fn write_stamp(stamp: &Path) {
    if let Some(dir) = stamp.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(
        stamp,
        b"trusty-search: the #767 allowlist grandfather pass has run; \
delete this file to let it seed again\n",
    ) {
        tracing::warn!(
            "allowlist: could not write the grandfather stamp {} ({e}) — the pass \
             may run again if allowlist.toml is deleted (#767)",
            stamp.display()
        );
    }
}

/// Write `cfg` to `path`, failing with `AlreadyExists` when someone beat us.
///
/// Why: `AllowlistConfig::save_to` writes a temp file and renames over whatever
/// is there, which is right for an update and wrong for a first-run seed racing
/// a concurrent `index add` — the rename would silently discard that approval.
/// `create_new(true)` is what makes the other writer win instead.
/// What: serialises to TOML, then opens with `create_new(true)` and writes.
/// Test: `grandfather_yields_to_a_concurrently_created_allowlist`.
fn create_new_toml(path: &Path, cfg: &AllowlistConfig) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let toml_str = toml::to_string_pretty(cfg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(toml_str.as_bytes())
}
