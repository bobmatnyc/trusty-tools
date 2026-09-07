//! Deterministic, reversible legacy import from `.claude/` into `.trusty-code/`
//! (#5426, epic #2892).
//!
//! Why: switching discovery to `.trusty-code/` would strand every project whose
//! agents and skills live under `.claude/`. An import has to be observable
//! before it runs (a plan), it must never overwrite a file the project author
//! wrote, and it must be undoable — so it reports exactly which files it
//! created, and nothing else.
//!
//! What: [`plan_import`] walks `.claude/agents/`, `.claude/skills/`, and
//! `.claude/settings.json` and produces a SORTED [`ImportPlan`] naming, for each
//! source file, the target under `.trusty-code/` and the [`ImportAction`] that
//! would be taken. [`apply_import`] executes exactly that plan and returns an
//! [`ImportReport`] listing the files it created. Determinism is what makes the
//! dry run trustworthy: the same tree always yields the same plan in the same
//! order, so `--dry-run` output and the real run cannot disagree.
//!
//! Four refusals, all producing [`ImportAction::Refuse`] rather than an error
//! that aborts the whole import:
//!
//! - the target already exists (never overwrite a user-authored file);
//! - the source is, or reaches through, a symlink out of `.claude/`;
//! - the source carries the executable bit (an imported executable would be
//!   trusted by every later `.trusty-code/` consumer);
//! - a `settings.json` whose JSON carries a secret-bearing key
//!   ([`super::find_secret_key`]).
//!
//! Test: `paths::import_tests::*`.

use std::path::{Path, PathBuf};

use super::{CLAUDE_COMPAT_DIRNAME, SETTINGS_FILENAME, check_native_write_target, native_child};

/// The `.claude/` subtrees an import considers, in plan order.
///
/// Why: agents and skills are the authored content #5426 must carry across;
/// plugins are third-party trees whose provenance this import cannot vouch for,
/// so they are deliberately NOT copied — a plugin stays discoverable in place
/// through the compatibility search root.
/// What: `["agents", "skills"]`.
/// Test: `paths::import_tests::plan_is_sorted_and_deterministic`.
pub const IMPORTED_SUBTREES: &[&str] = &["agents", "skills"];

/// What an import would do with one source file.
///
/// Why: a plan that only listed copies could not explain a file's absence from
/// the result, which is the question an operator asks first.
/// What: [`ImportAction::Copy`] or [`ImportAction::Refuse`] with a
/// human-readable reason.
/// Test: `paths::import_tests::existing_target_is_never_overwritten`,
/// `paths::import_tests::executable_source_is_refused`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportAction {
    /// The file will be copied to its target.
    Copy,
    /// The file will be skipped; the string says why.
    Refuse(String),
}

/// One source file and the target it maps to.
///
/// Why: the unit both the dry-run listing and the applied report are built from,
/// so the two can never describe different work.
/// What: absolute `from` under `.claude/`, absolute `to` under `.trusty-code/`,
/// and the [`ImportAction`].
/// Test: `paths::import_tests::plan_is_sorted_and_deterministic`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportEntry {
    /// The source file under `<project>/.claude/`.
    pub from: PathBuf,
    /// The target under `<project>/.trusty-code/`.
    pub to: PathBuf,
    /// What the import would do with it.
    pub action: ImportAction,
}

/// Everything an import would do, in a stable order.
///
/// Why: producing the plan and executing it are separate steps precisely so the
/// operator can read the first before authorising the second.
/// What: entries sorted by target path.
/// Test: `paths::import_tests::plan_is_sorted_and_deterministic`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportPlan {
    /// The per-file decisions, sorted by [`ImportEntry::to`].
    pub entries: Vec<ImportEntry>,
}

impl ImportPlan {
    /// The entries that would actually be copied.
    ///
    /// Why: the count an operator wants before confirming, without re-filtering
    /// at each call site.
    /// What: entries whose action is [`ImportAction::Copy`].
    /// Test: `paths::import_tests::plan_is_sorted_and_deterministic`.
    pub fn to_copy(&self) -> impl Iterator<Item = &ImportEntry> {
        self.entries
            .iter()
            .filter(|e| e.action == ImportAction::Copy)
    }
}

