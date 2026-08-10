//! Detection and quarantine of stray `.mcp.json` files above a workspace.
//!
//! Why: Claude Code discovers `.mcp.json` by walking UP from a session's cwd,
//! so a file at `/tmp/.mcp.json` configures the MCP servers of every session
//! whose cwd is anywhere beneath `/tmp` — including agent scratchpad
//! directories, which is how the one that motivated this module was found. tm
//! writes `<workspace>/.mcp.json` on every managed launch, so a run whose
//! workspace resolved above real projects leaves such a file behind. Stopping
//! future writes is a separate decision (ADR-0042) and would not help: a file
//! already on disk keeps being discovered no matter what the write path later
//! does.
//!
//! The whole design is bounded by two constraints, and both push toward
//! refusing:
//!
//! 1. **A `.mcp.json` may be the operator's.** The observed file declares four
//!    `trusty-*` servers AND four operator HTTP servers in one map. "It contains
//!    trusty-* servers" is not provenance — an operator can register exactly
//!    those by hand. Proof comes from [`crate::core::mcp_provenance`]'s ledger
//!    and from nowhere else.
//! 2. **Nothing here deletes.** A quarantine RENAMES `.mcp.json` to
//!    `.mcp.json.quarantined-<epoch>` beside itself. Claude Code looks for the
//!    exact basename, so the rename is what actually stops the discovery, and
//!    every byte survives for the operator to inspect or restore. This keeps
//!    [`crate::core::doctor_repair`]'s "nothing is deleted" rule intact rather
//!    than carving an exception into it.
//!
//! **Search bound** ([`scan_dirs`]): the strict ancestors of the workspace,
//! walking up and stopping at the operator's home directory (inclusive), the
//! filesystem root (exclusive), or [`MAX_ANCESTOR_DEPTH`] levels — whichever
//! comes first — plus the two well-known temp roots. Two things this
//! deliberately is NOT: it is not a walk to `/` (above home is system config
//! and none of tm's business), and it is not a recursive descent (a scan that
//! finds `.mcp.json` files INSIDE unrelated projects would report other tools'
//! legitimate config as strays). The temp roots are listed explicitly because
//! they are not ancestors of any workspace — they are ancestors of the agent
//! scratchpad cwds, which is exactly why a file there is so far-reaching. The
//! whole set is a fixed handful of `stat` calls, cheap enough to run on every
//! `tm doctor`.
//!
//! Test: `stray_mcp_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};
use crate::core::mcp_config::MCP_JSON;
use crate::core::mcp_provenance::{self, LedgerLoad, Provenance};

/// The `tm doctor` check name, and the `check` field of every repair step.
pub const CHECK_NAME: &str = "stray_mcp_json";

/// Hard ceiling on how far up from a workspace the scan walks.
///
/// Why: the home directory is the normal ceiling, but a workspace outside home
/// (`/opt/…`, an external volume) has no such boundary and would otherwise walk
/// to the filesystem root. A fixed cap keeps the cost constant and keeps the
/// scan away from system directories in that case too.
/// Test: `scan_dirs_stops_at_the_depth_cap_outside_home`.
pub const MAX_ANCESTOR_DEPTH: usize = 8;

/// One `.mcp.json` found above a workspace.
///
/// Why: the doctor check reports these and the repair acts on them, from one
/// scan — so the provenance verdict and the server list are computed once and
/// cannot disagree between the two surfaces.
/// What: the file's path, what tm can prove about who wrote it, and the
/// `mcpServers` keys it declares (for the report; never for attribution).
/// Test: `stray_mcp_tests.rs`.
#[derive(Debug, Clone)]
pub struct StrayMcpFile {
    /// Absolute path to the stray `.mcp.json`.
    pub path: PathBuf,
    /// What tm can prove about its origin.
    pub provenance: Provenance,
    /// The `mcpServers` keys it declares, sorted; empty when unparseable.
    pub servers: Vec<String>,
}

