//! `trusty-sld-lint` — the Spec-Linked Documentation (DOC-38) linter engine.
//!
//! Why: DOC-38 defines the SLD reference grammar and spec-document conventions;
//! a gate that mechanically enforces them (DOC-38 §10 F1) keeps declared linkage
//! honest and specs well-formed. This crate is the check engine — a thin
//! `anyhow`/clap CLI (`src/main.rs`) drives it. It reuses the ONE SLD grammar
//! from `trusty_common::sld` rather than re-implementing a parser.
//!
//! What: [`run`] discovers the in-scope files, applies the pure [`checks`] (with
//! filesystem lookups for reference targets), suppresses grandfathered
//! [`allowlist`] entries, and returns a [`LintReport`]. Strictness follows the
//! grandfathering model: reference resolution runs everywhere; the full
//! spec-document checks run on opted-in files (frontmatter-bearing) by default,
//! or on every spec under `--strict` (the post-retrofit mode).
//!
//! The crate is a library so each check is unit-testable in isolation; only
//! [`run`] touches the filesystem, and it returns a `thiserror` [`LintError`]
//! (the binary maps it to `anyhow`).
//!
//! Test: `cargo test -p trusty-sld-lint` (unit `mod tests` + the real-tree
//! integration test `tests/lint_real_tree.rs`).

pub mod allowlist;
pub mod catalog;
pub mod checks;
pub mod discover;
pub mod report;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

pub use report::{Diagnostic, Severity};

/// The default grandfather-allowlist filename (repo root).
pub const ALLOWLIST_FILE: &str = ".sld-lint-allowlist.tsv";

/// Inputs to a lint run.
///
/// Why: a single options struct keeps [`run`]'s signature stable as flags are
/// added, and makes a run fully described by its inputs (testable, reproducible).
/// What: the repository `root` to scan, the `strict` toggle (full spec-doc checks
/// on every spec, not just opted-in ones), and the `allowlist_path` of
/// grandfathered exceptions.
/// Test: `tests::run_clean_tree`.
#[derive(Debug, Clone)]
pub struct LintOptions {
    /// Repository root to scan (`docs/specs/**` + `crates/**`).
    pub root: PathBuf,
    /// Apply full spec-document checks to every spec, not only opted-in ones.
    pub strict: bool,
    /// Path to the grandfather-allowlist TSV (may be absent).
    pub allowlist_path: PathBuf,
}

impl LintOptions {
    /// Build options for `root` with defaults (non-strict, root allowlist file).
    ///
    /// Why: the common case is "lint this checkout with the repo's allowlist";
    /// a constructor keeps callers terse.
    /// What: `strict = false`, `allowlist_path = root/.sld-lint-allowlist.tsv`.
    /// Test: `tests::run_clean_tree`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let allowlist_path = root.join(ALLOWLIST_FILE);
        Self {
            root,
            strict: false,
            allowlist_path,
        }
    }
}

/// The outcome of a lint run.
///
/// Why: the CLI needs both the findings (to print) and enough counts to render a
/// one-line summary and choose an exit code; bundling them keeps [`run`] a pure
/// value producer.
/// What: the surviving `diagnostics` (post-allowlist) and the number of spec docs
/// and code files scanned.
/// Test: `tests::run_clean_tree`.
#[derive(Debug, Default)]
pub struct LintReport {
    /// All findings that survived the allowlist, in scan order.
    pub diagnostics: Vec<Diagnostic>,
    /// Number of `docs/specs/**/*.md` documents scanned.
    pub spec_docs: usize,
    /// Number of `crates/**` code files scanned.
    pub code_files: usize,
}

impl LintReport {
    /// Count of `Error`-severity diagnostics (what fails the lint).
    ///
    /// Why: advisories (revision drift) must not fail CI; only errors do.
    /// What: the number of diagnostics whose severity is [`Severity::Error`].
    /// Test: `tests::run_clean_tree`.
    #[must_use]
    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
    }

    /// True when the run has no errors (advisories are allowed).
    ///
    /// Why: the CLI exits 0 exactly when the tree is clean of hard violations.
    /// What: `error_count() == 0`.
    /// Test: `tests::run_clean_tree`.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.error_count() == 0
    }
}

