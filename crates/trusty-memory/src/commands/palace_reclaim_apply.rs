//! `palace reclaim --apply` and `--purge-trash` (#9140 ruling f0).
//!
//! Why: the owner reviewed the dry-run list and ruled that every listed item
//! goes, through a trash dir that is purged after 7 days. The apply must act
//! on exactly the reviewed list, so it re-runs the same [`scan`] and moves an
//! item only when the fresh scan still lists it unchanged.
//! What: [`apply`] matches each reviewed item to the fresh scan — by path,
//! size and mtime, or by palace and drawer id for a fixture turn — and skips
//! and reports every other item. A matched file or directory is renamed into
//! `<root>/.trash/<UTC-date>-reclaim/` at its relative path while this process
//! holds the `flock` on every store it would move; a palace the live daemon
//! holds is skipped. A fixture drawer is exported as JSON into the trash dir,
//! then removed through the daemon's `memory.drawer_delete` RPC; with no
//! daemon serving, it is skipped. `manifest.json` records every attempted
//! item. Any skip or error leaves the item in place and makes the exit
//! non-zero.
//! Test: `apply_moves_the_reviewed_items_and_writes_the_manifest`,
//! `apply_skips_an_item_that_changed_since_review`,
//! `apply_skips_a_palace_whose_store_is_locked`,
//! `apply_refuses_a_bad_root_before_touching_anything`,
//! `apply_continues_past_a_rename_failure_and_deletes_nothing`,
//! `apply_routes_fixture_drawers_through_the_remover_and_exports_them`,
//! `move_one_skips_an_item_changed_after_the_scan`,
//! `move_one_refuses_to_overwrite_an_existing_trash_path`,
//! `apply_refuses_a_list_reviewed_for_another_root`,
//! `apply_refuses_a_symlinked_trash_dir`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::palace_reclaim::{mtime_unix, scan, tree_bytes, ReclaimClass, ReclaimItem};
use super::palace_reclaim_trash::{
    check_root, lock_stores, prepare_trash_dir, purge_trash, read_manifest, write_manifest,
    EntryStatus, StoreLocks, TrashEntry,
};
use super::store_snapshot::with_store_copy;

/// Budget for one `memory.drawer_delete` call.
const DRAWER_DELETE_TIMEOUT: Duration = Duration::from_secs(30);

/// The reviewed list: the `--json` dry-run output the owner signed off.
#[derive(Debug, Deserialize)]
pub struct ReviewedList {
    pub root: PathBuf,
    pub items: Vec<ReclaimItem>,
}

/// How an item is recognised in the fresh scan.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ItemKey {
    Drawer {
        palace: Option<String>,
        id: String,
    },
    Path {
        class: ReclaimClass,
        path: PathBuf,
        bytes: u64,
        mtime: Option<i64>,
    },
}

/// The match key: drawer id for a fixture turn, else path + size + mtime.
fn key(i: &ReclaimItem) -> ItemKey {
    match (&i.class, &i.drawer_id) {
        (ReclaimClass::FixtureDrawer, Some(id)) => ItemKey::Drawer {
            palace: i.palace.clone(),
            id: id.clone(),
        },
        _ => ItemKey::Path {
            class: i.class,
            path: i.path.clone(),
            bytes: i.bytes,
            mtime: i.mtime_unix,
        },
    }
}

/// Everything [`apply`] needs from outside the reviewed list.
pub struct ApplyEnv<'a> {
    /// Home dir for the root check; `None` refuses.
    pub home: Option<&'a Path>,
    pub now_unix: i64,
    /// UTC `YYYY-MM-DD`, the trash dir's date.
    pub date: String,
    /// The reviewed list's own path, recorded in the manifest.
    pub reviewed_path: PathBuf,
    /// Removes one drawer `(palace, drawer_id)`; production routes it to the
    /// daemon's `memory.drawer_delete`.
    pub remove_drawer: &'a mut dyn FnMut(&str, &str) -> Result<()>,
}