/// The directories a scan probes for a stray `.mcp.json`.
///
/// Why: see the module doc — the bound is the design decision here, and it is a
/// single function so the check and the repair can never sweep different sets.
/// What: the strict ancestors of `workspace` (nearest first), stopping after
/// `home` (inclusive), before the filesystem root, or after
/// [`MAX_ANCESTOR_DEPTH`] entries; then `std::env::temp_dir()` and `/tmp`.
/// Deduplicated, order-stable. Never includes `workspace` itself — that file is
/// the project's own managed config, not a stray. Pure path arithmetic plus the
/// temp-dir lookup; touches the filesystem only through the caller's later
/// `stat`.
/// Test: `scan_dirs_walks_ancestors_up_to_home`,
/// `scan_dirs_excludes_the_workspace_itself`,
/// `scan_dirs_never_reaches_the_filesystem_root`,
/// `scan_dirs_stops_at_the_depth_cap_outside_home`,
/// `scan_dirs_includes_the_temp_roots`.
pub fn scan_dirs(workspace: Option<&Path>, home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(workspace) = workspace {
        for ancestor in workspace.ancestors().skip(1).take(MAX_ANCESTOR_DEPTH) {
            // The filesystem root is excluded: a `.mcp.json` there is system
            // configuration, and tm never places a workspace at `/`.
            if ancestor.parent().is_none() {
                break;
            }
            dirs.push(ancestor.to_path_buf());
            // Home is the ceiling — above it lies other users' and the
            // system's business.
            if ancestor == home {
                break;
            }
        }
    }

    // Not ancestors of any workspace: these are the ancestors of agent
    // scratchpad cwds, which is why a file here reaches so many sessions.
    for temp in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        dirs.push(temp);
    }

    // Dedupe by CANONICAL form, not by spelling. On macOS `/tmp` is a symlink
    // to `/private/tmp`, and the ancestor walk reaches the latter while the
    // temp-root list names the former — without this the same file is reported
    // twice and an operator reasonably concludes there are two of them.
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| {
        let key = std::fs::canonicalize(d).unwrap_or_else(|_| d.clone());
        seen.insert(key)
    });
    dirs
}

/// Find every stray `.mcp.json` in the bounded scan set.
///
/// Why: one read-only pass feeds both the doctor check and the repair preview.
/// What: for each directory from [`scan_dirs`] holding a regular-file
/// `.mcp.json`, classifies it via [`mcp_provenance::classify`] and collects the
/// `mcpServers` keys. A symlink is reported (so the operator sees it) but
/// carries a [`Provenance::Unknown`] verdict, because what it points at is not
/// the file that was found and following it is how a repair damages something
/// elsewhere. Writes nothing.
/// Test: `scan_finds_a_stray_above_the_workspace`,
/// `scan_ignores_the_workspaces_own_file`,
/// `scan_reports_a_symlink_as_unknown`,
/// `scan_lists_the_declared_servers`.
pub fn scan(workspace: Option<&Path>, home: &Path, ledger: &LedgerLoad) -> Vec<StrayMcpFile> {
    let mut found = Vec::new();
    for dir in scan_dirs(workspace, home) {
        let path = dir.join(MCP_JSON);
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let provenance = if meta.file_type().is_symlink() {
            Provenance::Unknown(
                "the path is a symlink — what it points at is not the file found here".to_string(),
            )
        } else if !meta.is_file() {
            continue;
        } else {
            mcp_provenance::classify(ledger, &path)
        };
        let servers = declared_servers(&path);
        found.push(StrayMcpFile {
            path,
            provenance,
            servers,
        });
    }
    found
}

