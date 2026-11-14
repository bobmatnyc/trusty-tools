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
//! the `.gitignore` diff adds only ledgered lines. #11221: the compiled-prompt
//! writer adds its own file through [`record_compiled_prompt`], and
//! [`ProvisioningLedger::vouches_for`] answers for one path, for the
//! `.trusty-mpm/` dirt pass and the gitignored-output check. A missing,
//! unreadable or corrupt ledger ([`load`] answers `None`) excuses nothing.
//! Test: `decommission_owned_ledger_tests`, `provisioned_dirt_tests`.

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
    /// #11221: repo-relative `.trusty-mpm/` path to the sha256 (hex) of the
    /// bytes tm's own writer put there ([`record_compiled_prompt`]). Absent in
    /// a pre-#11221 ledger, which then vouches for no `.trusty-mpm/` file.
    #[serde(default)]
    pub harness_files: BTreeMap<String, String>,
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

/// The sha256 (hex) of `path`'s bytes, or `None` when it cannot be read or is
/// not a regular file.
///
/// Why: `--force` deletes what this vouches for, and a FIFO named `CLAUDE.md`
/// blocked the read forever inside decommission's blocking check (#8540).
/// What: opened with `O_NOFOLLOW | O_NONBLOCK`, so neither a symlink nor a
/// FIFO can stall the open; the handle must then be a regular file, and its
/// bytes are streamed into the hash.
/// Test: `provisioning_entry_refuses_a_claude_md_that_is_not_a_regular_file`.
fn file_sha256(path: &Path) -> Option<String> {
    use sha2::{Digest as _, Sha256};
    use std::os::unix::fs::OpenOptionsExt as _;
    // #8540: fail closed on a symlink, FIFO, socket or device.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).ok()?;
    Some(format!("{:x}", hasher.finalize()))
}

/// `path`'s bytes when it is a regular file and not a symlink, read without
/// ever blocking on the open.
///
/// Why: `std::fs::read` on a FIFO blocks until a writer appears, which hung a
/// launch's [`snapshot`] and decommission's `?? .gitignore` check (#8540).
/// What: opened with `O_NOFOLLOW | O_NONBLOCK`, as [`file_sha256`] is, so a
/// symlink fails the open (`ELOOP`) and a FIFO's open returns at once; the
/// handle must then be a regular file, or the answer is an `InvalidInput`
/// error. Every caller treats any error as "not tm's".
/// Test: `a_fifo_claude_md_or_gitignore_does_not_block_snapshot`,
/// `a_fifo_untracked_gitignore_is_not_excused_and_does_not_block`,
/// `a_symlinked_untracked_gitignore_is_not_excused`.
pub(super) fn read_regular_file(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    // #8540: check the type on the open handle, never read a FIFO or device,
    // and never follow a symlink to bytes that are not the tree's own.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("`{}` is not a regular file", path.display()),
        ));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// `path`'s [`PreState`]; only `NotFound` is [`PreState::Absent`], and a
