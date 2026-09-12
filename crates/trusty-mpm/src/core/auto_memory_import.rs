//! Migrate Claude Code's auto-memory store into the project's palace (#7685).
//!
//! Why: the owner ruling of 2026-09-12 makes trusty-memory the memory and Claude
//! Code's auto memory a fallback. Turning auto memory off is only half of that —
//! the facts it already captured live in
//! `<claude_config_dir>/projects/<slug>/memory/`, and a project whose
//! `MEMORY.md` is merely zeroed has LOST them. This module is the other half:
//! it moves each fact into the palace first, and only then empties the index.
//! What: [`run_auto_memory_import`] reads every fact file in that directory,
//! stores it as a drawer tagged [`MIGRATION_TAG`] plus `type:<type>` and
//! `name:<name>`, moves the file into a dated archive beside the memory
//! directory, and — once every file has landed — archives and truncates
//! `MEMORY.md`.
//!
//! Two properties it is built around:
//!
//! * **Nothing is ever deleted.** Facts are MOVED into
//!   `memory.archived-<YYYYMMDD>/`, and `MEMORY.md` is copied there before it is
//!   truncated. A migration that turns out to have been wrong is recoverable
//!   with `mv`.
//! * **A failed store leaves its file alone.** The archive move happens per file,
//!   AFTER that file's drawer id comes back, and a single failure also stops the
//!   index from being truncated — so the index still names every file that is
//!   still on disk, and a re-run picks up exactly the remainder.
//!
//! Idempotent by construction: a second run finds no fact files (they are in the
//! archive) and an empty index, so it stores nothing.
//! Test: `core::auto_memory_import::tests`.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::Serialize;

use crate::core::memory_import::{parse_memory_file, remember};

/// The tag every migrated drawer carries.
///
/// Why: one tag makes the whole migration addressable afterwards —
/// `memory_list { tag }` recovers exactly what this command wrote, which is what
/// an operator needs to audit or undo it.
/// What: the literal tag, applied to every drawer this module stores.
/// Test: `auto_memory_import_stores_and_archives_each_fact`.
pub const MIGRATION_TAG: &str = "migrated-from-auto-memory";

/// Claude Code's index file inside an auto-memory directory.
pub const INDEX_FILE: &str = "MEMORY.md";

/// Everything [`run_auto_memory_import`] needs for one migration.
///
/// Why: the CLI layer stays a pure translation of clap args, and every input a
/// test must pin — the config dir, the palace, the socket, the archive date — is
/// a field rather than something the run reads from the host.
/// What: where the auto-memory store is, where the drawers go, and the date
/// stamp the archive directory is named for.
/// Test: `auto_memory_import_stores_and_archives_each_fact`,
/// `auto_memory_import_is_idempotent`,
/// `auto_memory_import_leaves_a_failed_file_in_place`.
#[derive(Debug, Clone)]
pub struct AutoImportOptions {
    /// The project whose auto-memory store is migrated.
    pub project_dir: PathBuf,
    /// The Claude config dir holding `projects/<slug>/memory/`.
    pub config_dir: PathBuf,
    /// Target palace slug.
    pub palace: String,
    /// Explicit trusty-memory socket. `None` uses daemon discovery.
    pub memory_socket: Option<PathBuf>,
    /// `YYYYMMDD` stamp the archive directory is named for.
    pub archive_stamp: String,
}

/// What happened to one fact file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoImportStatus {
    /// Stored in the palace and moved into the archive.
    Stored,
    /// Not stored; the file was left exactly where it was.
    Failed,
}

/// One row of the migration report.
#[derive(Debug, Clone, Serialize)]
pub struct AutoFileResult {
    /// The fact file's basename.
    pub file: String,
    /// Stored, or failed.
    pub status: AutoImportStatus,
    /// The drawer the fact landed in, when it landed.
    pub drawer_id: Option<String>,
    /// The tags written with it.
    pub tags: Vec<String>,
    /// Why it failed, when it failed.
    pub error: Option<String>,
}