/// What an applied import actually created, and what it declined.
///
/// Why: reversibility is a property of the REPORT, not of a flag — deleting
/// exactly the paths in `created` restores the tree, and nothing else was
/// touched.
/// What: `created` holds the files written (sorted); `refused` pairs each
/// skipped source with its reason.
/// Test: `paths::import_tests::apply_creates_only_the_planned_files`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Files this import created, sorted — delete exactly these to undo it.
    pub created: Vec<PathBuf>,
    /// Sources not imported, each with the reason.
    pub refused: Vec<(PathBuf, String)>,
}

/// Build the import plan for a project root.
///
/// Why: the observable half of the import. It touches nothing, so it is safe to
/// run in a diagnostic.
/// What: walks each of [`IMPORTED_SUBTREES`] under `<root>/.claude/`, plus
/// `.claude/settings.json`, mapping every regular file to the same relative path
/// under `<root>/.trusty-code/` and classifying it per the module docs. Entries
/// are sorted by target path so the plan is a function of the tree alone. A
/// missing `.claude/` yields an empty plan, never an error.
/// Test: `paths::import_tests::plan_is_sorted_and_deterministic`,
/// `paths::import_tests::missing_claude_dir_yields_empty_plan`,
/// `paths::import_tests::symlink_escaping_claude_is_refused`.
pub fn plan_import(project_root: &Path) -> ImportPlan {
    let claude_root = project_root.join(CLAUDE_COMPAT_DIRNAME);
    let mut entries = Vec::new();

    for subtree in IMPORTED_SUBTREES {
        let dir = claude_root.join(subtree);
        let mut files = Vec::new();
        collect_files(&dir, &dir, subtree, &mut files);
        for (relative, from) in files {
            entries.push(classify(project_root, &claude_root, &relative, &from));
        }
    }

    let settings = claude_root.join(SETTINGS_FILENAME);
    if settings.is_file() {
        entries.push(classify(
            project_root,
            &claude_root,
            SETTINGS_FILENAME,
            &settings,
        ));
    }

    entries.sort_by(|a, b| a.to.cmp(&b.to));
    ImportPlan { entries }
}

/// Recursively collect the regular files under `dir`, relative to `base`.
///
/// Why: the plan must cover a skill's whole directory (`SKILL.md` plus its
/// `references/`), not just its top level.
/// What: appends `(relative_path_with_prefix, absolute_path)` pairs. A directory
/// that cannot be read is skipped and logged at `warn` with the path tried —
/// fail open, since one unreadable subtree must not abort the import of the
/// rest. Symlinked DIRECTORIES are not descended into; a symlinked file is
/// collected and refused later by [`classify`], where the reason can be
/// reported.
/// Test: `paths::import_tests::plan_is_sorted_and_deterministic`,
/// `paths::import_tests::unreadable_subtree_is_skipped_with_a_warning`.
fn collect_files(base: &Path, dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) {
    if !dir.is_dir() {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            // #5426: fail open — an unreadable subtree loses its files, not the
            // whole import, and says so.
            tracing::warn!(
                path = %dir.display(),
                error = %e,
                "could not read a .claude subtree while planning the trusty-code \
                 import; skipping it and continuing with the rest"
            );
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            collect_files(base, &path, prefix, out);
        } else if let Ok(rel) = path.strip_prefix(base) {
            out.push((format!("{prefix}/{}", rel.display()), path.clone()));
        }
    }
}

/// Decide what to do with one source file.
///
/// Why: every refusal rule lives here so the plan and the applied run cannot
/// diverge on a judgement call.
/// What: applies, in order, the write-target guard (which catches a symlinked
/// `.trusty-code/` subdirectory pointing out of the project), the
/// symlink-escape check on the SOURCE, the existing-target check, the
/// executable-bit check, and — for `settings.json` — the secret-key check.
/// Test: `paths::import_tests::existing_target_is_never_overwritten`,
/// `paths::import_tests::executable_source_is_refused`,
/// `paths::import_tests::symlink_escaping_claude_is_refused`,
/// `paths::import_tests::secret_bearing_settings_is_refused`.
fn classify(project_root: &Path, claude_root: &Path, relative: &str, from: &Path) -> ImportEntry {
    let to = native_child(project_root, relative);
    let refuse = |reason: String| ImportEntry {
        from: from.to_path_buf(),
        to: to.clone(),
        action: ImportAction::Refuse(reason),
    };

    if let Err(e) = check_native_write_target(project_root, &to) {
        return refuse(e.to_string());
    }
    if !source_stays_inside(claude_root, from) {
        return refuse(format!(
            "source resolves outside {} — refusing to import through a symlink",
            claude_root.display()
        ));
    }
    if to.exists() {
        return refuse(format!(
            "{} already exists — an import never overwrites a user-authored file",
            to.display()
        ));
    }
    if is_executable(from) {
        return refuse(
            "source carries the executable bit — refusing to import an executable \
             into trusty-code's own configuration tree"
                .to_string(),
        );
    }
    if relative == SETTINGS_FILENAME
        && let Some(key) = settings_secret_key(from)
    {
        return refuse(format!(
            "settings carry a secret-bearing key `{key}` — remove it, or set it \
             in the environment, before importing"
        ));
    }

    ImportEntry {
        from: from.to_path_buf(),
        to,
        action: ImportAction::Copy,
    }
}