/// symlink, FIFO or other non-regular file is [`PreState::Unreadable`] (#8540).
fn pre_state(path: &Path) -> PreState {
    match read_regular_file(path) {
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

/// `.gitignore`'s lines under `ws`; empty when absent, unreadable, not UTF-8,
/// a symlink or not a regular file.
fn gitignore_lines(ws: &Path) -> Vec<String> {
    // #8540: a FIFO `.gitignore` blocked `std::fs::read_to_string` forever.
    read_regular_file(&ws.join(".gitignore"))
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
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
        // #11221: a relaunch keeps vouching only for unedited harness writes.
        harness_files: still_matching(ws, prior.harness_files),
    };
    write_atomic(&path, &serde_json::to_vec_pretty(&ledger)?)?;
    Ok(true)
}

/// The entries of `recorded` whose file under `ws` still hashes to the
/// recorded value; an edited or deleted file drops out (#11221).
fn still_matching(ws: &Path, recorded: BTreeMap<String, String>) -> BTreeMap<String, String> {
    recorded
        .into_iter()
        .filter(|(rel, want)| file_sha256(&ws.join(rel)).as_ref() == Some(want))
        .collect()
}

/// The harness root and repo-relative path of a compiled prompt at `dest`, or
/// `None` unless `dest` is
/// `<root>/.trusty-mpm/sessions/<scope>/INSTRUCTIONS-COMPILED.md`.
///
/// Why: the path is decided by `instruction_pipeline::compiled_prompt_path`;
/// matching its exact shape keeps a scope segment named `.trusty-mpm` from
/// being taken for the harness directory.
fn compiled_prompt_location(dest: &Path) -> Option<(&Path, String)> {
    use crate::core::harness_root::{HARNESS_DIR, SESSIONS_DIR};
    use crate::core::instruction_pipeline::COMPILED_PROMPT_FILE;
    let scope_dir = dest.parent()?;
    let sessions = scope_dir.parent()?;
    let harness = sessions.parent()?;
    let root = harness.parent()?;
    let named = |p: &Path, want: &str| p.file_name().is_some_and(|n| n == want);
    if !(named(dest, COMPILED_PROMPT_FILE) && named(sessions, SESSIONS_DIR))
        || !named(harness, HARNESS_DIR)
    {
        return None;
    }
    let scope = scope_dir.file_name()?.to_str()?;
    let rel = format!("{HARNESS_DIR}/{SESSIONS_DIR}/{scope}/{COMPILED_PROMPT_FILE}");
    Some((root, rel))
}

/// Record that tm just wrote `bytes` as the compiled prompt at `dest`
/// (#11221).
///
/// Why: a launch writes `.trusty-mpm/sessions/<id>/INSTRUCTIONS-COMPILED.md`
/// into its harness root on every spawn, resume and relaunch, after [`record`]
/// has run. Without a record of those bytes the dirt check counted the file as
/// work, and every spawn-provisioned workspace was refused.
/// What: adds `dest`'s repo-relative path and the sha256 of `bytes` to the
/// harness root's existing ledger, written atomically. `Ok(false)`, writing
/// nothing, when `dest` is not a compiled-prompt path, the root has no git
/// admin dir, or no ledger exists yet; that file is then never excused.
/// Two launches racing in one root can drop each other's entry, which also
/// only keeps a workspace.
/// Test: `a_spawn_provisioned_clone_passes_the_dirt_check_11221`,
/// `an_edited_compiled_prompt_still_keeps_the_clone_11221`.
pub(crate) fn record_compiled_prompt(dest: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    let Some((root, rel)) = compiled_prompt_location(dest) else {
        return Ok(false);
    };
    let Some(path) = ledger_path(root) else {
        return Ok(false);
    };
    let Some(mut ledger) = read_ledger(&path) else {
        return Ok(false);
    };
    ledger.harness_files.insert(rel, sha256_hex(bytes));
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

    /// Does the file at repo-relative `rel` in `ws` hold exactly the bytes tm
    /// wrote there (#11221)?
    ///
    /// Why: the `.trusty-mpm/` dirt pass and the gitignored-output check see
    /// tm's own writes as `!!` or `??` entries; only a byte match proves an
    /// entry is tm's and not an edit made after provisioning.
    /// What: a [`LEDGERED_FILES`] entry is matched against `files`, a
    /// `.trusty-mpm/` path against `harness_files`; the file must be a regular
    /// file, read without following a symlink. Any other path, a missing
    /// entry or an unreadable file is `false`.
    /// Test: `a_spawn_provisioned_clone_passes_the_dirt_check_11221`,
    /// `a_gitignored_claude_md_tm_wrote_passes_the_ignored_output_check_11221`,
    /// `an_edited_gitignored_claude_md_still_keeps_the_clone_11221`.
    pub(crate) fn vouches_for(&self, ws: &Path, rel: &str) -> bool {
        let recorded = if LEDGERED_FILES.contains(&rel) {
            self.files.get(rel)
        } else if rel.starts_with(".trusty-mpm/") {
            self.harness_files.get(rel)
        } else {
            None
        };
        recorded.is_some_and(|want| file_sha256(&ws.join(rel)).as_ref() == Some(want))
    }
}
