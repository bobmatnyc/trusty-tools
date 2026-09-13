//! Memory files ABOVE the project root, and what they cost (#7673).
//!
//! Why: Claude Code loads every `CLAUDE.md` from the session's cwd up to the
//! filesystem root. A file at `$HOME` — or at `~/work`, or at `/` — is therefore
//! prepended to every turn of every agent in every project beneath it, and
//! nothing in the harness said so. On 2026-09-12 the file found at `$HOME` was
//! tm's own seed template: boilerplate with no project content in it at all,
//! paid for on every turn for as long as it sat there.
//!
//! What: [`resolve_project_root`] finds the project a directory belongs to;
//! [`scan`] walks the ancestors ABOVE that root and reports every `CLAUDE.md`,
//! `CLAUDE.local.md` and `.claude/CLAUDE.md` it finds, with the file's size, a
//! token estimate, whether it is tm's seed template and whether the operator has
//! already excluded it via `claudeMdExcludes`. [`warn_ancestors`] is the
//! launch-path WARN and [`scan_notice`] the text every user-facing caller
//! prints; the doctor check and its `--fix` live in
//! `daemon::doctor_ancestor_claude_md` and
//! [`crate::core::ancestor_claude_md_repair`].
//!
//! Read-only, and it stops at the filesystem root. It opens a file only to
//! shape-test it, and only when that file is small enough to be a seed. It FAILS
//! CLOSED: a start directory that does not resolve, or an ancestor it cannot
//! stat, is an error — never an empty scan, which would read as "nothing here".
//! Test: `ancestor_claude_md_tests.rs`.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::core::harness_root::HARNESS_DIR;
use crate::core::instruction_pipeline::CLAUDE_MD_STUB;

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
    /// What: `<path> (<bytes> B, ~<tokens> tokens at bytes/4)` plus ` — tm seed
    /// template` or ` — already in claudeMdExcludes` where those apply.
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

/// Every memory file above the project `dir` belongs to, exclusions resolved.
///
/// Why: the one entry point every caller shares — `tm doctor`, `tm doctor
/// --fix`, the launch WARN and `tm session instructions` — so no two of them
/// can disagree about which files a session is paying for.
/// What: resolves `dir` through [`resolve_project_root`], reads the
/// `claudeMdExcludes` union from the layers a session at that root merges, then
/// delegates to [`scan_with_excludes`]. `home` and `managed_config_dir` are
/// INJECTED so a test never writes process-global environment state (#5544).
/// Errors from either half propagate; see the module header.
/// Test: `ancestor_claude_md_tests.rs`.
pub fn scan(
    dir: &Path,
    home: Option<&Path>,
    managed_config_dir: Option<&Path>,
) -> io::Result<Vec<AncestorMemoryFile>> {
    let root = resolve_project_root(dir, home)?;
    let layers = crate::core::claude_md_excludes::settings_layers(&root, home, managed_config_dir);
    let excludes = crate::core::claude_md_excludes::merged_excludes(&layers);
    scan_with_excludes(&root, &excludes)
}

/// The root of the project `dir` belongs to: the NEAREST project boundary.
///
/// Why (#7673 rounds 2 and 3): a caller may hand this any subdirectory. Walking
/// from the subdirectory itself reported the project's own root `CLAUDE.md` as
/// a stray (round 2). Letting an enclosing git toplevel win unconditionally
/// resolved a registered, non-git project under a dotfiles `$HOME` repo to
/// `$HOME`, hiding `$HOME/CLAUDE.md` — the incident this module exists for
/// (round 3). Distance decides, never the kind of boundary.
/// What: canonicalizes `dir`, then walks its ancestors INCLUSIVE and returns the
/// first holding a `.git` entry (a checkout, a linked worktree, a submodule) or
/// a `.trusty-mpm` directory (a registered project). The `home` directory is
/// never a boundary: `~/.trusty-mpm` is tm's global state directory, not a
/// project marker, and a dotfiles `.git` at `$HOME` makes every directory under
/// it "inside a repo" — either would resolve a project-less directory to
/// `$HOME` and hide `$HOME/CLAUDE.md` again. With no boundary, `dir` itself.
/// Errors when `dir` does not resolve or a probe fails for any reason other
/// than absence.
/// Test: `a_marker_project_under_a_dotfiles_home_reports_the_home_claude_md`,
/// `scan_does_not_report_the_git_roots_own_claude_md_for_a_nested_project_root`,
/// `a_marker_project_inside_two_repos_reports_both_repo_roots`,
/// `a_symlinked_start_resolves_like_its_target`,
/// `a_missing_start_directory_is_an_error_not_an_empty_scan`,
/// `the_home_directory_is_never_a_project_boundary`.
pub fn resolve_project_root(dir: &Path, home: Option<&Path>) -> io::Result<PathBuf> {
    let start = std::fs::canonicalize(dir).map_err(|err| context(err, "resolve", dir))?;
    let home = home.map(|h| std::fs::canonicalize(h).unwrap_or_else(|_| h.to_path_buf()));
    for candidate in start.ancestors() {
        if home.as_deref() == Some(candidate) {
            continue;
        }
        if is_project_boundary(candidate)? {
            return Ok(candidate.to_path_buf());
        }
    }
    Ok(start)
}

