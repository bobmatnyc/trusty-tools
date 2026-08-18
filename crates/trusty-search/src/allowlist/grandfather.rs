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
//! What: [`grandfather_existing_indexes`] runs once per install and is keyed on
//! the durable `.grandfathered` stamp, NOT on whether `allowlist.toml` exists.
//! Every registered root the allowlist union does not already approve is added,
//! whether the file was absent or merely incomplete. Each root is re-checked
//! against the hard denylist first, so a sensitive root that slipped in before
//! the gate existed is dropped rather than laundered into an approval. Every
//! decision is logged: approvals at `info`, denials at `warn`.
//!
//! # The distinction the stamp draws (#5926)
//!
//! Two absences look identical in `allowlist.toml` and mean opposite things:
//!
//! - **Never approved because the gate is new.** Before #5686 nothing read this
//!   file at registration time, so its contents were a partial record of
//!   `index add` calls, never a policy. A root missing from it was not refused —
//!   it was never asked about.
//! - **Explicitly de-approved.** After the pass has run, the file IS the policy,
//!   and a root the operator removed must stay removed.
//!
//! The stamp separates them: it is written only by a pass that has had its turn,
//! so its absence means no de-approval decision can have been recorded yet.
//! Keying on `allowlist.toml`'s existence instead is what dropped 103 of 121
//! registered indexes from warm boot on an upgrade whose allowlist file already
//! held ~24 hand-added entries (#5926).
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
    /// Roots added to the allowlist — created or merged into an existing file.
    pub seeded: Vec<std::path::PathBuf>,
    /// Roots refused by the hard denylist, with the denial reason.
    pub denied: Vec<(std::path::PathBuf, String)>,
    /// True when the pass did not run because the `.grandfathered` stamp says
    /// it has already had its turn on this install.
    ///
    /// #5926: this used to mean "did not run because `allowlist.toml` exists",
    /// which conflated a file the gate had never read with a curated policy.
    pub skipped_already_done: bool,
}