/// What one apply run did.
#[derive(Debug, Default, Serialize)]
pub struct ApplyReport {
    pub root: PathBuf,
    pub trash_dir: PathBuf,
    /// Every attempted item with its final status.
    pub entries: Vec<TrashEntry>,
    /// Reviewed items the fresh scan does not list unchanged: `(label, why)`.
    pub unmatched: Vec<(String, String)>,
    /// Items the fresh scan lists that the review did not; never touched.
    pub new_since_review: Vec<String>,
}

impl ApplyReport {
    fn count(&self, status: EntryStatus) -> usize {
        self.entries.iter().filter(|e| e.status == status).count()
    }

    /// Items the run left in place: unmatched, skipped or failed.
    pub fn not_applied(&self) -> usize {
        self.unmatched.len() + self.count(EntryStatus::Skipped) + self.count(EntryStatus::Failed)
    }

    /// One summary line, plus one line per item not applied.
    pub fn render_text(&self) -> String {
        use std::fmt::Write as _;
        let mut out = format!(
            "palace reclaim APPLY root={} trash={}\nsummary moved={} moved_with_parent={} \
             drawers_removed={} skipped={} failed={} unmatched={} new_since_review={}\n",
            self.root.display(),
            self.trash_dir.display(),
            self.count(EntryStatus::Moved),
            self.count(EntryStatus::MovedWithParent),
            self.count(EntryStatus::DrawerRemoved),
            self.count(EntryStatus::Skipped),
            self.count(EntryStatus::Failed),
            self.unmatched.len(),
            self.new_since_review.len(),
        );
        for e in &self.entries {
            if matches!(e.status, EntryStatus::Skipped | EntryStatus::Failed) {
                let _ = writeln!(
                    out,
                    "{:?}\t{}\t{}",
                    e.status,
                    label_of(&e.original_path, e.drawer_id.as_deref()),
                    e.detail.as_deref().unwrap_or("")
                );
            }
        }
        for (label, why) in &self.unmatched {
            let _ = writeln!(out, "Unmatched\t{label}\t{why}");
        }
        for label in &self.new_since_review {
            let _ = writeln!(
                out,
                "NewSinceReview\t{label}\tnot in the reviewed list; untouched"
            );
        }
        out
    }
}

fn label_of(path: &Path, drawer: Option<&str>) -> String {
    match drawer {
        Some(id) => format!("{}#{id}", path.display()),
        None => path.display().to_string(),
    }
}

/// Move every reviewed item that the fresh scan still lists unchanged into
/// the dated trash dir (#9140 ruling f0).
///
/// Why: see the module doc. Plan and apply are one source: both are [`scan`].
/// What: refuses a bad root, a list reviewed for another root, or a trash dir
/// that cannot be made, before anything moves. Then writes the manifest with
/// every matched item `pending`, moves the items, and rewrites it with each
/// item's final status. A directory item carries the items inside it.
///
/// # Errors
///
/// Only the up-front refusals and a manifest write; an item's own error is a
/// `failed` entry and the run continues.
///
/// Test: see the module doc.
pub fn apply(root: &Path, reviewed: &ReviewedList, env: &mut ApplyEnv<'_>) -> Result<ApplyReport> {
    // #9140: the root check comes before the scan and before any mkdir.
    let canon = check_root(root, env.home)?;
    let listed_root = std::fs::canonicalize(&reviewed.root).unwrap_or(reviewed.root.clone());
    if listed_root != canon {
        bail!(
            "the reviewed list is for {}, not {}; refusing (#9140)",
            reviewed.root.display(),
            canon.display()
        );
    }
    // Scan the root as the dry run spelled it, so listed paths compare equal.
    let root = reviewed.root.clone();
    let fresh = scan(&root, env.now_unix)?;
    let fresh_keys: HashSet<ItemKey> = fresh.items.iter().map(key).collect();
    let reviewed_keys: HashSet<ItemKey> = reviewed.items.iter().map(key).collect();

    let mut report = ApplyReport {
        root: root.clone(),
        ..Default::default()
    };
    report.new_since_review = fresh
        .items
        .iter()
        .filter(|i| !reviewed_keys.contains(&key(i)))
        .map(|i| label_of(&i.path, i.drawer_id.as_deref()))
        .collect();
    // #9140: an item is acted on only when the fresh scan lists it unchanged.
    let mut matched = Vec::new();
    for item in &reviewed.items {
        if fresh_keys.contains(&key(item)) {
            matched.push(item);
        } else {
            report.unmatched.push((
                label_of(&item.path, item.drawer_id.as_deref()),
                "changed, vanished, or no longer listed since the review".into(),
            ));
        }
    }

    let trash = prepare_trash_dir(&root, &env.date)?;
    report.trash_dir = trash.clone();
    let mut manifest = read_manifest(&trash, &root)?;
    manifest.reviewed_lists.push(env.reviewed_path.clone());
    let first = manifest.entries.len();
    for item in &matched {
        manifest.entries.push(pending_entry(item, &root, &trash));
    }
    write_manifest(&trash, &manifest)?;

    let entries = &mut manifest.entries[first..];
    move_paths(&root, &matched, entries);
    remove_drawers(&root, &matched, entries, env.remove_drawer);
    write_manifest(&trash, &manifest)?;
    report.entries = manifest.entries.split_off(first);
    Ok(report)
}

