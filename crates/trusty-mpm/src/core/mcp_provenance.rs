//! Provenance ledger recording which `.mcp.json` files trusty-mpm wrote.
//!
//! Why: `prepare_session` writes `<workspace>/.mcp.json` through
//! [`session_launch::settings::inject_mcp_server`](crate::core::session_launch),
//! and Claude Code discovers that file by walking UP from a session's cwd. A
//! run whose workspace resolved to a directory ABOVE real projects therefore
//! leaves a file that keeps configuring every later session started anywhere
//! beneath it. Removing the write path does not clean up a file already
//! written, so the leftovers need their own detection and repair.
//!
//! That repair needs to answer one question first: **did tm write this file, or
//! did the operator?** Nothing on disk answered it. A `.mcp.json` full of
//! `trusty-*` servers is NOT evidence — an operator may have registered exactly
//! those entries by hand, and a file may hold both (the one that motivated this
//! module holds four `trusty-*` servers and four operator HTTP servers in the
//! same `mcpServers` map). Guessing from content and guessing wrong means
//! deleting hand-authored config.
//!
//! This module is the missing evidence, and it is deliberately a SIDE ledger in
//! tm's own config home rather than a marker inside the file:
//!
//! - A marker key inside `.mcp.json` would put tm-invented bytes into a file
//!   Claude Code parses (and that some repos track in git), for every project.
//! - A sidecar file next to `.mcp.json` would litter a second stray file into
//!   exactly the directories this work exists to clean up.
//! - A central ledger touches neither, and additionally records a CHECKSUM, so
//!   "tm wrote it" and "tm wrote it and nobody has edited it since" stay
//!   distinguishable — the same ownership rule
//!   [`skill_repair`](crate::core::skill_repair) applies to a FROZEN skill.
//!
//! It only attributes files written AFTER it ships. A file already on disk has
//! no record and classifies [`Provenance::Unattributed`], which the repair
//! REFUSES — see [`crate::core::stray_mcp`]. That is the intended outcome, not
//! a gap to paper over: an unattributable file is removed by the operator
//! naming it, never by tm inferring ownership it cannot prove.
//!
//! Test: `mcp_provenance_tests.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::agent_manifest::{ManifestError, atomic_write, checksum, with_ledger_lock};

/// Basename of the ledger inside the framework root (`~/.trusty-mpm`).
///
/// Why: the writer, the reader, and the lock helper must not spell this
/// differently — a reader looking at another name silently attributes nothing,
/// which downgrades every repair to a refusal without any error to notice.
/// Test: `ledger_path_is_under_the_framework_root`.
pub const PROVENANCE_LEDGER_FILE: &str = "mcp-json-provenance.json";

/// Current on-disk ledger schema version.
const LEDGER_VERSION: u32 = 1;

/// One recorded write of one `.mcp.json`.
///
/// Why: the path alone proves tm wrote the file at some point; the checksum is
/// what proves the bytes still are the ones tm wrote. Without it a repair
/// cannot tell its own output from a file the operator has since edited, and
/// would quarantine the operator's edit.
/// What: the sha256 of the exact bytes tm last wrote, plus an RFC 3339 stamp
/// for the operator reading the ledger.
/// Test: `record_then_classify_reports_unmodified`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpWriteRecord {
    /// sha256 hex digest of the content tm wrote.
    pub checksum: String,
    /// RFC 3339 timestamp of that write.
    pub written_at: String,
}

/// The ledger document: every `.mcp.json` path tm has written.
///
/// Why: a single machine-global document (rather than one per directory) is
/// what lets the repair sweep several unrelated directories — a workspace's
/// ancestors and the temp roots — from one read.
/// What: `written` is keyed by the ABSOLUTE path, so two projects that both
/// have a `.mcp.json` never collide.
/// Test: `ledger_round_trips`, `record_is_idempotent_for_identical_content`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpProvenanceLedger {
    /// Schema version of this document.
    pub version: u32,
    /// Absolute `.mcp.json` path → the write tm last performed there.
    pub written: BTreeMap<String, McpWriteRecord>,
}

impl Default for McpProvenanceLedger {
    fn default() -> Self {
        Self {
            version: LEDGER_VERSION,
            written: BTreeMap::new(),
        }
    }
}

/// The result of reading the ledger.
///
/// Why: "the ledger says tm never wrote this" and "the ledger could not be
/// read" are different facts, and collapsing them is how a repair deletes
/// something on bad evidence. A missing ledger is the normal first-run state;
/// an unreadable one means attribution is UNAVAILABLE, and every decision that
/// depends on it must refuse rather than fall through to "not ours" (which
/// reads identically) or "ours" (which would be catastrophic). Same shape, and
/// the same reason, as [`ManifestLoad`](crate::core::agent_manifest).
/// What: three states; [`LedgerLoad::Unreadable`] carries the reason so the
/// refusal can name it.
/// Test: `load_reports_missing_when_absent`,
/// `load_reports_unreadable_on_malformed_json`.
#[derive(Debug, Clone)]
pub enum LedgerLoad {
    /// The ledger was read.
    Loaded(McpProvenanceLedger),
    /// No ledger exists yet — tm has recorded no writes on this machine.
    Missing,
    /// The ledger exists but could not be parsed; the string says why.
    Unreadable(String),
}

