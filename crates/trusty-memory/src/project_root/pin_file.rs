//! Pin-file I/O — read, write, and slug derivation helpers.
//!
//! Why: Isolating the pin-file I/O from detection and validation keeps each
//! file under the 500-SLOC cap and groups the YAML serialization in one place.
//! What: `ProjectPin`, `PIN_SCHEMA_VERSION`, `PIN_FILE_REL`, `read_project_pin`,
//! `write_project_pin`, `project_slug_from_basename`, `project_slug_at`,
//! `project_slug_at_readonly`, `project_slug`.
//! Test: `pin_file_read_when_present`, `absent_pin_writes_computed_slug`,
//! `renamed_dir_with_pin_resolves_to_original_slug`.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::messaging::slugify_string;

use super::detection::{find_project_root, is_unsafe_pin_location, TRUSTY_TOOLS_DIR};

/// Schema version for `.trusty-tools/trusty-memory.yaml`.
///
/// Why: forward-proofing — a future phase may need to distinguish older pin
/// files that lack new fields. Hard-coding `1` now makes that migration
/// straightforward: read `schema_version`, branch on the value.
/// What: the `u32` constant `1`.
/// Test: `write_project_pin` embeds this value; `read_project_pin` accepts it.
pub const PIN_SCHEMA_VERSION: u32 = 1;

/// Relative path of the pin file within a project root.
///
/// Why: defined as a constant so every call site (`read_project_pin`,
/// `write_project_pin`, `find_project_root`) agrees on the same path and
/// tests can compare against this value instead of a bare string literal.
/// What: `".trusty-tools/trusty-memory.yaml"`.
/// Test: used in every pin-file test in this module.
pub const PIN_FILE_REL: &str = ".trusty-tools/trusty-memory.yaml";

/// Serialisable schema for `.trusty-tools/trusty-memory.yaml`.
///
/// Why: a typed struct with `serde` makes the YAML schema self-documenting
/// and prevents future fields from silently deserialising to wrong types.
/// What: holds `schema_version` (always 1 for Phase 1) and `palace` (the
/// pinned slug string). An optional `note` field is supported for humans who
/// want to document why the slug was pinned.
/// Test: `write_project_pin` round-trips through `read_project_pin`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectPin {
    /// Pin-file format version. Always `1` in Phase 1.
    pub schema_version: u32,
    /// The pinned palace slug — stored verbatim, no re-slugification.
    pub palace: String,
    /// Optional human note (e.g. "pinned before drive reorg 2026-06").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Read the palace pin from `.trusty-tools/trusty-memory.yaml` at `root`.
///
/// Why: the pin file is the authoritative source for a project's palace slug
/// when present. Reading it in a dedicated helper keeps the I/O concern
/// separate from the slug-derivation logic and makes it easy to test the
/// round-trip in isolation.
/// What: constructs the path `root/.trusty-tools/trusty-memory.yaml`, reads
/// it, and deserialises with `serde_yaml`. Returns `None` when the file does
/// not exist. Returns `Err` only on I/O or parse failures.
/// Test: `pin_file_read_when_present`, `read_project_pin_returns_none_when_absent`.
pub fn read_project_pin(root: &Path) -> Result<Option<ProjectPin>> {
    let pin_path = root.join(PIN_FILE_REL);
    match std::fs::read_to_string(&pin_path) {
        Ok(s) => {
            let pin: ProjectPin = serde_yaml::from_str(&s)
                .map_err(|e| anyhow::anyhow!("parse {}: {e}", pin_path.display()))?;
            Ok(Some(pin))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::anyhow!("read {}: {e}", pin_path.display())),
    }
}