/// The manifest row for a matched item, before it moves.
fn pending_entry(item: &ReclaimItem, root: &Path, trash: &Path) -> TrashEntry {
    let trash_path = match (&item.class, &item.drawer_id) {
        (ReclaimClass::FixtureDrawer, Some(id)) => trash
            .join(item.palace.as_deref().unwrap_or("_"))
            .join("kg.redb.drawers")
            .join(format!("{id}.json")),
        _ => trash.join(item.path.strip_prefix(root).unwrap_or(&item.path)),
    };
    TrashEntry {
        original_path: item.path.clone(),
        trash_path,
        class: item.class,
        bytes: item.bytes,
        reason: item.reason.clone(),
        palace: item.palace.clone(),
        drawer_id: item.drawer_id.clone(),
        status: EntryStatus::Pending,
        detail: None,
    }
}

fn set(entry: &mut TrashEntry, status: EntryStatus, detail: Option<String>) {
    entry.status = status;
    entry.detail = detail;
}

/// Move every matched file and directory item; nested items follow their
/// directory.
fn move_paths(root: &Path, matched: &[&ReclaimItem], entries: &mut [TrashEntry]) {
    let is_dir_item =
        |i: &ReclaimItem| matches!(i.class, ReclaimClass::EmptyPalace | ReclaimClass::OrphanDir);
    let parent_of: Vec<Option<usize>> = matched
        .iter()
        .map(|i| {
            (i.class != ReclaimClass::FixtureDrawer)
                .then(|| {
                    matched.iter().position(|d| {
                        is_dir_item(d) && d.path != i.path && i.path.starts_with(&d.path)
                    })
                })
                .flatten()
        })
        .collect();
    for (n, item) in matched.iter().enumerate() {
        if item.class == ReclaimClass::FixtureDrawer || parent_of[n].is_some() {
            continue;
        }
        let lock_dir = if is_dir_item(item) {
            Some(item.path.clone())
        } else {
            item.palace.as_ref().map(|p| root.join(p))
        };
        let (status, detail) = match move_one(item, lock_dir.as_deref(), &entries[n].trash_path) {
            Ok(()) => (EntryStatus::Moved, None),
            Err(Outcome::Skip(why)) => (EntryStatus::Skipped, Some(why)),
            Err(Outcome::Fail(e)) => (EntryStatus::Failed, Some(format!("{e:#}"))),
        };
        set(&mut entries[n], status, detail);
    }
    for (n, parent) in parent_of.iter().enumerate() {
        let Some(p) = *parent else { continue };
        let (status, detail) = match entries[p].status {
            EntryStatus::Moved => (EntryStatus::MovedWithParent, None),
            _ => (
                EntryStatus::Skipped,
                Some(format!(
                    "inside {}, which was not moved",
                    matched[p].path.display()
                )),
            ),
        };
        set(&mut entries[n], status, detail);
    }
}

/// Why an item was left in place.
enum Outcome {
    Skip(String),
    Fail(anyhow::Error),
}

impl From<anyhow::Error> for Outcome {
    fn from(e: anyhow::Error) -> Self {
        Self::Fail(e)
    }
}