/// The machine-readable result of one migration.
#[derive(Debug, Clone, Serialize)]
pub struct AutoImportReport {
    /// The auto-memory directory that was read.
    pub dir: String,
    /// The palace the drawers were written to.
    pub palace: String,
    /// The archive directory, when anything was archived.
    pub archive: Option<String>,
    /// Fact files seen.
    pub total: usize,
    /// Fact files stored and archived.
    pub stored: usize,
    /// Fact files that failed and are still on disk.
    pub failed: usize,
    /// Whether `MEMORY.md` was archived and emptied by this run.
    pub index_cleared: bool,
    /// Per-file detail, in filename order.
    pub files: Vec<AutoFileResult>,
}

/// Resolve the options for a `tm memory import-auto-memory` run.
///
/// Why: the CLI layer stays a pure translation of clap args — every "where does
/// this come from" answer (the managed config dir, the palace slug, today's
/// date) is resolved here, once, beside the code that consumes it. A palace the
/// caller did not name is the one the launch path would pin for this project, so
/// the migrated drawers land where the session will look for them.
/// What: fills [`AutoImportOptions`] from `project_dir`, using
/// [`crate::core::trusty_tools_config::managed_claude_config_dir`] and
/// [`crate::core::session_launch::resolve_palace_slug`]. Errors when either
/// cannot be resolved, rather than migrating into a guessed palace.
/// Test: covered through [`resolve_in`], which is this function with the config
/// dir supplied — see `resolve_auto_import_options_keeps_an_explicit_palace`.
pub fn resolve_auto_import_options(
    project_dir: &Path,
    palace: Option<String>,
    memory_socket: Option<PathBuf>,
) -> anyhow::Result<AutoImportOptions> {
    let config_dir = crate::core::trusty_tools_config::managed_claude_config_dir().context(
        "cannot resolve the tm-managed Claude config dir, so there is no auto-memory store to read",
    )?;
    resolve_in(project_dir, config_dir, palace, memory_socket)
}

/// [`resolve_auto_import_options`] with the config dir supplied.
///
/// Why: the managed config dir is derived from the host's own install layout,
/// which a test cannot point anywhere — so the decision worth testing (an
/// explicit palace is taken verbatim, with today's stamp beside it) would
/// otherwise be untestable.
/// What: defaults the palace to the slug this project's sessions pin, and stamps
/// the archive directory with today's date. A palace that cannot be derived is
/// an error naming `--palace`, never a guess — migrating into the wrong palace
/// would scatter the facts somewhere no session looks.
/// Test: `resolve_auto_import_options_keeps_an_explicit_palace`.
pub(crate) fn resolve_in(
    project_dir: &Path,
    config_dir: PathBuf,
    palace: Option<String>,
    memory_socket: Option<PathBuf>,
) -> anyhow::Result<AutoImportOptions> {
    let palace = match palace {
        Some(palace) => palace,
        None => crate::core::session_launch::resolve_palace_slug(project_dir, None).with_context(
            || {
                format!(
                    "cannot derive a palace slug for {} — pass --palace",
                    project_dir.display()
                )
            },
        )?,
    };
    Ok(AutoImportOptions {
        project_dir: project_dir.to_path_buf(),
        config_dir,
        palace,
        memory_socket,
        archive_stamp: chrono::Local::now().format("%Y%m%d").to_string(),
    })
}

/// The auto-memory directory Claude Code keeps for `project_dir`.
///
/// Why: `tm doctor`'s `auto_memory` row and this migration must look at the SAME
/// directory, and the encoding of a workspace path into a project-directory name
/// is Claude Code's, reproduced once in
/// [`crate::runtime::encode_project_dir`].
/// What: `<config_dir>/projects/<encoded project dir>/memory`.
/// Test: `auto_memory_dir_uses_the_claude_project_encoding`.
pub fn auto_memory_dir(config_dir: &Path, project_dir: &Path) -> PathBuf {
    config_dir
        .join("projects")
        .join(crate::runtime::encode_project_dir(project_dir))
        .join("memory")
}

