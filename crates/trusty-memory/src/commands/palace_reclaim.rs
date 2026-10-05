//! `trusty-memory palace reclaim` — a dry-run list of reclaimable palace data (#9140).
//!
//! Why: the live palace store carried 1.09 GB of quarantined `*.v2-incompatible`
//! files, empty palaces, KG backups, directories with no `palace.json`, a stale
//! `uds_addr`, and ~2,300 trusty-code e2e fixture turns (#9139). The owner ruled
//! that nothing is deleted until he has reviewed a list of exactly what would
//! go, so the first deliverable is that list and nothing else.
//! What: READ-ONLY. [`scan`] walks the palace root and classifies each
//! candidate with its path, size, mtime, palace and reason. Drawer counts and
//! fixture turns are read from a private copy of each `kg.redb`
//! ([`super::store_snapshot::with_store_copy`]), so no live store is opened,
//! locked or written. This module has no delete path; the apply and the
//! trash purge (#9140 ruling f0) live in `palace_reclaim_apply.rs` and
//! re-use [`scan`], so the reviewed list and the apply share one source.
//! Test: `reclaim_scan_lists_every_class_and_writes_nothing`,
//! `reclaim_scan_keeps_recent_and_nonempty_palaces`,
//! `fixture_turns_match_only_the_fixture_prompt_set` (in
//! `palace_reclaim_tests.rs`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use trusty_common::memory_core::palace::Drawer;
use trusty_common::memory_core::store::INCOMPATIBLE_SUFFIX;

use super::store_snapshot::with_store_copy;

/// An empty palace is listed only when idle at least this long (#9140 AC 3).
pub(crate) const EMPTY_PALACE_IDLE_DAYS: i64 = 30;

/// Tag the trusty-code turn recorder puts on every turn drawer.
pub(crate) const TURN_TAG: &str = "turn";

/// The prompts trusty-code's e2e tests and mock LLM send (#9139, #9140 AC 5).
///
/// Why: a fixture turn is told apart from a real one by its prompt, not its
/// tag; every real turn carries the `turn` tag too. Matching the prompt exactly
/// keeps a real turn whose text merely contains `say hi` out of the list.
/// What: the `task_description` and `run-task` prompts under
/// `crates/trusty-code/tests/` at the time of #9140. A new fixture prompt there
/// is not listed until it is added here.
/// Test: `fixture_turns_match_only_the_fixture_prompt_set`.
pub(crate) const FIXTURE_PROMPTS: &[&str] = &[
    "say hi",
    "say hi again",
    "find where auth lives",
    "fan out to two same-named engineers",
    "just chat",
    "just chat, no project",
    "add a doc comment to fn X",
    "recall what we know about the pkce oauth flow",
    "ship the feature",
];

/// Why a path is on the list.
// #9140: `Deserialize` + `Hash` so the apply can read the reviewed list back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReclaimClass {
    /// A redb 2.x store quarantined at open (`*.v2-incompatible`).
    IncompatibleFile,
    /// A `kg.redb.*.bak` backup left by a compaction or migration.
    KgBackup,
    /// A palace with 0 drawers, idle for [`EMPTY_PALACE_IDLE_DAYS`].
    EmptyPalace,
    /// A directory in the palace root with no `palace.json`.
    OrphanDir,
    /// `uds_addr` naming a socket that does not exist.
    StaleUdsAddr,
    /// A trusty-code e2e fixture turn drawer.
    FixtureDrawer,
}

/// One candidate for reclamation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReclaimItem {
    pub class: ReclaimClass,
    pub palace: Option<String>,
    pub path: PathBuf,
    /// File or directory bytes; for a drawer, its content length.
    pub bytes: u64,
    /// Unix seconds; an apply step must skip a path whose mtime changed.
    pub mtime_unix: Option<i64>,
    pub drawer_id: Option<String>,
    pub reason: String,
}

/// The whole dry run: candidates, plus palaces that could not be read.
#[derive(Debug, Default, Serialize)]
pub struct ReclaimReport {
    pub root: PathBuf,
    pub generated_at: String,
    pub dry_run: bool,
    pub items: Vec<ReclaimItem>,
    /// `(palace, error)` for a palace whose drawers could not be counted. Such
    /// a palace is never listed as empty.
    pub unreadable: Vec<(String, String)>,
}