/// Whether a source file really lives inside `.claude/`.
///
/// Why: `.claude/agents/leak.md -> ~/.ssh/id_rsa` would otherwise be copied into
/// a tree the project commits.
/// What: canonicalises both and tests containment; a source that cannot be
/// canonicalised (a dangling symlink) is treated as escaping.
/// Test: `paths::import_tests::symlink_escaping_claude_is_refused`.
fn source_stays_inside(claude_root: &Path, from: &Path) -> bool {
    match (claude_root.canonicalize(), from.canonicalize()) {
        (Ok(root), Ok(file)) => file.starts_with(&root),
        _ => false,
    }
}

/// Whether a file has any executable bit set.
///
/// Why: an imported executable would be trusted by every later consumer of
/// `.trusty-code/`, which is exactly the provenance this import cannot vouch
/// for.
/// What: on Unix, any of `0o111`. On other platforms, `false` — there is no
/// executable bit to inspect, and the extension-based equivalent would be a
/// different rule wearing the same name.
/// Test: `paths::import_tests::executable_source_is_refused`.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// See the Unix variant — non-Unix targets expose no executable bit.
#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    false
}

/// The first secret-bearing key in a settings file, if any.
///
/// Why: a settings file that will not parse is not evidence that it is clean, so
/// an unparseable file is refused too rather than waved through.
/// What: parses the file as JSON and delegates to [`super::find_secret_key`]; an
/// I/O or parse failure yields a synthetic reason naming the problem, logged at
/// `warn` with the path tried.
/// Test: `paths::import_tests::secret_bearing_settings_is_refused`,
/// `paths::import_tests::unparseable_settings_is_refused`.
fn settings_secret_key(path: &Path) -> Option<String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "could not read .claude/settings.json while planning the import; \
                 refusing to import it rather than assuming it holds no secrets"
            );
            return Some("<unreadable>".to_string());
        }
    };
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(value) => super::find_secret_key(&value),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "could not parse .claude/settings.json while planning the import; \
                 refusing to import it rather than assuming it holds no secrets"
            );
            Some("<unparseable>".to_string())
        }
    }
}

/// Execute a plan, creating only the files it marked [`ImportAction::Copy`].
///
/// Why: taking the plan as an ARGUMENT (rather than recomputing it) is what
/// makes `--dry-run` meaningful — the operator authorises the exact list they
/// read.
/// What: creates each target's parent directory and copies the file, in plan
/// order. Returns the [`ImportReport`]. A copy that fails mid-run is reported as
/// a refusal with the I/O error and does not abort the remaining entries, so one
/// bad file cannot leave the import half-done with no record of what landed.
/// Test: `paths::import_tests::apply_creates_only_the_planned_files`,
/// `paths::import_tests::apply_is_idempotent`.
pub fn apply_import(plan: &ImportPlan) -> ImportReport {
    let mut report = ImportReport::default();
    for entry in &plan.entries {
        match &entry.action {
            ImportAction::Refuse(reason) => {
                report.refused.push((entry.from.clone(), reason.clone()));
            }
            ImportAction::Copy => match copy_one(&entry.from, &entry.to) {
                Ok(()) => report.created.push(entry.to.clone()),
                Err(e) => report
                    .refused
                    .push((entry.from.clone(), format!("copy failed: {e}"))),
            },
        }
    }
    report.created.sort();
    report
}

/// Copy one file, creating its parent directory.
///
/// Why: a one-line helper keeps [`apply_import`]'s error handling readable.
/// What: `create_dir_all` on the parent, then `std::fs::copy`.
/// Test: `paths::import_tests::apply_creates_only_the_planned_files`.
fn copy_one(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(from, to)?;
    Ok(())
}

#[cfg(test)]
#[path = "import_tests.rs"]
mod import_tests;