/// What tm can prove about one `.mcp.json` on disk.
///
/// Why: the repair's entire safety argument is this enum. Only
/// [`Provenance::TmWritten`] is tm's own residue; everything else belongs to
/// the operator or is unproven, and both are refused.
/// What: four states, ordered from "safe to act on" to "must not touch".
/// Test: `mcp_provenance_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    /// tm wrote this file and its bytes are unchanged since.
    TmWritten,
    /// tm wrote this file, but the bytes on disk differ from what tm wrote —
    /// somebody edited it afterwards, so its current content is theirs.
    TmWrittenThenEdited,
    /// No ledger record. tm cannot prove it wrote this file.
    Unattributed,
    /// Attribution is unavailable (unreadable ledger, or unreadable file); the
    /// string says why. Never treated as either of the two above.
    Unknown(String),
}

/// Where the ledger lives under a framework root.
///
/// Why: `~/.trusty-mpm` is tm's own state directory, so recording tm's writes
/// there adds no file to any directory the operator manages — which is the
/// whole point, given this module exists because tm wrote files where it
/// should not have.
/// What: `<framework_root>/mcp-json-provenance.json`.
/// Test: `ledger_path_is_under_the_framework_root`.
pub fn ledger_path(framework_root: &Path) -> PathBuf {
    framework_root.join(PROVENANCE_LEDGER_FILE)
}

/// The machine-global framework root the ledger lives under in production.
///
/// Why: the ledger must resolve to ONE path per machine. A workspace-rooted
/// `FrameworkPaths` (what `run_doctor` builds for a managed session) would give
/// each workspace its own ledger, so a file recorded by one session would be
/// unattributable to the next — the exact failure this ledger exists to
/// prevent. `FrameworkPaths::default` is documented as the single global path
/// regardless of workspace, which is why it is the one used here and at the
/// write site.
/// What: `~/.trusty-mpm`. Tests pass their own root instead of calling this.
/// Test: exercised end-to-end by the daemon's `stray_mcp_json` check.
pub fn default_framework_root() -> PathBuf {
    crate::core::paths::FrameworkPaths::default().root
}

/// Sidecar lock serialising load-modify-save of the ledger.
///
/// Why: `atomic_write` publishes by `rename`, which discards any lock held on
/// the replaced inode — so the lock must be a stable sidecar, exactly as
/// [`agent_manifest::manifest_lock_path`](crate::core::agent_manifest) settled
/// on for the same reason.
/// What: `<framework_root>/mcp-json-provenance.json.lock`.
/// Test: `record_serialises_concurrent_writers`.
fn lock_path(framework_root: &Path) -> PathBuf {
    framework_root.join(format!("{PROVENANCE_LEDGER_FILE}.lock"))
}

/// Read the ledger.
///
/// Why: see [`LedgerLoad`] — the three outcomes are kept distinct because the
/// repair's refusal reasons differ between them.
/// What: [`LedgerLoad::Missing`] when the file is absent,
/// [`LedgerLoad::Unreadable`] when it exists but does not parse (it is NEVER
/// silently reset to empty — that would erase tm's own attribution for every
/// file it has written and turn every subsequent repair into a refusal with no
/// signal), otherwise [`LedgerLoad::Loaded`].
/// Test: `load_reports_missing_when_absent`,
/// `load_reports_unreadable_on_malformed_json`, `ledger_round_trips`.
pub fn load(framework_root: &Path) -> LedgerLoad {
    let path = ledger_path(framework_root);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LedgerLoad::Missing,
        Err(e) => return LedgerLoad::Unreadable(e.to_string()),
    };
    match serde_json::from_str::<McpProvenanceLedger>(&text) {
        Ok(ledger) => LedgerLoad::Loaded(ledger),
        Err(e) => LedgerLoad::Unreadable(format!("{} is malformed: {e}", path.display())),
    }
}