/// Classify every reclaimable path under the palace root `root`.
///
/// Why (#9140): see the module doc. `now_unix` is a parameter so a test can
/// place a palace either side of the idle cutoff.
/// What: one pass over `root`. A directory with `palace.json` contributes its
/// incompatible files, KG backups, fixture turns, and itself when empty and
/// idle; one without contributes itself as an orphan; `uds_addr` is listed
/// when its socket is gone; root-level incompatible files are listed. Hidden
/// entries are skipped. Opens no store; writes nothing under `root`.
/// Test: `reclaim_scan_lists_every_class_and_writes_nothing`,
/// `reclaim_scan_keeps_recent_and_nonempty_palaces`.
pub fn scan(root: &Path, now_unix: i64) -> Result<ReclaimReport> {
    let mut report = ReclaimReport {
        root: root.to_path_buf(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        dry_run: true,
        ..Default::default()
    };
    let mut entries: Vec<_> = std::fs::read_dir(root)
        .with_context(|| format!("list palace root {}", root.display()))?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    let scratch = std::env::temp_dir();
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if name.starts_with('.') {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if path.join("palace.json").is_file() {
                scan_palace(&mut report, &name, &path, now_unix, &scratch);
            } else {
                report.items.push(item(
                    ReclaimClass::OrphanDir,
                    None,
                    &path,
                    "directory in the palace root has no palace.json".into(),
                ));
            }
        } else if name.contains(INCOMPATIBLE_SUFFIX) {
            report.items.push(item(
                ReclaimClass::IncompatibleFile,
                None,
                &path,
                "redb 2.x store quarantined at open; this binary cannot read it".into(),
            ));
        } else if name == "uds_addr" {
            if let Some(reason) = stale_uds_addr(&path) {
                report
                    .items
                    .push(item(ReclaimClass::StaleUdsAddr, None, &path, reason));
            }
        }
    }
    Ok(report)
}

/// Add one palace's candidates to `report`.
///
/// Why: a palace contributes up to four classes, and each must be decided
/// without opening its live store.
/// What: lists `*.v2-incompatible` and `kg.redb.*.bak` files, then reads the
/// drawer table from a private copy to count drawers and match fixture turns.
/// A copy that cannot be read is an `unreadable` row, never an empty palace.
fn scan_palace(report: &mut ReclaimReport, name: &str, dir: &Path, now: i64, scratch: &Path) {
    let palace = Some(name.to_string());
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    files.sort();
    for f in &files {
        let fname = f.file_name().map(|s| s.to_string_lossy().into_owned());
        let Some(fname) = fname else { continue };
        if fname.contains(INCOMPATIBLE_SUFFIX) {
            report.items.push(item(
                ReclaimClass::IncompatibleFile,
                palace.clone(),
                f,
                "redb 2.x store quarantined at open; this binary cannot read it".into(),
            ));
        } else if fname.starts_with("kg.redb.") && fname.ends_with(".bak") {
            report.items.push(item(
                ReclaimClass::KgBackup,
                palace.clone(),
                f,
                "KG backup left by a compaction or migration".into(),
            ));
        }
    }
    let drawers = match with_store_copy(dir, scratch, |s| s.load_drawers()) {
        Ok(d) => d.unwrap_or_default(),
        Err(e) => {
            report.unreadable.push((name.to_string(), format!("{e:#}")));
            return;
        }
    };
    for d in drawers.iter().filter(|d| is_fixture_turn(d)) {
        report.items.push(ReclaimItem {
            class: ReclaimClass::FixtureDrawer,
            palace: palace.clone(),
            path: dir.join("kg.redb"),
            bytes: d.content().len() as u64,
            mtime_unix: Some(d.created_at.timestamp()),
            drawer_id: Some(d.id.to_string()),
            reason: format!("trusty-code e2e fixture turn: {:?}", prompt_of(d)),
        });
    }
    if !drawers.is_empty() {
        return;
    }
    if let Some(reason) = idle_reason(dir, now) {
        report
            .items
            .push(item(ReclaimClass::EmptyPalace, palace, dir, reason));
    }
}

/// `Some(reason)` when an empty palace at `dir` has been idle long enough.
///
/// Why (#9140 AC 3): an empty palace used in the last 30 days, or created in
/// them, is someone's live namespace and is never listed.
/// What: the newest of the `last_used` stamp and `palace.json`'s mtime must be
/// at least [`EMPTY_PALACE_IDLE_DAYS`] before `now`.
fn idle_reason(dir: &Path, now: i64) -> Option<String> {
    let last_used = crate::palace_last_used::read(dir).map(|s| s as i64);
    let created = mtime_unix(&dir.join("palace.json"));
    let newest = last_used.into_iter().chain(created).max()?;
    let idle_days = (now - newest) / 86_400;
    (idle_days >= EMPTY_PALACE_IDLE_DAYS).then(|| {
        format!(
            "0 drawers; last used {} ({idle_days} days ago)",
            if last_used.is_some() {
                "per last_used"
            } else {
                "never; palace.json age"
            }
        )
    })
}

/// True when `d` is a turn drawer whose prompt is in [`FIXTURE_PROMPTS`].
///
/// Why (#9140 AC 5): tag AND prompt, so a real turn is never listed.
/// What: requires the [`TURN_TAG`] and a `User: <prompt>\n\nAssistant:` body
/// (the trusty-code `memory_sink` format) whose prompt matches exactly.
/// Test: `fixture_turns_match_only_the_fixture_prompt_set`.
pub(crate) fn is_fixture_turn(d: &Drawer) -> bool {
    d.tags.iter().any(|t| t == TURN_TAG)
        && prompt_of(d).is_some_and(|p| FIXTURE_PROMPTS.contains(&p))
}