/// Rename one item to `target` while holding the locks on `lock_dir`'s stores.
///
/// Why (#9140 ruling f0): a held palace is never moved or modified; an item
/// that changed between the scan and the move is skipped; a rename that fails
/// leaves the item where it was, and nothing falls back to copy-and-delete.
/// What: takes [`lock_stores`] on `lock_dir`, re-measures size and mtime under
/// the locks, refuses an existing `target`, creates its parent, renames.
fn move_one(
    item: &ReclaimItem,
    lock_dir: Option<&Path>,
    target: &Path,
) -> std::result::Result<(), Outcome> {
    let _locks = match lock_dir.map(lock_stores).transpose()? {
        Some(StoreLocks::Held(store)) => {
            return Err(Outcome::Skip(format!(
                "store {} is held open by the live daemon (#9140)",
                store.display()
            )));
        }
        Some(StoreLocks::Acquired(files)) => files,
        None => Vec::new(),
    };
    // #9140: re-check under the locks; the scan ran before they were taken.
    if tree_bytes(&item.path) != item.bytes || mtime_unix(&item.path) != item.mtime_unix {
        return Err(Outcome::Skip(
            "changed between the scan and the move".into(),
        ));
    }
    if std::fs::symlink_metadata(target).is_ok() {
        return Err(Outcome::Fail(anyhow!(
            "trash path {} already exists; refusing to overwrite it",
            target.display()
        )));
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::rename(&item.path, target)
        .with_context(|| format!("rename {} -> {}", item.path.display(), target.display()))?;
    Ok(())
}

/// Export each matched fixture drawer into the trash, then remove it through
/// `remove_drawer`.
///
/// Why (#9140 ruling f0): a drawer lives inside `kg.redb`, which the live
/// daemon holds. Removing it through the store API would need the daemon's
/// write lock and would leave its vector, BM25 and KG copies behind; the
/// daemon's own `memory.drawer_delete` removes all of them. The export is
/// what makes the removal recoverable, so it is written first.
/// What: reads each palace's drawers once from a private store copy; writes
/// `<trash>/<palace>/kg.redb.drawers/<id>.json`; calls `remove_drawer`; on
/// any error deletes that export and marks the drawer failed.
fn remove_drawers(
    root: &Path,
    matched: &[&ReclaimItem],
    entries: &mut [TrashEntry],
    remove_drawer: &mut dyn FnMut(&str, &str) -> Result<()>,
) {
    let scratch = std::env::temp_dir();
    let mut by_palace: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (n, item) in matched.iter().enumerate() {
        if item.class == ReclaimClass::FixtureDrawer {
            by_palace
                .entry(item.palace.as_deref().unwrap_or(""))
                .or_default()
                .push(n);
        }
    }
    for (palace, rows) in by_palace {
        let loaded = with_store_copy(&root.join(palace), &scratch, |s| s.load_drawers());
        let drawers: HashMap<String, _> = match loaded {
            Ok(d) => d
                .unwrap_or_default()
                .into_iter()
                .map(|d| (d.id.to_string(), d))
                .collect(),
            Err(e) => {
                for n in rows {
                    set(
                        &mut entries[n],
                        EntryStatus::Failed,
                        Some(format!("read store copy: {e:#}")),
                    );
                }
                continue;
            }
        };
        for n in rows {
            let id = matched[n].drawer_id.clone().unwrap_or_default();
            let Some(drawer) = drawers.get(&id) else {
                set(
                    &mut entries[n],
                    EntryStatus::Skipped,
                    Some("drawer no longer in the store".into()),
                );
                continue;
            };
            let export = entries[n].trash_path.clone();
            let result = write_export(&export, drawer).and_then(|()| {
                remove_drawer(palace, &id).inspect_err(|_| {
                    let _ = std::fs::remove_file(&export);
                })
            });
            match result {
                Ok(()) => set(&mut entries[n], EntryStatus::DrawerRemoved, None),
                Err(e) => set(&mut entries[n], EntryStatus::Failed, Some(format!("{e:#}"))),
            }
        }
    }
}

/// Write one drawer's JSON export, refusing to overwrite an earlier one.
fn write_export(path: &Path, drawer: &impl Serialize) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(drawer)?;
    let mut f = std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create drawer export {}", path.display()))?;
    std::io::Write::write_all(&mut f, &bytes)
        .and_then(|()| f.sync_all())
        .with_context(|| format!("write drawer export {}", path.display()))
}

