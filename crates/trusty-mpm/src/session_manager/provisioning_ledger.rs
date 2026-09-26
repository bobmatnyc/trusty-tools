//! What tm's own provisioning wrote into a workspace, recorded outside the
//! work tree (#8663).
//!
//! Why: every launch leaves ` M .gitignore`, `?? CLAUDE.md` and
//! `?? .claude/settings.json` behind, and the dirty-tree guard counts them as
//! work. Decommission of an SM-owned workspace refuses dirt, and the idle
//! reaper, MCP, RPC and the prune sweeps all decommission without `--force`,
//! so every freshly provisioned owned clone was kept forever. Excusing those
//! paths by name would also excuse an agent's edit to them.
//! What: [`snapshot`] before a launch's writes and [`record`] after them store
//! a JSON ledger in the git admin dir, beside the ownership marker: the
//! sha256 of each [`LEDGERED_FILES`] entry this launch wrote or changed, and
//! the `.gitignore` lines it added. [`ProvisioningLedger::excuses`] excuses a
//! `git status` entry only when the file still byte-matches the ledger, or when
//! the `.gitignore` diff adds only ledgered lines. A missing, unreadable or
//! corrupt ledger ([`load`] answers `None`) excuses nothing.
//! Test: `decommission_owned_ledger_tests`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::decommission_force::gitignore_diff_only_adds;
use super::worktree_ownership_location::{AdminLocation, admin_location};

/// The ledger's file name inside the git admin dir (#8663).
pub(crate) const LEDGER_NAME: &str = "trusty-mpm-provisioning-ledger.json";

/// The files a launch writes whose content the ledger pins (#8663).
const LEDGERED_FILES: [&str; 3] = [
    "CLAUDE.md",
    ".claude/settings.json",
    ".claude/settings.json.bak",
];

/// The on-disk ledger (#8663).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProvisioningLedger {
    /// Schema version; only `1` is read.
    pub version: u32,
    /// Repo-relative path to the sha256 (hex) of the bytes tm left there.
    pub files: BTreeMap<String, String>,
    /// The exact `.gitignore` lines tm added, blank ones included.
    pub gitignore_appended: Vec<String>,
}

/// The state of the ledgered paths before a launch writes (#8663).
#[derive(Debug, Clone, Default)]
pub(crate) struct ProvisioningSnapshot {
    /// Path to its sha256, or `None` when absent or unreadable.
    files: BTreeMap<&'static str, Option<String>>,
    /// `.gitignore`'s lines, or empty when absent or unreadable.
    gitignore: Vec<String>,
}

/// Where the ledger for the tree rooted at `ws` lives, or `None` when `ws`
/// has no resolvable git admin dir.
fn ledger_path(ws: &Path) -> Option<PathBuf> {
    match admin_location(ws) {
        AdminLocation::At(marker) => marker.parent().map(|dir| dir.join(LEDGER_NAME)),
        AdminLocation::NotGit | AdminLocation::Unresolvable(_) => None,
    }
}

/// The sha256 (hex) of `path`'s bytes, or `None` when it cannot be read.
fn file_sha256(path: &Path) -> Option<String> {
    use sha2::{Digest as _, Sha256};
    let bytes = std::fs::read(path).ok()?;
    Some(format!("{:x}", Sha256::digest(&bytes)))
}