/// The prompt of a `User: <prompt>\n\nAssistant: …` turn body, if it has one.
fn prompt_of(d: &Drawer) -> Option<&str> {
    let body = d.content().strip_prefix("User: ")?;
    body.split_once("\n\nAssistant:").map(|(p, _)| p)
}

/// `Some(reason)` when the `uds_addr` file names a socket that is gone.
fn stale_uds_addr(path: &Path) -> Option<String> {
    let target = std::fs::read_to_string(path).ok()?;
    let target = target.trim();
    (!target.is_empty() && !Path::new(target).exists())
        .then(|| format!("names socket {target}, which does not exist"))
}

/// Build an item for a file or directory, measuring its size and mtime.
fn item(class: ReclaimClass, palace: Option<String>, path: &Path, reason: String) -> ReclaimItem {
    ReclaimItem {
        class,
        palace,
        path: path.to_path_buf(),
        bytes: tree_bytes(path),
        mtime_unix: mtime_unix(path),
        drawer_id: None,
        reason,
    }
}

/// Bytes under `path`: the file's length, or a directory's recursive total.
/// Symlinks are counted as links, never followed.
pub(crate) fn tree_bytes(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| tree_bytes(&e.path()))
                .sum()
        })
        .unwrap_or(0)
}

/// A path's own mtime in Unix seconds; a symlink is not followed.
pub(crate) fn mtime_unix(path: &Path) -> Option<i64> {
    let modified = std::fs::symlink_metadata(path).ok()?.modified().ok()?;
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    i64::try_from(secs).ok()
}

impl ReclaimReport {
    /// Bytes the listed paths occupy, counting a path inside a listed
    /// directory once. Fixture drawers live inside `kg.redb` and are excluded.
    pub fn unique_bytes(&self) -> u64 {
        let dirs: Vec<&Path> = self
            .items
            .iter()
            .filter(|i| matches!(i.class, ReclaimClass::EmptyPalace | ReclaimClass::OrphanDir))
            .map(|i| i.path.as_path())
            .collect();
        self.items
            .iter()
            .filter(|i| i.class != ReclaimClass::FixtureDrawer)
            .filter(|i| !dirs.iter().any(|d| i.path != *d && i.path.starts_with(d)))
            .map(|i| i.bytes)
            .sum()
    }

    /// The plain-text report: a summary per class, then one line per item.
    ///
    /// Test: `reclaim_scan_lists_every_class_and_writes_nothing`.
    pub fn render_text(&self) -> String {
        use std::fmt::Write as _;
        let mut out = format!(
            "palace reclaim DRY RUN — nothing was deleted\nroot={}\ngenerated_at={}\n",
            self.root.display(),
            self.generated_at
        );
        for class in [
            ReclaimClass::IncompatibleFile,
            ReclaimClass::KgBackup,
            ReclaimClass::EmptyPalace,
            ReclaimClass::OrphanDir,
            ReclaimClass::StaleUdsAddr,
            ReclaimClass::FixtureDrawer,
        ] {
            let (n, b) = self
                .items
                .iter()
                .filter(|i| i.class == class)
                .fold((0usize, 0u64), |(n, b), i| (n + 1, b + i.bytes));
            let _ = writeln!(out, "summary class={class:?} count={n} bytes={b}");
        }
        let _ = writeln!(
            out,
            "summary total_items={} unique_bytes={} unreadable_palaces={}",
            self.items.len(),
            self.unique_bytes(),
            self.unreadable.len()
        );
        for (p, e) in &self.unreadable {
            let _ = writeln!(out, "unreadable palace={p} error={e}");
        }
        for i in &self.items {
            let _ = writeln!(
                out,
                "{:?}\t{}\t{}\t{}{}\t{}",
                i.class,
                i.bytes,
                i.palace.as_deref().unwrap_or("-"),
                i.path.display(),
                i.drawer_id
                    .as_deref()
                    .map(|d| format!("#{d}"))
                    .unwrap_or_default(),
                i.reason
            );
        }
        out
    }
}

/// `trusty-memory palace reclaim` — print the dry run for the live data root.
///
/// Why (#9140): the operator entry point for the dry run. `--apply` and
/// `--purge-trash` (ruling f0) are separate handlers in `palace_reclaim_apply`.
/// What: resolves the palace root, runs [`scan`], prints text or JSON.
/// Test: `reclaim_scan_lists_every_class_and_writes_nothing` covers `scan`.
pub fn handle_reclaim(json: bool) -> Result<()> {
    let data_dir = trusty_common::resolve_data_dir("trusty-memory")
        .context("resolve trusty-memory data dir")?;
    let root = crate::resolve_palace_registry_dir(data_dir);
    let now = i64::try_from(crate::palace_last_used::now_unix()).unwrap_or(i64::MAX);
    let report = scan(&root, now)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", report.render_text());
    }
    Ok(())
}

#[cfg(test)]
#[path = "palace_reclaim_tests.rs"]
pub(crate) mod tests;