/// The `mcpServers` keys a `.mcp.json` declares.
///
/// Why: the report names them so an operator can recognise the file without
/// opening it — the point being that a mixed file (framework servers beside
/// hand-added ones) is visibly the case where deletion would lose something.
/// This is for the OPERATOR's judgement, never for tm's: nothing downstream
/// derives provenance from these names.
/// What: sorted top-level keys of `mcpServers`; empty on any read or parse
/// failure, which is a display gap, not an error worth failing a check over.
/// Test: `scan_lists_the_declared_servers`, `declared_servers_tolerates_garbage`.
fn declared_servers(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut names: Vec<String> = value
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// Quarantine every stray `.mcp.json` tm can PROVE it wrote.
///
/// Why: this is the sweep `tm doctor --fix` runs, and its safety rests
/// entirely on the ledger. Only [`Provenance::TmWritten`] — tm wrote it and the
/// bytes are unchanged — is acted on. An unattributed file (every file written
/// before the ledger shipped, including the one that motivated this work), a
/// file edited after tm wrote it, and anything whose provenance could not be
/// determined are each REFUSED with the reason shown, exactly as
/// `refuse_legacy_sources` reports rather than acts. The operator clears those
/// with [`quarantine_explicit`], where naming the path IS the attribution.
/// What: one [`RepairStep`] per finding; [`RepairMode::DryRun`] classifies and
/// prints without touching the filesystem.
/// Test: `sweep_quarantines_a_tm_written_stray`,
/// `sweep_refuses_an_unattributed_stray`,
/// `sweep_refuses_a_stray_edited_after_tm_wrote_it`,
/// `sweep_refuses_when_the_ledger_is_unreadable`,
/// `sweep_dry_run_writes_nothing`,
/// `sweep_never_touches_the_workspaces_own_file`.
pub fn quarantine_strays(
    framework_root: &Path,
    workspace: Option<&Path>,
    home: &Path,
    mode: RepairMode,
) -> Vec<RepairStep> {
    let ledger = mcp_provenance::load(framework_root);
    scan(workspace, home, &ledger)
        .into_iter()
        .map(|stray| match &stray.provenance {
            Provenance::TmWritten => apply_or_plan(framework_root, &stray.path, mode),
            Provenance::TmWrittenThenEdited => refuse(
                &stray.path,
                "tm wrote this file, but its bytes changed afterwards — the current content is \
                 somebody's edit, not tm's output. Quarantine it explicitly with \
                 `tm doctor --quarantine-mcp <path>` once you have confirmed nothing needs it",
            ),
            Provenance::Unattributed => refuse(
                &stray.path,
                "tm has no record of writing this file, and its contents cannot prove \
                 authorship — a `.mcp.json` full of trusty-* servers may equally be one the \
                 operator wrote. Inspect it, then quarantine it explicitly with \
                 `tm doctor --quarantine-mcp <path>`",
            ),
            Provenance::Unknown(why) => refuse(&stray.path, why),
        })
        .collect()
}

/// Quarantine one `.mcp.json` the operator named on the command line.
///
/// Why: the sweep above can never clear a file written before the ledger
/// existed, which is every stray on disk today. Rather than weaken the sweep's
/// evidence rule — the one thing standing between a repair and somebody's
/// hand-written config — this takes the attribution from the operator: naming
/// an exact path is a deliberate act tm cannot perform on its own, and it is
/// still a dry run until `--yes`.
///
/// It refuses three things regardless of what was named: a path that is not a
/// `.mcp.json` (the operator meant something else, and this command has no
/// business renaming an arbitrary file), the WORKSPACE's own `.mcp.json` (live
/// managed config, whose removal breaks the current project rather than fixing
/// anything), and a symlink (the bytes are elsewhere).
/// What: one [`RepairStep`]. On [`RepairMode::Apply`] the rename happens and
/// the ledger claim, if any, is released.
/// Test: `explicit_quarantines_an_unattributed_file`,
/// `explicit_refuses_a_non_mcp_path`,
/// `explicit_refuses_the_workspaces_own_file`,
/// `explicit_refuses_a_symlink`, `explicit_refuses_a_missing_file`,
/// `explicit_dry_run_writes_nothing`.
pub fn quarantine_explicit(
    framework_root: &Path,
    workspace: Option<&Path>,
    target: &Path,
    mode: RepairMode,
) -> RepairStep {
    if target.file_name().and_then(|n| n.to_str()) != Some(MCP_JSON) {
        return refuse(
            target,
            "not a `.mcp.json` — this command only quarantines MCP config files discovered by \
             Claude Code's upward walk",
        );
    }
    if let Some(workspace) = workspace
        && target.parent() == Some(workspace)
    {
        return refuse(
            target,
            "this is the current workspace's own managed `.mcp.json`, not a stray above it — \
             removing it would strip this project's MCP servers",
        );
    }
    match std::fs::symlink_metadata(target) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return refuse(
                target,
                "the path is a symlink — renaming it would leave the real file in place and \
                 still discovered",
            );
        }
        Ok(meta) if !meta.is_file() => {
            return refuse(target, "not a regular file");
        }
        Ok(_) => {}
        Err(e) => return refuse(target, &format!("cannot stat the path: {e}")),
    }
    apply_or_plan(framework_root, target, mode)
}