/// Record a write against the machine-global ledger, logging on failure.
///
/// Why: the write path must not fail a session launch over tm's own
/// bookkeeping, and the "log it and carry on" policy belongs with the ledger
/// rather than being restated (and eventually varied) at each call site. The
/// cost of a lost record is a later REFUSAL to repair, never a wrong deletion,
/// which is what makes swallowing the error acceptable here.
/// What: [`record_write`] against [`default_framework_root`]; a failure becomes
/// a `warn!` naming the path and the consequence.
/// Test: [`record_write`]'s tests cover the ledger behaviour; this wrapper is
/// the root lookup and the log line.
pub fn record_write_best_effort(path: &Path, content: &str) {
    if let Err(err) = record_write(&default_framework_root(), path, content) {
        tracing::warn!(
            path = %path.display(),
            %err,
            "could not record .mcp.json provenance; if this file later turns up above a \
             workspace, `tm doctor --fix` will refuse to quarantine it and the operator \
             will have to name it explicitly"
        );
    }
}

/// Record that tm just wrote `content` to the `.mcp.json` at `path`.
///
/// Why: this is the only place attribution is created. It is called from the
/// single project-scope `.mcp.json` write path so that a future write cannot
/// land unrecorded — an unrecorded write is indistinguishable from the
/// operator's own file forever after.
/// What: under the cross-process ledger lock, loads, inserts (or replaces) the
/// entry for `path`'s absolute form, and atomically republishes the document.
/// An UNREADABLE ledger is left untouched and an error returned rather than
/// overwritten with a fresh one — clobbering it would drop every prior
/// attribution.
/// Test: `record_then_classify_reports_unmodified`,
/// `record_is_idempotent_for_identical_content`,
/// `record_refuses_to_clobber_an_unreadable_ledger`,
/// `record_serialises_concurrent_writers`.
pub fn record_write(
    framework_root: &Path,
    path: &Path,
    content: &str,
) -> Result<(), ManifestError> {
    std::fs::create_dir_all(framework_root).map_err(ManifestError::Io)?;
    let key = absolute_key(path);
    let record = McpWriteRecord {
        checksum: checksum(content),
        written_at: now_rfc3339(),
    };
    with_ledger_lock(&lock_path(framework_root), || {
        let mut ledger = match load(framework_root) {
            LedgerLoad::Loaded(ledger) => ledger,
            LedgerLoad::Missing => McpProvenanceLedger::default(),
            LedgerLoad::Unreadable(why) => {
                return Err(ManifestError::Io(std::io::Error::other(format!(
                    "refusing to overwrite the MCP provenance ledger: {why}"
                ))));
            }
        };
        ledger.version = LEDGER_VERSION;
        ledger.written.insert(key, record);
        prune_vanished(&mut ledger);
        let text = serde_json::to_string_pretty(&ledger)
            .map(|mut s| {
                s.push('\n');
                s
            })
            .map_err(|e| ManifestError::Io(std::io::Error::other(e.to_string())))?;
        atomic_write(&ledger_path(framework_root), &text)
    })
}

/// Above [`PRUNE_THRESHOLD`], drop records whose file no longer exists.
///
/// Why (#5371 critic MEDIUM): the ledger gained one entry per distinct
/// `.mcp.json` path and never shed one. Each entry is roughly 200 bytes, so
/// size alone is not the problem — but this repo creates and destroys a
/// worktree per PR, each with its own `.mcp.json`, so the entry count tracks
/// worktrees-ever-created rather than workspaces-that-exist, and it only ever
/// climbs. A stale entry is harmless to correctness ([`classify`] reads the
/// file and returns [`Provenance::Unknown`] when it is gone), so this is
/// housekeeping, not a safety fix.
///
/// It runs only ABOVE a threshold because the sweep costs one `stat` per
/// entry and this is called from the session-launch write path: an
/// unconditional sweep would put thousands of `stat` calls on every launch to
/// reclaim bytes nobody is short of.
/// What: when the map exceeds [`PRUNE_THRESHOLD`], drops entries whose path is
/// confirmed gone and retains everything else — including an entry whose path
/// could not be probed, since dropping an uncertain claim risks misattributing
/// a later `.mcp.json` at the same path (#5551). The entry just inserted
/// always survives — its file was written moments earlier.
/// Test: `record_prunes_vanished_entries_above_the_threshold`,
/// `record_keeps_every_entry_below_the_threshold`.
fn prune_vanished(ledger: &mut McpProvenanceLedger) {
    if ledger.written.len() <= PRUNE_THRESHOLD {
        return;
    }
    ledger
        .written
        .retain(|path, _| Path::new(path).try_exists().unwrap_or(true));
}

/// Entry count above which [`prune_vanished`] sweeps.
///
/// Why: high enough that an ordinary machine never pays the sweep, low enough
/// that the ledger cannot grow without bound. At ~200 bytes per entry this caps
/// the steady-state document near 100 KB plus however many live workspaces
/// exist.
/// Test: `record_prunes_vanished_entries_above_the_threshold`.
const PRUNE_THRESHOLD: usize = 512;

