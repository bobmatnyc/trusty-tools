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
//! sha256 of each [`LEDGERED_FILES`] entry this launch wrote over bytes that
//! were tm's own (see [`record`]), and
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

/// A ledgered path before a launch writes (#8663 critic round 2).
#[derive(Debug, Clone, PartialEq, Eq)]
enum PreState {
    /// The file did not exist.
    Absent,
    /// The sha256 (hex) of its bytes.
    Hashed(String),
    /// It existed but could not be read, so whose bytes they were is unknown.
    Unreadable,
}

/// The state of the ledgered paths before a launch writes (#8663).
#[derive(Debug, Clone, Default)]
pub(crate) struct ProvisioningSnapshot {
    /// Each [`LEDGERED_FILES`] entry's state.
    files: BTreeMap<&'static str, PreState>,
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

/// The sha256 (hex) of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// The sha256 (hex) of `path`'s bytes, or `None` when it cannot be read.
fn file_sha256(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|bytes| sha256_hex(&bytes))
}

/// `path`'s [`PreState`]; only `NotFound` is [`PreState::Absent`].
fn pre_state(path: &Path) -> PreState {
    match std::fs::read(path) {
        Ok(bytes) => PreState::Hashed(sha256_hex(&bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => PreState::Absent,
        Err(_) => PreState::Unreadable,
    }
}

/// The sha256 (hex) of `rel`'s blob in `HEAD`, or `None` when `HEAD` holds
/// no such blob or git cannot answer.
fn head_blob_sha256(ws: &Path, rel: &str) -> Option<String> {
    let spec = format!("HEAD:{rel}");
    let out = super::worktree_safety::git_command(ws, &["cat-file", "blob", &spec])
        .output()
        .ok()?;
    out.status.success().then(|| sha256_hex(&out.stdout))
}

/// The ledgered file whose old bytes a launch copies into `rel`, if any: the
/// statusline writer backs `settings.json` up to `settings.json.bak`.
fn copied_from(rel: &str) -> Option<&'static str> {
    (rel == ".claude/settings.json.bak").then_some(".claude/settings.json")
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
            .map(|rel| (rel, pre_state(&ws.join(rel))))
            .collect(),
        gitignore: gitignore_lines(ws),
    }
}

/// Record what a launch wrote into `ws` since `before` (#8663).
///
/// Why: only bytes tm wrote may be excused later; an edit someone made
/// between launches must not be. tm rewrites `.claude/settings.json` in place
/// and backs the old bytes up to `settings.json.bak`, so "the hash changed"
/// alone would adopt that edit as tm's.
/// What: a file's current hash is recorded only when the state this launch
/// found there was tm's — absent, the previous ledger's hash, or the `HEAD`
/// blob; an unreadable pre-state never is. A changed file a launch copies
/// another into ([`copied_from`]) also needs that source's pre-state to be
/// tm's. A file left out is never excused. `.gitignore` lines present now and
/// absent from `before` are recorded, plus previously ledgered lines still in
/// the file. The ledger is written atomically into the git admin dir.
/// `Ok(false)` when `ws` has no resolvable admin dir (nothing is written);
/// write errors propagate for the caller to log.
/// Test: `ledger_excuses_only_provisioning_dirt_on_a_clone`,
/// `ledger_does_not_carry_an_edit_made_between_launches`,
/// `ledger_does_not_adopt_a_settings_edit_the_next_launch_rewrites`,
/// `ledger_adopts_a_relaunch_rewrite_of_tm_or_head_bytes`.
pub(crate) fn record(ws: &Path, before: &ProvisioningSnapshot) -> std::io::Result<bool> {
    let Some(path) = ledger_path(ws) else {
        return Ok(false);
    };
    let prior = read_ledger(&path).unwrap_or_default();
    // #8663 critic round 2: whose bytes did this launch find at `rel`?
    let was_tms = |rel: &str| match before.files.get(rel) {
        Some(PreState::Absent) => true,
        Some(PreState::Hashed(pre)) => {
            prior.files.get(rel) == Some(pre) || head_blob_sha256(ws, rel).as_ref() == Some(pre)
        }
        Some(PreState::Unreadable) | None => false,
    };
    let mut files = BTreeMap::new();
    for rel in LEDGERED_FILES {
        let Some(now) = file_sha256(&ws.join(rel)) else {
            continue;
        };
        let unchanged = before.files.get(rel) == Some(&PreState::Hashed(now.clone()));
        let source_ok = copied_from(rel).is_none_or(|src| unchanged || was_tms(src));
        if was_tms(rel) && source_ok {
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

/// Write `bytes` to `path` through a uniquely named sibling temp file and a
/// rename; the temp file is removed when the rename fails.
// #8663 critic round 2: a pid-keyed temp name collided between two launches in
// one daemon process.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("the ledger path has no parent directory"))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
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