/// `.gitignore`'s lines under `ws`; empty when absent or unreadable.
fn gitignore_lines(ws: &Path) -> Vec<String> {
    std::fs::read_to_string(ws.join(".gitignore"))
        .map(|body| body.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Capture the ledgered paths of `ws` before a launch writes (#8663).
/// Test: `ledger_excuses_only_provisioning_dirt_on_a_clone`.
pub(crate) fn snapshot(ws: &Path) -> ProvisioningSnapshot {
    ProvisioningSnapshot {
        files: LEDGERED_FILES
            .into_iter()
            .map(|rel| (rel, file_sha256(&ws.join(rel))))
            .collect(),
        gitignore: gitignore_lines(ws),
    }
}

/// Record what a launch wrote into `ws` since `before` (#8663).
///
/// Why: only bytes tm wrote may be excused later; an edit someone made
/// between launches must not be.
/// What: a file whose hash changed since `before` is recorded with its new
/// hash; an unchanged one keeps its entry only while the previous ledger
/// already pinned exactly those bytes. `.gitignore` lines present now and
/// absent from `before` are recorded, plus previously ledgered lines still in
/// the file. The ledger is written atomically into the git admin dir.
/// `Ok(false)` when `ws` has no resolvable admin dir (nothing is written);
/// write errors propagate for the caller to log.
/// Test: `ledger_excuses_only_provisioning_dirt_on_a_clone`,
/// `ledger_does_not_carry_an_edit_made_between_launches`.
pub(crate) fn record(ws: &Path, before: &ProvisioningSnapshot) -> std::io::Result<bool> {
    let Some(path) = ledger_path(ws) else {
        return Ok(false);
    };
    let prior = read_ledger(&path).unwrap_or_default();
    let mut files = BTreeMap::new();
    for rel in LEDGERED_FILES {
        let Some(now) = file_sha256(&ws.join(rel)) else {
            continue;
        };
        let wrote = before.files.get(rel).and_then(Option::as_ref) != Some(&now);
        if wrote || prior.files.get(rel) == Some(&now) {
            files.insert(rel.to_string(), now);
        }
    }
    let after = gitignore_lines(ws);
    let mut unmatched = before.gitignore.clone();
    let mut appended: Vec<String> = Vec::new();
    for line in &after {
        // #8663: a multiset difference — a line tm repeats is still tm's.
        if let Some(pos) = unmatched.iter().position(|l| l == line) {
            unmatched.remove(pos);
        } else if !appended.contains(line) {
            appended.push(line.clone());
        }
    }
    for line in prior.gitignore_appended {
        if after.contains(&line) && !appended.contains(&line) {
            appended.push(line);
        }
    }
    let ledger = ProvisioningLedger {
        version: 1,
        files,
        gitignore_appended: appended,
    };
    write_atomic(&path, &serde_json::to_vec_pretty(&ledger)?)?;
    Ok(true)
}

/// Write `bytes` to `path` through a sibling temp file and a rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// The ledger at `path`, or `None` when missing, unreadable, corrupt or of an
/// unknown version.
fn read_ledger(path: &Path) -> Option<ProvisioningLedger> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<ProvisioningLedger>(&bytes)
        .ok()
        .filter(|l| l.version == 1)
}

/// The ledger recorded for `ws`, or `None` — the fail-closed state, in which
/// nothing is excused (#8663).
/// Test: `ledger_missing_keeps_a_clone_with_provisioning_dirt`.
pub(crate) fn load(ws: &Path) -> Option<ProvisioningLedger> {
    read_ledger(&ledger_path(ws)?)
}

impl ProvisioningLedger {
    /// Does this `git status --porcelain` line in `ws` show only what tm
    /// wrote (#8663)?
    ///
    /// What: ` M` or `??` on a [`LEDGERED_FILES`] entry whose current bytes
    /// hash to the ledgered value; ` M .gitignore` whose unstaged diff adds
    /// only ledgered lines and removes none; `?? .gitignore` whose every line
    /// is ledgered. Anything else, including a staged change, is `false`.
    /// Test: `ledger_excuses_only_provisioning_dirt_on_a_clone`,
    /// `ledger_keeps_a_clone_with_an_edited_claude_md`,
    /// `ledger_keeps_a_clone_whose_gitignore_gained_a_user_line`.
    pub(crate) fn excuses(&self, ws: &Path, line: &str) -> bool {
        let (Some(status), Some(path)) = (line.get(..3), line.get(3..)) else {
            return false;
        };
        let path = path.trim();
        let ledgered = |l: &str| self.gitignore_appended.iter().any(|a| a == l);
        match (status, path) {
            (" M ", ".gitignore") => gitignore_diff_only_adds(ws, &ledgered),
            ("?? ", ".gitignore") => {
                let lines = gitignore_lines(ws);
                !lines.is_empty() && lines.iter().all(|l| ledgered(l))
            }
            (" M " | "?? ", rel) if LEDGERED_FILES.contains(&rel) => self
                .files
                .get(rel)
                .is_some_and(|want| file_sha256(&ws.join(rel)).as_ref() == Some(want)),
            _ => false,
        }
    }
}