/// Drop `path`'s record after the file it describes is gone.
///
/// Why: a ledger entry is a claim about a file that exists. Leaving the claim
/// behind after a quarantine means a LATER `.mcp.json` written at the same path
/// by the operator would match a stale tm record on path — and would be
/// attributed to tm on the strength of that alone if the checksum ever
/// coincided. Releasing the claim keeps the ledger a statement about the
/// present, mirroring `skill_retire`'s "a ledger entry is a claim" rule.
/// What: under the lock, removes the key and republishes. A missing key, a
/// missing ledger, or an unreadable one is a no-op — this runs AFTER the
/// filesystem change and must never turn a completed repair into an error.
/// Test: `forget_releases_the_claim`, `forget_is_a_noop_without_a_ledger`.
pub fn forget(framework_root: &Path, path: &Path) {
    let key = absolute_key(path);
    let _: Result<(), ManifestError> = with_ledger_lock(&lock_path(framework_root), || {
        let LedgerLoad::Loaded(mut ledger) = load(framework_root) else {
            return Ok(());
        };
        if ledger.written.remove(&key).is_none() {
            return Ok(());
        }
        let text = serde_json::to_string_pretty(&ledger)
            .map(|mut s| {
                s.push('\n');
                s
            })
            .map_err(|e| ManifestError::Io(std::io::Error::other(e.to_string())))?;
        atomic_write(&ledger_path(framework_root), &text)
    });
}

/// Classify what tm can prove about the `.mcp.json` at `path`.
///
/// Why: every quarantine decision routes through this one function, so the
/// rules cannot drift between the doctor check that REPORTS and the repair that
/// ACTS — a check saying "tm wrote this" while the repair disagrees is the
/// failure mode that makes an operator distrust both.
/// What: reads the file and compares its sha256 against the ledger record for
/// its absolute path. An unreadable ledger or an unreadable file yields
/// [`Provenance::Unknown`] (never a guess in either direction); no record
/// yields [`Provenance::Unattributed`]; a record whose checksum differs yields
/// [`Provenance::TmWrittenThenEdited`].
/// Test: `record_then_classify_reports_unmodified`,
/// `classify_reports_edited_when_bytes_changed`,
/// `classify_reports_unattributed_without_a_record`,
/// `classify_reports_unknown_when_the_ledger_is_unreadable`,
/// `classify_reports_unknown_when_the_file_is_unreadable`.
pub fn classify(load: &LedgerLoad, path: &Path) -> Provenance {
    let ledger = match load {
        LedgerLoad::Loaded(ledger) => ledger,
        LedgerLoad::Missing => return Provenance::Unattributed,
        LedgerLoad::Unreadable(why) => {
            return Provenance::Unknown(format!("the MCP provenance ledger is unreadable ({why})"));
        }
    };
    let Some(record) = ledger.written.get(&absolute_key(path)) else {
        return Provenance::Unattributed;
    };
    match std::fs::read_to_string(path) {
        Ok(content) if checksum(&content) == record.checksum => Provenance::TmWritten,
        Ok(_) => Provenance::TmWrittenThenEdited,
        Err(e) => Provenance::Unknown(format!("cannot read the file to verify its checksum: {e}")),
    }
}

/// The ledger key for a path: absolute, with its PARENT directory canonical.
///
/// Why: keys must be comparable across a write (a session launch, cwd unknown)
/// and a later read (`tm doctor`, different cwd), so a relative path can never
/// be a key — and neither can two spellings of one directory. Canonicalizing
/// the parent is also what lets [`crate::core::stray_mcp`] dedupe `/tmp` and
/// `/private/tmp` into a single finding instead of reporting one file twice.
/// What: `path` made absolute, then its parent resolved through
/// [`std::fs::canonicalize`] and the basename rejoined. The FILE is never
/// canonicalized: that would fail once it is gone (the [`forget`] case) and
/// would follow a symlink to somewhere else entirely.
/// Test: `absolute_key_is_stable_for_a_relative_path`,
/// `absolute_key_collapses_symlinked_parents`.
pub fn absolute_key(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    // Canonicalize the PARENT, never the file. The parent outlives the file
    // (it is still there after a quarantine renames it away, which is what
    // makes `forget` work), and it is where the aliasing actually lives: on
    // macOS `/tmp` is a symlink to `/private/tmp`, so a write recorded under
    // one spelling and a scan arriving via the other would be two keys for one
    // file — attribution lost, and the repair silently downgraded to a refusal.
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => match std::fs::canonicalize(parent) {
            Ok(real) => real.join(name).display().to_string(),
            // A parent that cannot be resolved (absent, unreadable) keeps the
            // lexical form: a stable key beats no key.
            Err(_) => absolute.display().to_string(),
        },
        _ => absolute.display().to_string(),
    }
}

/// Current time as an RFC 3339 string.
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
#[path = "mcp_provenance_tests.rs"]
mod tests;