/// Whether `MEMORY.md` in `memory_dir` still holds anything.
///
/// Why: an emptied index is the observable end state of a migration, so the
/// doctor row grades it and this module decides when to produce it.
/// What: true when the file exists and its trimmed contents are non-empty. An
/// absent or unreadable file is false — there is nothing to migrate either way.
/// Test: `index_has_content_reads_the_index_file`.
pub fn index_has_content(memory_dir: &Path) -> bool {
    std::fs::read_to_string(memory_dir.join(INDEX_FILE))
        .map(|text| !text.trim().is_empty())
        .unwrap_or(false)
}

/// Migrate one project's auto-memory store into its palace.
///
/// Why: see the module doc — this is the command behind
/// `tm memory import-auto-memory`, and the reason zeroing `MEMORY.md` is safe.
/// What: stores each fact file as a drawer, moves it into
/// `memory.archived-<stamp>/`, and clears the index only when every file landed.
/// An absent auto-memory directory is an empty report, not an error, so the
/// command is safe to run against a project that never had one.
/// Test: `auto_memory_import_stores_and_archives_each_fact`,
/// `auto_memory_import_is_idempotent`,
/// `auto_memory_import_leaves_a_failed_file_in_place`,
/// `auto_memory_import_on_an_absent_store_is_a_no_op`.
pub async fn run_auto_memory_import(opts: &AutoImportOptions) -> anyhow::Result<AutoImportReport> {
    let memory_dir = auto_memory_dir(&opts.config_dir, &opts.project_dir);
    let mut report = AutoImportReport {
        dir: memory_dir.display().to_string(),
        palace: opts.palace.clone(),
        archive: None,
        total: 0,
        stored: 0,
        failed: 0,
        index_cleared: false,
        files: Vec::new(),
    };
    if !memory_dir.is_dir() {
        return Ok(report);
    }

    let facts = fact_files(&memory_dir)?;
    report.total = facts.len();
    if facts.is_empty() && !index_has_content(&memory_dir) {
        // The idempotent second run: everything is already in the archive.
        return Ok(report);
    }

    let socket = match &opts.memory_socket {
        Some(socket) => socket.clone(),
        None => trusty_common::memory_rpc::resolve_memory_socket()
            .context("resolve the trusty-memory daemon socket")?,
    };
    let archive = archive_dir(&memory_dir, &opts.archive_stamp);

    for path in facts {
        report
            .files
            .push(migrate_one(&socket, opts, &path, &archive).await);
    }
    report.stored = report
        .files
        .iter()
        .filter(|f| f.status == AutoImportStatus::Stored)
        .count();
    report.failed = report.files.len() - report.stored;
    if report.stored > 0 {
        report.archive = Some(archive.display().to_string());
    }

    // The index names the files; clearing it while any of them is still on disk
    // would strand exactly the ones that failed.
    if report.failed == 0 && index_has_content(&memory_dir) {
        clear_index(&memory_dir, &archive)?;
        report.archive = Some(archive.display().to_string());
        report.index_cleared = true;
    }
    Ok(report)
}

/// Store one fact file, then move it into the archive.
///
/// Why: the ORDER is the fail-open guarantee — a file is archived only after its
/// drawer id came back, so a declined write, an unparseable file or a dead
/// daemon all leave the fact exactly where it was.
/// What: parses, derives [`migration_tags`], writes through
/// [`crate::core::memory_import::remember`], then renames the file under
/// `archive`. A rename failure is reported as a failure even though the drawer
/// exists, because the file is still there and a re-run must see it.
/// Test: `auto_memory_import_leaves_a_failed_file_in_place`.
async fn migrate_one(
    socket: &Path,
    opts: &AutoImportOptions,
    path: &Path,
    archive: &Path,
) -> AutoFileResult {
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let failed = |tags: Vec<String>, error: String| AutoFileResult {
        file: file.clone(),
        status: AutoImportStatus::Failed,
        drawer_id: None,
        tags,
        error: Some(error),
    };

    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(e) => return failed(Vec::new(), format!("read failed: {e}")),
    };
    let parsed = match parse_memory_file(&source) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => return failed(Vec::new(), "no YAML frontmatter".to_string()),
        Err(e) => return failed(Vec::new(), format!("parse failed: {e:#}")),
    };
    let tags = migration_tags(&parsed.name, &parsed.kind);

    // `allow_secret_like`: these facts are already on this machine in plain
    // text; refusing to move one because its prose looks token-shaped would
    // strand it in the store this command exists to empty.
    let drawer_id = match remember(socket, &opts.palace, &parsed.text, &tags, true).await {
        Ok(id) => id,
        Err(e) => return failed(tags, format!("store failed: {e:#}")),
    };
    if let Err(e) = archive_file(path, archive, &file) {
        return failed(
            tags,
            format!("stored as {drawer_id} but not archived: {e:#}"),
        );
    }
    AutoFileResult {
        file,
        status: AutoImportStatus::Stored,
        drawer_id: Some(drawer_id),
        tags,
        error: None,
    }
}

