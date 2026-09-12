//! Memory files ABOVE the project root, and what they cost (#7673).
//!
//! Why: Claude Code loads every `CLAUDE.md` from the session's cwd up to the
//! filesystem root. A file at `$HOME` — or at `~/work`, or at `/` — is therefore
//! prepended to every turn of every agent in every project beneath it, and
//! nothing in the harness said so. On 2026-09-12 the file found at `$HOME` was
//! tm's own seed template: boilerplate with no project content in it at all,
//! paid for on every turn for as long as it sat there.
//!
//! What: [`scan`] walks the ancestors of a project root and reports every
//! `CLAUDE.md`, `CLAUDE.local.md` and `.claude/CLAUDE.md` it finds, with the
//! file's size, a token estimate, whether it is tm's seed template
//! ([`crate::core::claude_md_seed::is_seed_template`]) and whether the operator
//! has already excluded it via `claudeMdExcludes`. [`warn_ancestors`] is the
//! launch-path WARN; the doctor check and its `--fix` live in
//! `daemon::doctor_ancestor_claude_md` and
//! [`crate::core::ancestor_claude_md_repair`].
//!
//! Read-only, and it stops at the filesystem root. It opens a file only to
//! shape-test it, and only when that file is small enough to be a seed.
//! Test: `ancestor_claude_md_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Bytes per token in the estimate this module reports.
///
/// Why: the operator's question is "what is this costing me", and a byte count
/// does not answer it. Four bytes per token is the rule of thumb for English
/// prose; the divisor is named in every hint so the number is never mistaken
/// for a measurement.
/// What: `4`.
/// Test: `the_token_estimate_divides_bytes_by_four`.
pub const BYTES_PER_TOKEN: u64 = 4;

/// Memory-file names Claude Code loads from each ancestor directory.
///
/// What: `CLAUDE.md` and `CLAUDE.local.md` at the directory itself, plus
/// `.claude/CLAUDE.md` beneath it.
/// Test: `every_memory_file_shape_is_reported`.
const MEMORY_FILE_RELATIVE_PATHS: [&str; 3] = ["CLAUDE.md", "CLAUDE.local.md", ".claude/CLAUDE.md"];

/// One memory file found above the project root.
///
/// Why: the three facts an operator needs in order to act — where it is, what
/// it costs, and whether it is worth anything — belong in one value so the
/// doctor row, the launch WARN and the repair all describe it identically.
/// Test: `ancestor_claude_md_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AncestorMemoryFile {
    /// Absolute path to the file.
    pub path: PathBuf,
    /// Size in bytes, as the filesystem reports it.
    pub bytes: u64,
    /// Whether the content is tm's seed template with nothing added.
    pub seed_template: bool,
    /// Whether `claudeMdExcludes` already keeps this file out of sessions.
    pub excluded: bool,
}

impl AncestorMemoryFile {
    /// The rule-of-thumb token cost of loading this file.
    ///
    /// What: `bytes / 4` — see [`BYTES_PER_TOKEN`].
    /// Test: `the_token_estimate_divides_bytes_by_four`.
    pub fn token_estimate(&self) -> u64 {
        self.bytes / BYTES_PER_TOKEN
    }

    /// One line naming the file, its size, its estimate and its verdict.
    ///
    /// Why: the doctor row, the launch WARN and the repair preview all need the
    /// same rendering, and an operator comparing two of them must not have to
    /// reconcile two formats.
    /// What: `<path> (<bytes> B, ~<tokens> tokens, bytes/4)` plus ` — tm seed
    /// template` or ` — already excluded` where those apply.
    /// Test: `the_summary_names_the_divisor`.
    pub fn summary(&self) -> String {
        let mut line = format!(
            "{} ({} B, ~{} tokens at bytes/{})",
            self.path.display(),
            self.bytes,
            self.token_estimate(),
            BYTES_PER_TOKEN
        );
        if self.seed_template {
            line.push_str(" — tm seed template, no project content");
        }
        if self.excluded {
            line.push_str(" — already in claudeMdExcludes");
        }
        line
    }
}

/// Every memory file above `project_root`, with its exclusion state resolved.
///
/// Why: the one entry point the doctor check and the launch WARN share, so the
/// two can never disagree about which files a session is paying for.
/// What: resolves the `claudeMdExcludes` union from the layers a session in
/// `project_root` merges, then delegates to [`scan_with_excludes`]. `home` and
/// `managed_config_dir` are INJECTED so a test never writes process-global
/// environment state (#5544).
/// Test: `ancestor_claude_md_tests.rs`.
pub fn scan(
    project_root: &Path,
    home: Option<&Path>,
    managed_config_dir: Option<&Path>,
) -> Vec<AncestorMemoryFile> {
    // Canonicalised ONCE, here: the walk below reports canonical paths, and an
    // exclude entry `--fix` wrote from a previous scan must compare equal to
    // them. On macOS a temp or symlinked project reaches the same file under two
    // spellings, and comparing one against the other reports an already-excluded
    // ancestor as an outstanding finding.
    let root = std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let layers = crate::core::claude_md_excludes::settings_layers(&root, home, managed_config_dir);
    let excludes = crate::core::claude_md_excludes::merged_excludes(&layers);
    scan_with_excludes(&root, &excludes)
}