/// The live palace root and the home dir, the inputs every handler needs.
fn live_root() -> Result<PathBuf> {
    let data_dir = trusty_common::resolve_data_dir("trusty-memory")
        .context("resolve trusty-memory data dir")?;
    Ok(crate::resolve_palace_registry_dir(data_dir))
}

/// The production drawer remover: the daemon's `memory.drawer_delete`.
///
/// Why: see [`remove_drawers`]. With no daemon serving, every call fails, so
/// no drawer is removed and every export is deleted again.
fn daemon_remover(rt: tokio::runtime::Handle) -> impl FnMut(&str, &str) -> Result<()> {
    let socket = crate::transport::uds::socket_path();
    let serving = rt.block_on(crate::client::is_running());
    move |palace, id| {
        if !serving {
            bail!("the trusty-memory daemon is not serving; drawers are removed only through its memory.drawer_delete");
        }
        let socket = socket.as_ref().map_err(|e| anyhow!("{e:#}"))?;
        let params = serde_json::json!({ "palace_id": palace, "drawer_id": id });
        rt.block_on(crate::client::call_at(
            socket,
            "memory.drawer_delete",
            params,
            DRAWER_DELETE_TIMEOUT,
        ))
        .map(|_| ())
    }
}

/// `trusty-memory palace reclaim --apply --manifest <file>`.
///
/// Why (#9140 ruling f0): the operator entry point for the one reviewed run.
/// What: reads the reviewed list, runs [`apply`] against the live root,
/// prints the report, and fails with counts when any item was left in place.
/// Test: `apply` is covered by the tests named in the module doc.
pub fn handle_apply(manifest: &Path, json: bool, rt: tokio::runtime::Handle) -> Result<()> {
    let bytes = std::fs::read(manifest)
        .with_context(|| format!("read reviewed list {}", manifest.display()))?;
    let reviewed: ReviewedList = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse reviewed list {}", manifest.display()))?;
    let home = dirs::home_dir();
    let mut remover = daemon_remover(rt);
    let mut env = ApplyEnv {
        home: home.as_deref(),
        now_unix: i64::try_from(crate::palace_last_used::now_unix()).unwrap_or(i64::MAX),
        date: chrono::Utc::now().format("%Y-%m-%d").to_string(),
        reviewed_path: manifest.to_path_buf(),
        remove_drawer: &mut remover,
    };
    let report = apply(&live_root()?, &reviewed, &mut env)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", report.render_text());
    }
    let left = report.not_applied();
    if left > 0 {
        bail!("palace reclaim --apply left {left} reviewed item(s) in place; see the report above (#9140)");
    }
    Ok(())
}

/// `trusty-memory palace reclaim --purge-trash`.
///
/// Why (#9140 ruling f0): trash older than 7 days is purged by the operator,
/// not by a daemon hook, so the only deleting path runs when someone asks.
/// What: vets the root, runs [`purge_trash`], prints what it removed and
/// kept, and fails when a removal errored.
/// Test: `purge_trash` is covered by the tests named in its doc.
pub fn handle_purge(json: bool) -> Result<()> {
    let home = dirs::home_dir();
    let root = check_root(&live_root()?, home.as_deref())?;
    let report = purge_trash(&root, chrono::Utc::now().date_naive())?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for p in &report.removed {
            println!("removed\t{}", p.display());
        }
        for (p, why) in &report.kept {
            println!("kept\t{}\t{why}", p.display());
        }
        for (p, e) in &report.failed {
            println!("failed\t{}\t{e}", p.display());
        }
    }
    if !report.failed.is_empty() {
        bail!(
            "palace reclaim --purge-trash failed on {} trash dir(s) (#9140)",
            report.failed.len()
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "palace_reclaim_apply_tests.rs"]
mod tests;