/// The tag set one migrated fact carries.
///
/// Why: the migration tag makes the whole batch addressable; `name:` preserves
/// the fact's own slug as a lookup key; `type:` preserves Claude Code's
/// classification. All three are prefixed or literal so they cannot collide with
/// a tag a human wrote.
/// What: [`MIGRATION_TAG`], `name:<name>`, and `type:<kind>` when `kind` is
/// non-empty.
/// Test: `migration_tags_omit_an_empty_type`.
fn migration_tags(name: &str, kind: &str) -> Vec<String> {
    let mut tags = vec![MIGRATION_TAG.to_string(), format!("name:{name}")];
    if !kind.trim().is_empty() {
        tags.push(format!("type:{kind}"));
    }
    tags
}

/// Where migrated files go: `memory.archived-<stamp>` beside the memory dir.
///
/// Why: a sibling rather than a subdirectory of `memory/`, so Claude Code cannot
/// read the archived facts back as live memory.
/// What: `<memory_dir parent>/memory.archived-<stamp>`; falls back to a
/// subdirectory only if `memory_dir` somehow has no parent.
/// Test: `auto_memory_import_stores_and_archives_each_fact`.
fn archive_dir(memory_dir: &Path, stamp: &str) -> PathBuf {
    let name = format!("memory.archived-{stamp}");
    match memory_dir.parent() {
        Some(parent) => parent.join(name),
        None => memory_dir.join(name),
    }
}

/// Move one file into the archive, creating it on first use.
fn archive_file(path: &Path, archive: &Path, file: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(archive)
        .with_context(|| format!("create archive dir {}", archive.display()))?;
    std::fs::rename(path, archive.join(file))
        .with_context(|| format!("archive {} into {}", path.display(), archive.display()))
}

/// Archive a copy of `MEMORY.md`, then truncate it.
///
/// Why: the index is what Claude Code reads at session start, so emptying it is
/// the point of the migration — but "never delete" applies to it too, and the
/// copy is what makes the truncation reversible.
/// What: copies the index into `archive` under its own name, then writes an
/// empty file in its place. The file itself is kept: Claude Code re-creating it
/// is not something this command should race.
/// Test: `auto_memory_import_stores_and_archives_each_fact`.
fn clear_index(memory_dir: &Path, archive: &Path) -> anyhow::Result<()> {
    let index = memory_dir.join(INDEX_FILE);
    std::fs::create_dir_all(archive)
        .with_context(|| format!("create archive dir {}", archive.display()))?;
    std::fs::copy(&index, archive.join(INDEX_FILE))
        .with_context(|| format!("archive {}", index.display()))?;
    std::fs::write(&index, "").with_context(|| format!("empty {}", index.display()))
}

/// The fact files in an auto-memory directory, in filename order.
///
/// Why: deterministic order makes a partial run resumable and two runs
/// comparable — the same reason `memory_import::markdown_files` sorts.
/// What: `*.md` files directly inside `dir`, excluding [`INDEX_FILE`], which is
/// the index rather than a fact.
/// Test: `auto_memory_import_stores_and_archives_each_fact`.
fn fact_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("read auto-memory directory {}", dir.display()))?;
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry
            .with_context(|| format!("read entry in {}", dir.display()))?
            .path();
        if !path.is_file() {
            continue;
        }
        let is_md = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("md"));
        let is_index = path.file_name().and_then(|n| n.to_str()) == Some(INDEX_FILE);
        if is_md && !is_index {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
#[path = "auto_memory_import_tests.rs"]
mod tests;