/// Does `dir` hold a `.git` entry or a `.trusty-mpm` directory?
///
/// Test: see [`resolve_project_root`].
fn is_project_boundary(dir: &Path) -> io::Result<bool> {
    let git = dir.join(".git");
    if git.try_exists().map_err(|err| context(err, "stat", &git))? {
        return Ok(true);
    }
    let marker = dir.join(HARNESS_DIR);
    match std::fs::metadata(&marker) {
        Ok(meta) => Ok(meta.is_dir()),
        Err(err) if is_absent(&err) => Ok(false),
        Err(err) => Err(context(err, "stat", &marker)),
    }
}

/// [`scan`] against an already-resolved root and exclude set.
///
/// What: walks `project_root`'s ancestors — its parent first, then upward to the
/// filesystem root — and reports every existing [`MEMORY_FILE_RELATIVE_PATHS`]
/// entry. Files INSIDE `project_root` are never reported: they are the project's
/// own instructions, not an ancestor's. A candidate that is absent is skipped;
/// one that cannot be stat'ed for any other reason is an error, because
/// skipping it could hide exactly the file this scan exists to find.
/// Test: `no_ancestors_yields_nothing`, `a_seed_ancestor_is_reported_as_a_seed`,
/// `a_project_root_file_is_never_reported`,
/// `an_excluded_ancestor_is_flagged_excluded`,
/// `an_unreadable_ancestor_is_an_error_not_an_empty_scan`.
pub fn scan_with_excludes(
    project_root: &Path,
    excludes: &BTreeSet<String>,
) -> io::Result<Vec<AncestorMemoryFile>> {
    let start =
        std::fs::canonicalize(project_root).map_err(|err| context(err, "resolve", project_root))?;
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut found = Vec::new();
    for current in start.ancestors().skip(1) {
        for relative in MEMORY_FILE_RELATIVE_PATHS {
            let path = current.join(relative);
            let meta = match std::fs::metadata(&path) {
                Ok(meta) => meta,
                Err(err) if is_absent(&err) => continue,
                Err(err) => return Err(context(err, "stat", &path)),
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
    }
    Ok(found)
}

/// Is `err` "nothing there" rather than "could not look"?
///
/// What: `NotFound`, or `NotADirectory` — `.claude/CLAUDE.md` under a `.claude`
/// that is a plain file.
fn is_absent(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// `err`, with the operation and path it concerns named in the message.
fn context(err: io::Error, op: &str, path: &Path) -> io::Error {
    io::Error::new(err.kind(), format!("cannot {op} {}: {err}", path.display()))
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
/// file that cannot be read; otherwise [`is_seed_template`] decides. An
/// unreadable file is still REPORTED — only the destructive seed verdict is
/// withheld.
/// Test: see [`scan_with_excludes`].
fn looks_like_seed(path: &Path, bytes: u64) -> bool {
    if bytes > MAX_SEED_BYTES {
        return false;
    }
    std::fs::read_to_string(path)
        .map(|text| is_seed_template(&text))
        .unwrap_or(false)
}

/// Is `content` tm's seed template with nothing of the operator's added?
///
/// Why: the doctor check's `Fail` verdict and `--fix`'s rename both hinge on
/// "this file is pure cost". Only a file whose prose is the stub's can carry
/// that verdict — anything else is content someone may want.
/// What: compares both sides with HTML comments removed (Claude Code strips
/// them on load), each line trimmed and blank lines dropped.
/// Test: `a_seed_ancestor_is_reported_as_a_seed`,
/// `a_content_ancestor_is_not_a_seed`.
fn is_seed_template(content: &str) -> bool {
    fn normalized(content: &str) -> String {
        let mut stripped = String::with_capacity(content.len());
        let mut rest = content;
        while let Some(start) = rest.find("<!--") {
            stripped.push_str(&rest[..start]);
            rest = rest[start..]
                .find("-->")
                .map_or("", |end| &rest[start + end + 3..]);
        }
        stripped.push_str(rest);
        let lines: Vec<&str> = stripped
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        lines.join("\n")
    }
    normalized(content) == normalized(CLAUDE_MD_STUB)
}

/// The text a user-facing caller prints for one scan, or `None` for silence.
///
/// Why: the launch WARN and `tm session instructions` must say the same thing,
/// and both must show a FAILED scan — swallowing the error into silence would
/// read exactly like a clean machine.
/// What: [`warning_text`] for a successful scan; for an error, a line naming
/// `dir`, the error, and that the ancestors went unchecked.
/// Test: `a_failed_scan_is_announced_not_silent`,
/// `the_warning_names_both_remedies`.
pub fn scan_notice(dir: &Path, scanned: &io::Result<Vec<AncestorMemoryFile>>) -> Option<String> {
    match scanned {
        Ok(found) => warning_text(found),
        Err(err) => Some(format!(
            "could not check for CLAUDE.md files above {}: {err}. Memory files an \
             ancestor directory loads into this session were NOT checked; `tm doctor` \
             reports the same failure.",
            dir.display()
        )),
    }
}

/// Emit one launch-time WARN naming every ancestor memory file, if any (#7673).
///
/// Why: the cost is invisible from inside a session — the PM sees the text but
/// not where it came from — so the one moment it can be surfaced is the launch
/// that composes the instructions.
/// What: [`scan`] then [`scan_notice`]; silent when nothing loads, otherwise one
/// `tracing::warn!`. Never fails the launch.
/// Test: `a_failed_scan_is_announced_not_silent`,
/// `warn_is_silent_when_there_are_no_ancestors`.
pub fn warn_ancestors(dir: &Path, home: Option<&Path>, managed: Option<&Path>) {
    if let Some(text) = scan_notice(dir, &scan(dir, home, managed)) {
        tracing::warn!("{text}");
    }
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