/// Rename the file aside, or describe the rename.
///
/// Why: preview and apply must be one code path — a separate planner drifts
/// from the actor exactly when it matters, which is the rule
/// [`RepairMode`] exists to enforce.
/// What: in [`RepairMode::DryRun`] returns a [`StepStatus::Planned`] naming the
/// destination. In [`RepairMode::Apply`] renames `.mcp.json` to
/// `.mcp.json.quarantined-<epoch>` in the SAME directory (so the rename is
/// atomic — same filesystem — and no bytes are copied or lost), then releases
/// the ledger claim. The destination is reported as the backup, because that is
/// exactly what it is.
/// Test: `sweep_quarantines_a_tm_written_stray`,
/// `explicit_quarantines_an_unattributed_file`,
/// `quarantine_destination_does_not_collide`.
fn apply_or_plan(framework_root: &Path, path: &Path, mode: RepairMode) -> RepairStep {
    let dest = quarantine_destination(path);
    let what = format!(
        "rename aside to `{}` so Claude Code's upward walk stops finding it",
        dest.file_name().unwrap_or_default().to_string_lossy()
    );
    if mode == RepairMode::DryRun {
        return RepairStep {
            check: CHECK_NAME,
            path: path.to_path_buf(),
            what,
            status: StepStatus::Planned,
        };
    }
    match std::fs::rename(path, &dest) {
        Ok(()) => {
            // The file is gone from that path, so the ledger's claim on it is
            // stale. Released AFTER the rename: a claim outliving one failed
            // rename is harmless, a released claim over a file still present
            // would lose the attribution that made it repairable.
            mcp_provenance::forget(framework_root, path);
            RepairStep {
                check: CHECK_NAME,
                path: path.to_path_buf(),
                what,
                status: StepStatus::Applied { backup: Some(dest) },
            }
        }
        Err(e) => RepairStep {
            check: CHECK_NAME,
            path: path.to_path_buf(),
            what,
            status: StepStatus::Failed(e.to_string()),
        },
    }
}

/// Where a quarantined `.mcp.json` is moved to.
///
/// Why: the basename must stop matching `.mcp.json` exactly — that is what ends
/// the discovery — while staying beside the original so the rename is atomic
/// and the operator finds it without being told where to look.
/// What: `<dir>/.mcp.json.quarantined-<unix-seconds>`, with a numeric suffix if
/// that already exists, so a second run in the same second never overwrites the
/// first quarantine.
/// Test: `quarantine_destination_does_not_collide`.
fn quarantine_destination(path: &Path) -> PathBuf {
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let base = format!("{MCP_JSON}.quarantined-{epoch}");
    let mut dest = path.with_file_name(&base);
    let mut n = 1;
    while dest.exists() {
        dest = path.with_file_name(format!("{base}.{n}"));
        n += 1;
    }
    dest
}

/// A refusal step naming the path and the reason.
fn refuse(path: &Path, why: &str) -> RepairStep {
    RepairStep {
        check: CHECK_NAME,
        path: path.to_path_buf(),
        what: "quarantine this stray MCP config".to_string(),
        status: StepStatus::Refused(why.to_string()),
    }
}

#[cfg(test)]
#[path = "stray_mcp_tests.rs"]
mod tests;