/// [`scan`] against an already-resolved exclude set.
///
/// What: walks `project_root`'s ancestors — its parent first, then upward to the
/// filesystem root — and reports every existing [`MEMORY_FILE_RELATIVE_PATHS`]
/// entry. Files INSIDE `project_root` are never reported: they are the project's
/// own instructions, not an ancestor's. Paths are deduplicated, so a
/// canonicalisation that folds two spellings onto one file yields one row.
/// Test: `no_ancestors_yields_nothing`, `a_seed_ancestor_is_reported_as_a_seed`,
/// `a_project_root_file_is_never_reported`,
/// `an_excluded_ancestor_is_flagged_excluded`.
pub fn scan_with_excludes(
    project_root: &Path,
    excludes: &BTreeSet<String>,
) -> Vec<AncestorMemoryFile> {
    let start = std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut found = Vec::new();
    let mut dir = start.parent();
    while let Some(current) = dir {
        for relative in MEMORY_FILE_RELATIVE_PATHS {
            let path = current.join(relative);
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if !meta.is_file() || !seen.insert(path.clone()) {
                continue;
            }
            found.push(AncestorMemoryFile {
                bytes: meta.len(),
                seed_template: looks_like_seed(&path, meta.len()),
                excluded: crate::core::claude_md_excludes::is_excluded(&path, excludes),
                path,
            });
        }
        dir = current.parent();
    }
    found
}

/// Largest file this will open for the seed shape test.
///
/// Why: the scan runs on every launch, and the shape test is the only read in
/// it. The seed template is under 2 KiB, so anything an order of magnitude
/// larger cannot be one and is reported ⚠️ without being opened.
/// What: 16 KiB.
/// Test: `a_large_ancestor_is_not_opened_or_called_a_seed`.
const MAX_SEED_BYTES: u64 = 16 * 1024;

/// Is the file at `path` tm's seed template?
///
/// What: `false` without reading for anything over [`MAX_SEED_BYTES`] or any
/// file that cannot be read; otherwise
/// [`crate::core::claude_md_seed::is_seed_template`] decides.
/// Test: see [`scan_with_excludes`].
fn looks_like_seed(path: &Path, bytes: u64) -> bool {
    if bytes > MAX_SEED_BYTES {
        return false;
    }
    std::fs::read_to_string(path)
        .map(|text| crate::core::claude_md_seed::is_seed_template(&text))
        .unwrap_or(false)
}

/// Emit one launch-time WARN naming every ancestor memory file, if any (#7673).
///
/// Why: the cost is invisible from inside a session — the PM sees the text but
/// not where it came from — so the one moment it can be surfaced is the launch
/// that composes the instructions. One line per launch, naming every file and
/// both remedies, rather than one line per file.
/// What: no-op when nothing is found, so a healthy machine grows no log noise;
/// otherwise a single `tracing::warn!` with each file's [`AncestorMemoryFile::summary`]
/// and the two remedies. Files the operator has already excluded are not
/// reported — a session does not load them.
/// Test: `warn_is_silent_when_there_are_no_ancestors`,
/// `the_warning_names_both_remedies`.
pub fn warn_ancestors(project_root: &Path, home: Option<&Path>, managed: Option<&Path>) {
    let found = scan(project_root, home, managed);
    let Some(text) = warning_text(&found) else {
        return;
    };
    tracing::warn!("{text}");
}

/// The WARN's body, or `None` when there is nothing to say.
///
/// Why: split from [`warn_ancestors`] so the wording is assertable without
/// capturing a tracing subscriber.
/// What: `None` when every found file is already excluded (or none was found).
/// Test: `warn_is_silent_when_there_are_no_ancestors`,
/// `the_warning_names_both_remedies`,
/// `an_excluded_ancestor_produces_no_warning`.
pub fn warning_text(found: &[AncestorMemoryFile]) -> Option<String> {
    let loaded: Vec<&AncestorMemoryFile> = found.iter().filter(|f| !f.excluded).collect();
    if loaded.is_empty() {
        return None;
    }
    let total: u64 = loaded.iter().map(|f| f.token_estimate()).sum();
    let lines: Vec<String> = loaded.iter().map(|f| f.summary()).collect();
    Some(format!(
        "{} CLAUDE.md file(s) ABOVE this project load into every session here \
         (~{total} tokens per turn, estimated at bytes/{BYTES_PER_TOKEN}): {}. \
         Remedy: delete the file if it is a tm seed template, or add its path to \
         `claudeMdExcludes` in .claude/settings.local.json. `tm doctor` reports \
         them and `tm doctor --fix --yes` applies both remedies.",
        loaded.len(),
        lines.join("; ")
    ))
}

/// The name a pure seed template is renamed to by `tm doctor --fix`.
///
/// Why: the repair must be reversible — the operator can rename it back — and
/// must stop Claude Code loading the file, which is why the `.md` extension
/// moves inward rather than being kept.
/// What: `<path>.stale-seed-<YYYYMMDD>`, e.g.
/// `/Users/ada/CLAUDE.md.stale-seed-20260912`.
/// Test: `the_rename_target_carries_the_date`.
pub fn stale_seed_name(path: &Path, today: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".stale-seed-{today}"));
    path.with_file_name(name)
}

#[cfg(test)]
#[path = "ancestor_claude_md_tests.rs"]
mod tests;