/// Seed the allowlist from the daemon's own index registry, once per install.
///
/// Why: see the module doc — this is what keeps switching on default-deny from
/// silently un-indexing a working install, and #5926 is what happens when the
/// trigger is the allowlist file's existence instead of the stamp.
/// What: no-op (with `skipped_already_done`) once the `.grandfathered` stamp
/// exists. Otherwise reads `registry_path` (`indexes.toml`), drops roots the
/// hard denylist refuses, drops roots the allowlist union already approves
/// (explicit entry, project registry, provisioned worktree, or containment),
/// and adds the remainder as explicit entries — creating `allowlist.toml` when
/// it is absent, merging into it when it is present but incomplete. Writes
/// nothing when the registry is empty, so a genuinely fresh install stays at
/// default-deny with no file.
/// Test: `grandfather_seeds_registered_roots`, `grandfather_skips_denied_roots`,
/// `grandfather_seeds_roots_missing_from_a_partial_allowlist`,
/// `grandfather_noop_once_the_stamp_exists`, `grandfather_noop_on_fresh_install`.
pub fn grandfather_existing_indexes(
    paths: &AllowlistPaths,
    registry_path: &Path,
) -> anyhow::Result<GrandfatherOutcome> {
    let allowlist_file = paths.allowlist_file();
    let stamp = stamp_path(&allowlist_file);
    // #5926: the stamp is the ONLY trigger. `allowlist.toml` existing is not
    // evidence the pass has run — before #5686 no gate read that file, so a
    // pre-upgrade copy is a partial record of `index add` calls rather than a
    // policy. Deleting the file after the pass ran IS a "reset to default-deny"
    // gesture, and the stamp is what makes that stick.
    if stamp.exists() {
        return Ok(GrandfatherOutcome {
            skipped_already_done: true,
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

    let existed = allowlist_file.exists();
    // #5926: merging means starting from what is already there. A file that
    // cannot be PARSED is not a file we may overwrite — that would discard the
    // operator's approvals over a syntax error. Leave it and the stamp alone so
    // the next boot retries once they have fixed it; warm-boot separately keeps
    // every entry while the allowlist is unreadable, so nothing is un-indexed
    // in the meantime.
    let mut cfg = if existed {
        match AllowlistConfig::load_from(&allowlist_file) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!(
                    "allowlist: {} exists but could not be parsed ({e:#}) — the one-time \
                     grandfather pass is NOT marked done and will retry once the file \
                     parses. Nothing was written and no approval was lost (#5926)",
                    allowlist_file.display()
                );
                return Ok(GrandfatherOutcome::default());
            }
        }
    } else {
        AllowlistConfig::default()
    };

    let mut outcome = GrandfatherOutcome::default();
    for entry in entries {
        let root = super::canonicalise(&entry.root_path);
        // #767: a root registered before the gate existed is still re-checked —
        // grandfathering must never launder a sensitive path into an approval.
        if let Some(reason) = super::is_denied(&root) {
            outcome.denied.push((root, reason));
            continue;
        }
        // #5926: ask the union exactly the question warm-boot asks, so the set
        // written here is precisely the set warm-boot would otherwise drop. An
        // Err means the union could not be evaluated; skip the root rather than
        // guess.
        match super::sources::resolve_allow_source(&root, paths) {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    "allowlist: could not resolve an approval source for {} ({e:#}) — \
                     leaving it out of the grandfather pass (#5926)",
                    root.display()
                );
                continue;
            }
        }
        // `resolve_allow_source` reads the file from disk, so a root added
        // earlier in THIS loop is not visible to it. Two registered entries for
        // the same root would otherwise be added twice.
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

    // Nothing to add: an allowlist that already covers every registered root
    // must not be rewritten at all. Stamp and stop.
    if outcome.seeded.is_empty() {
        write_stamp(&stamp);
        return Ok(outcome);
    }

    if existed {
        // Merge path. Read-modify-write over an existing file, the same shape
        // `add_to_allowlist` / `remove_from_allowlist` already use, so a racing
        // CLI write has the same (unchanged) exposure it always had.
        cfg.save_to(&allowlist_file)?;
    } else {
        // #767: create-new, not save-over. `exists()` above and a plain write
        // here are a check-then-write, and `save_to` renames over whatever is
        // there — an `index add` racing this pass between the two would be
        // silently discarded. `create_new(true)` is what detects the race.
        match create_new_toml(&allowlist_file, &cfg) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // #5926: merge into the file that appeared rather than
                // discarding the seed. Discarding it was safe while the pass
                // only ever ran on a fresh install; now the seed is the
                // migration, so throwing it away costs exactly the indexes this
                // pass exists to keep — and one `index add` landing in the
                // window would be enough to trigger it.
                tracing::info!(
                    "allowlist: {} appeared while the grandfather pass was running — \
                     merging the seed into it rather than discarding either (#5926)",
                    allowlist_file.display()
                );
                let mut raced = AllowlistConfig::load_from(&allowlist_file)?;
                outcome.seeded.retain(|root| !raced.contains(root));
                for root in &outcome.seeded {
                    raced.upsert(AllowlistEntry {
                        path: root.clone(),
                        name: None,
                        exclude: Vec::new(),
                        extensions: Vec::new(),
                        skip_kg: false,
                    });
                }
                if !outcome.seeded.is_empty() {
                    raced.save_to(&allowlist_file)?;
                }
            }
            Err(e) => return Err(e.into()),
        }
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
        "allowlist: one-time grandfather pass {} {} entr{} in {} \
         ({} refused by the sensitive-path denylist). Review it with \
         `trusty-search index list` and prune what you do not want indexed.",
        if existed { "merged" } else { "wrote" },
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
pub(super) fn stamp_path(allowlist_file: &Path) -> std::path::PathBuf {
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
/// Test: `grandfather_merges_with_a_concurrently_created_allowlist`,
/// `create_new_toml_refuses_an_existing_file`.
pub(super) fn create_new_toml(path: &Path, cfg: &AllowlistConfig) -> std::io::Result<()> {
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