/// Errors that abort a lint run before any findings are produced.
///
/// Why: a library surfaces failures as a typed `thiserror` enum (repo rule); the
/// only fatal condition is being unable to read the spec catalog, without which
/// the catalog-row check cannot run.
/// What: `Catalog` wraps the failure to read `docs/specs/README.md`.
/// Test: `tests::run_missing_catalog_errors`.
#[derive(Debug, thiserror::Error)]
pub enum LintError {
    /// The spec catalog (`docs/specs/README.md`) could not be read.
    #[error("cannot read spec catalog `{path}`: {source}")]
    Catalog {
        /// The path that failed to read.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
}

/// Run the SLD linter over a repository checkout.
///
/// Why: this is the single entry point the CLI and the integration test call —
/// it wires discovery, the pure checks, filesystem target lookups, and the
/// allowlist into one report, so all I/O lives here and the checks stay pure.
/// What: parses the spec catalog; discovers spec docs and code files; resolves
/// each declared reference against on-disk targets (revision-tolerant); applies
/// the spec-document checks per the strictness/grandfathering model; and filters
/// grandfathered `(path, check)` pairs. Files that cannot be read as UTF-8 are
/// skipped (a read failure is not an SLD violation). Returns the [`LintReport`]
/// or a [`LintError`] when the catalog is unreadable.
///
/// **Ratchet enforcement is `--strict`-only.** Under `--strict`, every spec
/// gets the full spec-document checks (§4), so an allowlist entry that matches
/// no diagnostic there really does mean the underlying violation was fixed —
/// [`allowlist::stale_entries`] then fails the run until the entry is removed.
/// In DEFAULT (non-strict) mode, spec-document checks apply only to opted-in
/// files, so an allowlisted file's *absence* from the diagnostics proves
/// nothing (it may simply not have been checked yet) — stale-entry enforcement
/// is skipped there to avoid a false "already fixed" failure on every entry.
/// Test: `tests::run_clean_tree`, `tests::run_strict_flags_stale_allowlist_entry`,
/// and `tests/lint_real_tree.rs` (real tree, both modes).
pub fn run(opts: &LintOptions) -> Result<LintReport, LintError> {
    let root = opts.root.as_path();

    let catalog_path = root.join("docs/specs/README.md");
    let readme = std::fs::read_to_string(&catalog_path).map_err(|source| LintError::Catalog {
        path: catalog_path.display().to_string(),
        source,
    })?;
    let catalog = catalog::parse_catalog(&readme);

    let allow = std::fs::read_to_string(&opts.allowlist_path)
        .map(|s| allowlist::parse(&s))
        .unwrap_or_default();

    let discovered = discover::discover(root);
    let lookup = |path: &str| safe_read(root, path);

    let mut report = LintReport {
        spec_docs: discovered.spec_docs.len(),
        code_files: discovered.code_files.len(),
        ..LintReport::default()
    };

    for rel in &discovered.spec_docs {
        let Some(content) = read_rel(root, rel) else {
            continue;
        };
        let path = rel.to_string_lossy();
        report
            .diagnostics
            .extend(checks::check_markdown_refs(&path, &content, &lookup));
        report.diagnostics.extend(checks::check_spec_doc(
            &path,
            &content,
            &catalog,
            opts.strict,
        ));
    }

    for rel in &discovered.code_files {
        let Some(content) = read_rel(root, rel) else {
            continue;
        };
        let ext = rel.extension().and_then(|e| e.to_str()).unwrap_or("");
        let path = rel.to_string_lossy();
        report
            .diagnostics
            .extend(checks::check_code_file(&path, &content, ext, &lookup));
    }

    if opts.strict {
        // Ratchet enforcement: see this function's doc comment for why this is
        // `--strict`-only.
        report.diagnostics.extend(allowlist::stale_entries(
            &allow,
            &report.diagnostics.clone(),
        ));
    }
    // `allowlist-stale` diagnostics are NEVER themselves suppressible — an
    // allowlist entry naming (ALLOWLIST_FILE, "allowlist-stale") could
    // otherwise neutralize the ratchet's own enforcement mechanism.
    report
        .diagnostics
        .retain(|d| d.check == "allowlist-stale" || !allowlist::suppresses(&allow, d));
    Ok(report)
}

/// Read a declared reference's target file, refusing to escape `root`.
///
/// Why: `path` here comes from a **declared reference** (frontmatter `path:` or
/// an inline `# Spec References` entry) — untrusted document content, not a
/// path this crate itself discovered by walking the tree. `checks::is_unsafe_path`
/// already rejects `..` and absolute paths at the grammar layer before a
/// reference reaches this closure, but that guard lives in a different crate and
/// a future caller could bypass or forget it; the actual filesystem read is the
/// last line of defence and must be independently safe. `PathBuf::join` silently
/// *discards* `root` when `path` is absolute, so a check that only inspects the
/// unjoined string is not enough — the read site itself must verify the
/// **resolved, canonical** path is still rooted under `root` before opening it
/// (defence in depth: two independent layers, not one guard trusted twice).
/// What: rejects `path` outright when [`trusty_common::sld::is_unsafe_path`]
/// flags it; otherwise joins it onto `root`, canonicalizes both the join and
/// `root` (resolving `..`/symlinks), and returns the file's contents only when
/// the canonical join still starts with the canonical `root`. Any failure
/// (unsafe path, missing file, symlink escape, non-UTF-8) yields `None` rather
/// than an error — a lookup failure is `ref-path-missing`, not a crash.
/// Test: `tests::safe_read_rejects_absolute`, `tests::safe_read_rejects_dotdot_escape`,
/// `tests::safe_read_rejects_symlink_escape`, `tests::safe_read_resolves_in_tree`.
fn safe_read(root: &Path, path: &str) -> Option<String> {
    if trusty_common::sld::is_unsafe_path(path) {
        return None;
    }
    let root_canon = root.canonicalize().ok()?;
    let candidate_canon = root.join(path).canonicalize().ok()?;
    if !candidate_canon.starts_with(&root_canon) {
        return None;
    }
    std::fs::read_to_string(candidate_canon).ok()
}

/// Read a repo-relative file as UTF-8, or `None` on any failure.
///
/// Why: a discovered source file that cannot be read (e.g. non-UTF-8) is not an
/// SLD violation — the run skips it rather than aborting. Unlike [`safe_read`],
/// `rel` here is always a path this crate's own [`discover::discover`] walk
/// produced (never attacker-controlled document content), so no traversal guard
/// is needed at this call site.
/// What: `read_to_string(root/rel).ok()`.
/// Test: covered by `tests::run_clean_tree`.
fn read_rel(root: &Path, rel: &Path) -> Option<String> {
    std::fs::read_to_string(root.join(rel)).ok()
}