/// Write a palace pin to `.trusty-tools/trusty-memory.yaml` at `root`.
///
/// Why: the lazy-write path in `project_slug_at` and the explicit
/// `trusty-memory link` backfill command both need to emit the same YAML
/// schema. A single writer keeps the format consistent and avoids duplicated
/// YAML-construction logic.
/// What: creates `.trusty-tools/` if missing, serialises `pin` with
/// `serde_yaml`, and writes it atomically (write to `<file>.tmp`, then
/// rename). Returns the path that was written.
/// Test: `write_project_pin_creates_expected_yaml`,
///       `write_project_pin_round_trips_through_read`.
pub fn write_project_pin(root: &Path, pin: &ProjectPin) -> Result<PathBuf> {
    let dir = root.join(TRUSTY_TOOLS_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| anyhow::anyhow!("create {}: {e}", dir.display()))?;
    let pin_path = root.join(PIN_FILE_REL);
    let tmp_path = pin_path.with_extension("yaml.tmp");
    let yaml = serde_yaml::to_string(pin).map_err(|e| anyhow::anyhow!("serialise pin: {e}"))?;
    let header = "# .trusty-tools/trusty-memory.yaml\n\
                  # This file pins the trusty-memory palace slug for this project.\n\
                  # Commit it so the linkage survives directory renames and drive reorgs.\n\
                  # Schema: https://github.com/bobmatnyc/trusty-tools (trusty-tools convention)\n\n";
    let content = format!("{header}{yaml}");
    std::fs::write(&tmp_path, &content)
        .map_err(|e| anyhow::anyhow!("write {}: {e}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &pin_path).map_err(|e| {
        anyhow::anyhow!(
            "rename {} → {}: {e}",
            tmp_path.display(),
            pin_path.display()
        )
    })?;
    Ok(pin_path)
}

/// Compute the palace slug purely from the directory basename (the pre-Phase-1
/// logic, now extracted for composability).
///
/// Why: the resolution order in `project_slug_at` needs to call the basename
/// derivation without triggering the pin-file read/write side effects. Exposing
/// this as a separate function makes both paths testable in isolation.
/// What: calls `slugify_string` on the last path component of `root`. Returns
/// `None` when the basename is empty or slugifies to an empty string.
/// Test: `project_slug_from_basename_basic`.
pub fn project_slug_from_basename(root: &Path) -> Option<String> {
    let basename = root.file_name()?.to_str()?;
    let slug = slugify_string(basename);
    if slug.is_empty() {
        None
    } else {
        Some(slug)
    }
}

/// Derive a palace slug from the project root found at or above `start`.
///
/// Why: the core of issue #88 with Phase-1 pin-file support. Palace names
/// must match the canonical slug of the project they belong to, and that slug
/// must survive directory renames. The pin file provides the stable anchor.
/// What: implements the two-step resolution order:
///   a. Walk up to the project root. If `.trusty-tools/trusty-memory.yaml`
///      exists, return `pin.palace` (authoritative — survives renames).
///   b. If absent, compute the slug via `project_slug_from_basename`, then
///      lazily write the pin file (best-effort, non-fatal) so future calls
///      always land on path (a).
/// Returns `None` when no project root is found.
/// Test: `pin_file_read_when_present`, `absent_pin_writes_computed_slug`,
///       `renamed_dir_with_pin_resolves_to_original_slug`.
pub fn project_slug_at(start: &Path) -> Option<String> {
    let root = find_project_root(start)?;

    // Step (a): check for a committed pin file.
    match read_project_pin(&root) {
        Ok(Some(pin)) => return Some(pin.palace),
        Ok(None) => {} // absent — fall through to step (b)
        Err(e) => {
            // Corrupt or unreadable pin file: log to stderr and fall through
            // to the basename derivation so memory operations are not blocked.
            tracing::warn!(
                path = %root.join(PIN_FILE_REL).display(),
                "could not read palace pin file ({e:#}); falling back to basename slug"
            );
        }
    }

    // Step (b): compute from basename and lazily write the pin file.
    // Guard: never write into a system temp dir, home dir, or filesystem root —
    // those are unsafe pin locations that would poison every subdirectory.
    // `is_unsafe_pin_location` was introduced in PR #492 for exactly this
    // case; if the resolved root is unsafe we still return the derived slug
    // (so memory operations work) but skip the write.
    let slug = project_slug_from_basename(&root)?;
    if is_unsafe_pin_location(&root) {
        tracing::debug!(
            slug = %slug,
            root = %root.display(),
            "skipping lazy pin write: root is a system/home/temp dir"
        );
        return Some(slug);
    }
    let pin = ProjectPin {
        schema_version: PIN_SCHEMA_VERSION,
        palace: slug.clone(),
        note: None,
    };
    match write_project_pin(&root, &pin) {
        Ok(path) => {
            tracing::debug!(
                slug = %slug,
                path = %path.display(),
                "wrote palace pin file (lazy init)"
            );
        }
        Err(e) => {
            // Read-only tree, insufficient permissions, etc. — non-fatal.
            tracing::warn!(
                slug = %slug,
                root = %root.display(),
                "could not write palace pin file ({e:#}); slug will remain basename-derived"
            );
        }
    }
    Some(slug)
}

/// Return the *pinned* palace slug for the project at or above `start`, and
/// ONLY when a committed pin file exists — never the basename fallback.
///
/// Why: issue #1217 inserts git-`owner/repo` identity derivation between the
/// pin file (authoritative, rename-stable) and the directory-basename
/// fallback. The default-palace resolver therefore needs to consult the pin
/// file *in isolation* — if it used `project_slug_at_readonly` it would also
/// receive the basename slug, which would shadow the new git derivation and
/// reduce the change to a no-op inside any project directory. This helper
/// returns `Some` strictly when a pin exists, so callers can honour the pin
/// first and fall through to identity derivation when absent.
/// What: walks up to the project root, reads `.trusty-tools/trusty-memory.yaml`
/// via [`read_project_pin`], and returns `Some(pin.palace)` when present.
/// Returns `None` when no project root is found OR no pin file exists OR the
/// pin file is unreadable (logged, non-fatal — never panics, never writes).
/// Test: `pinned_slug_at_returns_pin_when_present`,
/// `pinned_slug_at_returns_none_without_pin`.
pub fn pinned_slug_at(start: &Path) -> Option<String> {
    let root = find_project_root(start)?;
    match read_project_pin(&root) {
        Ok(Some(pin)) if !pin.palace.is_empty() => Some(pin.palace),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(
                path = %root.join(PIN_FILE_REL).display(),
                "could not read palace pin file ({e:#}); ignoring pin and falling through"
            );
            None
        }
    }
}

/// Derive a palace slug from the project root found at or above `start`,
/// WITHOUT the lazy-write side-effect.
///
/// Why: the `prompt-context` hook runs in read-only or short-lived contexts
/// where creating `.trusty-tools/trusty-memory.yaml` would be surprising and
/// potentially disruptive. The slug is still resolved via the pin-file when
/// one already exists (step a), and falls back to the basename slug (step b)
/// without ever writing a new file. This makes `cwd_palace_slug_at` safe to
/// call unconditionally from hooks. The writing variant (`project_slug_at`)
/// remains the right choice for interactive commands (`trusty-memory link`,
/// `trusty-memory remember`) that want to stabilise the slug.
/// What: same two-step resolution as `project_slug_at` but step (b) only
/// computes and returns the basename slug — it does NOT write the pin file.
/// Returns `None` when no project root is found.
/// Test: `project_slug_at_readonly_no_write_when_absent`,
///       `project_slug_at_readonly_reads_existing_pin`,
///       `project_slug_at_readonly_falls_back_to_basename`.
pub fn project_slug_at_readonly(start: &Path) -> Option<String> {
    let root = find_project_root(start)?;

    // Step (a): if a pin file exists, use it authoritatively.
    match read_project_pin(&root) {
        Ok(Some(pin)) => return Some(pin.palace),
        Ok(None) => {} // absent — fall through to step (b)
        Err(e) => {
            // Corrupt or unreadable pin file: log to stderr and fall through
            // so the hook is not blocked.
            tracing::warn!(
                path = %root.join(PIN_FILE_REL).display(),
                "could not read palace pin file ({e:#}); falling back to basename slug (read-only)"
            );
        }
    }

    // Step (b): compute from basename — but do NOT write a pin file.
    project_slug_from_basename(&root)
}

/// Derive a palace slug for the current working directory.
///
/// Why: convenience wrapper over `project_slug_at` for callers that want
/// the "natural" project slug (CLI commands, MCP handlers, tests running
/// inside a repo).
/// What: calls `std::env::current_dir()`, propagates the error if the syscall
/// fails, then delegates to [`project_slug_at`].
/// Test: `project_slug_finds_git_root` (run from inside the trusty-tools repo
/// which is a git checkout).
pub fn project_slug() -> Result<Option<String>> {
    let cwd = std::env::current_dir().map_err(|e| anyhow::anyhow!("read cwd: {e}"))?;
    Ok(project_slug_at(&cwd))
}
