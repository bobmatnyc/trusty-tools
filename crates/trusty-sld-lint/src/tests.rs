//! Unit + orchestration tests for the SLD linter.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::allowlist;
use crate::catalog;
use crate::checks;
use crate::discover;
use crate::report::{Diagnostic, Severity};
use crate::{run, LintError, LintOptions};

// A single lookup that knows about one target spec containing SPEC-X-01~draft.
fn lookup(path: &str) -> Option<String> {
    (path == "docs/specs/x.md").then(|| "## S {#SPEC-X-01~draft}\n".to_string())
}

// ── report ───────────────────────────────────────────────────────────────────

#[test]
fn report_display() {
    let d = Diagnostic::error("a.md", 12, "ref-path-missing", "boom");
    assert_eq!(d.to_string(), "a.md:12: [error ref-path-missing] boom");
    let file_scoped = Diagnostic::error("a.md", 0, "spec-header", "no owner");
    assert_eq!(
        file_scoped.to_string(),
        "a.md: [error spec-header] no owner"
    );
}

#[test]
fn report_exit_counts_errors_only() {
    let err = Diagnostic::error("a", 1, "c", "m");
    let warn = Diagnostic::warning("a", 1, "c", "m");
    assert_eq!(err.severity, Severity::Error);
    assert_eq!(warn.severity, Severity::Warning);
}

// ── catalog ──────────────────────────────────────────────────────────────────

#[test]
fn catalog_parses_rows() {
    let readme = "| DOC | Spec ID |\n|---|---|\n| DOC-13 | x |\n| DOC-38 | y |\n> note: DOC-99 uncataloged\n";
    let set = catalog::parse_catalog(readme);
    assert!(set.contains(&13));
    assert!(set.contains(&38));
    // The prose note (`>`-prefixed) is NOT a row.
    assert!(!set.contains(&99));
}

#[test]
fn catalog_doc_number_of() {
    assert_eq!(catalog::doc_number_of("# DOC-38 — Title\n"), Some(38));
    assert_eq!(catalog::doc_number_of("# SPEC-INSTALLER-01\n"), None);
}

#[test]
fn catalog_doc_number_ignores_earlier_cross_reference() {
    // A spec_refs frontmatter block, a superseded/redirect banner, or body
    // prose can legitimately mention a DIFFERENT DOC-N before the document's
    // own title heading. The self-label must come from the file's own H1
    // heading, never the first "DOC-" substring in the file.
    let md = "---\nspec_refs:\n  - id: SPEC-CONFORMANCE-03~draft\n    path: docs/specs/intent-conformance.md\n    anchor: SPEC-CONFORMANCE-03~draft\n---\n\n# DOC-38 — Real Title\n\nSee DOC-15 for background.\n";
    assert_eq!(catalog::doc_number_of(md), Some(38));

    // A superseded banner mentioning another DOC-N BEFORE the real title.
    let superseded = "> Superseded by DOC-99, see that spec instead.\n\n# DOC-30 — Old Vision\n";
    assert_eq!(catalog::doc_number_of(superseded), Some(30));
}

// ── checks: references ───────────────────────────────────────────────────────

#[test]
fn checks_reference_resolves() {
    let d = checks::check_reference(
        "src.rs",
        "SPEC-X-01~draft",
        "docs/specs/x.md",
        "SPEC-X-01~draft",
        3,
        &lookup,
    );
    assert!(d.is_empty(), "clean reference should not diagnose: {d:?}");
}

#[test]
fn checks_reference_errors() {
    // anchor != id
    let m = checks::check_reference(
        "s",
        "SPEC-X-01~draft",
        "docs/specs/x.md",
        "SPEC-X-02~draft",
        1,
        &lookup,
    );
    assert!(m.iter().any(|d| d.check == "ref-anchor-mismatch"));
    // traversal
    let t = checks::check_reference(
        "s",
        "SPEC-X-01~draft",
        "../x.md",
        "SPEC-X-01~draft",
        1,
        &lookup,
    );
    assert!(t.iter().any(|d| d.check == "ref-traversal"));
    // absolute path — must be rejected as unsafe, never silently escape via
    // `root.join("/etc/passwd.md")` discarding the repo root.
    let abs = checks::check_reference(
        "s",
        "SPEC-X-01~draft",
        "/etc/passwd.md",
        "SPEC-X-01~draft",
        1,
        &lookup,
    );
    assert!(abs.iter().any(|d| d.check == "ref-traversal"));
    // missing path
    let p = checks::check_reference(
        "s",
        "SPEC-X-01~draft",
        "docs/specs/none.md",
        "SPEC-X-01~draft",
        1,
        &lookup,
    );
    assert!(p.iter().any(|d| d.check == "ref-path-missing"));
    // missing anchor
    let a = checks::check_reference(
        "s",
        "SPEC-Z-09~draft",
        "docs/specs/x.md",
        "SPEC-Z-09~draft",
        1,
        &lookup,
    );
    assert!(a.iter().any(|d| d.check == "ref-anchor-missing"));
}

#[test]
fn checks_reference_revision_drift() {
    // The target now anchors ~v2, but the reference still declares ~v1 — DOC-38
    // §4.4: a conforming resolver MAY still resolve this (advisory, not an
    // error) rather than failing outright.
    fn drifted_lookup(path: &str) -> Option<String> {
        (path == "docs/specs/x.md").then(|| "## S {#SPEC-X-01~v2}\n".to_string())
    }
    let d = checks::check_reference(
        "s",
        "SPEC-X-01~v1",
        "docs/specs/x.md",
        "SPEC-X-01~v1",
        1,
        &drifted_lookup,
    );
    assert!(
        d.iter()
            .any(|x| x.check == "ref-revision-drift" && x.severity == Severity::Warning),
        "expected an advisory ref-revision-drift diagnostic: {d:?}"
    );
    // Drift is advisory only — must never surface as an error.
    assert!(!d.iter().any(|x| x.severity == Severity::Error));
}

#[test]
fn checks_code_file() {
    let src = "//! # Spec References\n//! - [`SPEC-X-01~draft`](docs/specs/x.md#SPEC-X-01~draft)\ncode();";
    assert!(checks::check_code_file("s.rs", src, "rs", &lookup).is_empty());
    // Unknown extension → nothing scanned.
    assert!(checks::check_code_file("s.bin", src, "bin", &lookup).is_empty());
}

#[test]
fn checks_markdown_refs() {
    let md = "---\nspec_refs:\n  - id: SPEC-X-01~draft\n    path: docs/specs/x.md\n    anchor: SPEC-X-01~draft\n---\n# Doc\n";
    assert!(checks::check_markdown_refs("d.md", md, &lookup).is_empty());
}

#[test]
fn checks_markdown_bad_frontmatter() {
    let md = "---\nspec_refs:\n  - \"garbage\"\n---\n# Doc\n";
    let d = checks::check_markdown_refs("d.md", md, &lookup);
    assert!(d.iter().any(|x| x.check == "frontmatter-schema"));
}

// ── checks: spec-document conventions ────────────────────────────────────────

const CLEAN_SPEC: &str = "---\nspec_refs:\n  - id: SPEC-X-01~draft\n    path: docs/specs/x.md\n    anchor: SPEC-X-01~draft\n---\n\n# DOC-7 — X\n\n**Status:** Draft\n**Subsystem:** x\n**Owner:** eng\n**Last-updated:** 2026-01-01\n**Spec ID:** SPEC-X-01~draft\n\n## 1. Section {#SPEC-X-01~draft}\n**ID:** SPEC-X-01~draft\nbody\n";

fn catalog_with7() -> HashSet<u32> {
    [7].into_iter().collect()
}

#[test]
fn checks_spec_doc_opt_in() {
    // The clean opted-in spec passes every §4 check.
    assert!(checks::check_spec_doc("x.md", CLEAN_SPEC, &catalog_with7(), false).is_empty());
}

#[test]
fn checks_spec_doc_strict() {
    // A spec WITHOUT frontmatter is skipped by default but checked under --strict.
    let no_fm = "# DOC-7 — X\n\nbody without header block\n";
    assert!(checks::check_spec_doc("x.md", no_fm, &catalog_with7(), false).is_empty());
    let strict = checks::check_spec_doc("x.md", no_fm, &catalog_with7(), true);
    assert!(strict.iter().any(|d| d.check == "spec-header"));
}

#[test]
fn checks_header_block() {
    let missing_owner = CLEAN_SPEC.replace("**Owner:** eng\n", "");
    let d = checks::check_spec_doc("x.md", &missing_owner, &catalog_with7(), true);
    assert!(d
        .iter()
        .any(|x| x.check == "spec-header" && x.message.contains("Owner")));
}

#[test]
fn checks_header_block_ignores_fenced_example() {
    // The real header block is missing **Owner:**, but a FENCED example quotes
    // `**Owner:** eng` (illustrating the convention, exactly as DOC-38's own
    // body does) — the fenced quote must NOT satisfy the requirement.
    let missing_owner = CLEAN_SPEC.replace("**Owner:** eng\n", "");
    let with_fenced_example = missing_owner.replace("body\n", "body\n\n```\n**Owner:** eng\n```\n");
    let d = checks::check_spec_doc("x.md", &with_fenced_example, &catalog_with7(), true);
    assert!(
        d.iter()
            .any(|x| x.check == "spec-header" && x.message.contains("Owner")),
        "a fenced example must not mask a real missing header field: {d:?}"
    );
}

#[test]
fn checks_catalog_row() {
    // DOC-7 not in an empty catalog → spec-catalog.
    let d = checks::check_spec_doc("x.md", CLEAN_SPEC, &HashSet::new(), true);
    assert!(d.iter().any(|x| x.check == "spec-catalog"));
}

#[test]
fn checks_anchors() {
    // Anchor id disagrees with the section's declared **ID:**.
    let mismatch = CLEAN_SPEC.replace(
        "**ID:** SPEC-X-01~draft\nbody",
        "**ID:** SPEC-X-02~draft\nbody",
    );
    let d = checks::check_spec_doc("x.md", &mismatch, &catalog_with7(), true);
    assert!(d.iter().any(|x| x.check == "anchor-id-mismatch"));

    // A grammar-invalid anchor is flagged.
    let bad = CLEAN_SPEC.replace("{#SPEC-X-01~draft}", "{#SPEC-X-1~draft}");
    let d2 = checks::check_spec_doc("x.md", &bad, &catalog_with7(), true);
    assert!(d2.iter().any(|x| x.check == "spec-id-grammar"));

    // A backticked **ID:** value still matches its anchor (no false mismatch).
    let ticked = CLEAN_SPEC.replace("**ID:** SPEC-X-01~draft", "**ID:** `SPEC-X-01~draft`");
    let d3 = checks::check_spec_doc("x.md", &ticked, &catalog_with7(), true);
    assert!(!d3.iter().any(|x| x.check == "anchor-id-mismatch"));
}

// ── discover + allowlist ─────────────────────────────────────────────────────

#[test]
fn discover_classifies_tests() {
    assert!(discover::is_test_file(Path::new("crates/x/src/tests.rs")));
    assert!(discover::is_test_file(Path::new(
        "crates/x/src/foo_test.rs"
    )));
    assert!(discover::is_test_file(Path::new("crates/x/tests/it.rs")));
    assert!(discover::is_test_file(Path::new("crates/x/benches/b.rs")));
    assert!(!discover::is_test_file(Path::new("crates/x/src/lib.rs")));
}

#[test]
fn allowlist_suppresses() {
    let allow = allowlist::parse("# comment\ndocs/specs/legacy.md\tspec-catalog\n\n");
    let hit = Diagnostic::error("docs/specs/legacy.md", 0, "spec-catalog", "m");
    let miss_check = Diagnostic::error("docs/specs/legacy.md", 0, "spec-header", "m");
    let miss_path = Diagnostic::error("docs/specs/other.md", 0, "spec-catalog", "m");
    assert!(allowlist::suppresses(&allow, &hit));
    assert!(!allowlist::suppresses(&allow, &miss_check));
    assert!(!allowlist::suppresses(&allow, &miss_path));
}

#[test]
fn allowlist_stale_entry_flagged() {
    // An allowlist entry whose (path, check) matches NO diagnostic in the
    // pre-suppression set is stale — the underlying violation was fixed, and
    // the ratchet must fail until the entry is removed.
    let allow = allowlist::parse("docs/specs/fixed.md\tspec-header\n");
    let diagnostics: Vec<Diagnostic> = vec![]; // nothing wrong with fixed.md anymore
    let stale = allowlist::stale_entries(&allow, &diagnostics);
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].check, "allowlist-stale");
    assert!(stale[0].message.contains("docs/specs/fixed.md"));
    assert!(stale[0].message.contains("spec-header"));
}

#[test]
fn allowlist_live_entry_not_flagged() {
    // An allowlist entry that DOES still match a real diagnostic is not stale.
    let allow = allowlist::parse("docs/specs/legacy.md\tspec-header\n");
    let diagnostics = vec![Diagnostic::error(
        "docs/specs/legacy.md",
        0,
        "spec-header",
        "missing field",
    )];
    assert!(allowlist::stale_entries(&allow, &diagnostics).is_empty());
}

// ── safe_read (path-escape hardening, second independent layer) ─────────────

#[test]
fn safe_read_rejects_absolute() {
    let dir = tempfile::tempdir().unwrap();
    // An absolute path must never be read, even though it names a real file on
    // disk (this crate's own Cargo.toml) — PathBuf::join silently discards
    // `root` for an absolute argument, so an unguarded reader would read
    // wherever the declared reference string points.
    let abs = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("Cargo.toml")
        .display()
        .to_string();
    assert_eq!(crate::safe_read(dir.path(), &abs), None);
}

#[test]
fn safe_read_rejects_dotdot_escape() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(crate::safe_read(dir.path(), "../../etc/passwd"), None);
}

#[test]
fn safe_read_resolves_in_tree() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "docs/specs/x.md", "content\n");
    assert_eq!(
        crate::safe_read(dir.path(), "docs/specs/x.md").as_deref(),
        Some("content\n")
    );
}

#[test]
#[cfg(unix)]
fn safe_read_rejects_symlink_escape() {
    // The genuinely independent-layer case: `is_unsafe_path` only ever sees the
    // innocuous string "docs/specs/escape.md" and passes it — the escape is
    // invisible until the path is actually resolved on disk. Only the
    // canonicalize + prefix-check layer in `safe_read` catches a symlink that
    // points outside the scanned root.
    use std::os::unix::fs::symlink;

    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.md"), "TOP SECRET\n").unwrap();

    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("docs/specs")).unwrap();
    symlink(
        outside.path().join("secret.md"),
        root.path().join("docs/specs/escape.md"),
    )
    .unwrap();

    assert_eq!(crate::safe_read(root.path(), "docs/specs/escape.md"), None);
}

// ── run (orchestration, temp tree) ───────────────────────────────────────────

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

#[test]
fn run_clean_tree() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "docs/specs/README.md",
        "| DOC | Spec ID | Title | Subsystem |\n|---|---|---|---|\n| DOC-1 | `SPEC-X-01~draft` | [Foo](./foo.md) | x |\n",
    );
    write(
        root,
        "docs/specs/foo.md",
        "---\nspec_refs:\n  - id: SPEC-X-01~draft\n    path: docs/specs/foo.md\n    anchor: SPEC-X-01~draft\n---\n\n# DOC-1 — Foo\n\n**Status:** Draft\n**Subsystem:** x\n**Owner:** eng\n**Last-updated:** 2026-01-01\n**Spec ID:** SPEC-X-01~draft\n\n## 1. Section {#SPEC-X-01~draft}\n**ID:** SPEC-X-01~draft\nbody\n",
    );
    write(
        root,
        "crates/x/src/lib.rs",
        "//! # Spec References\n//!\n//! - [`SPEC-X-01~draft`](docs/specs/foo.md#SPEC-X-01~draft)\npub fn f() {}\n",
    );

    let report = run(&LintOptions::new(root)).expect("runs");
    assert!(
        report.is_clean(),
        "expected clean, got: {:?}",
        report.diagnostics
    );
    assert_eq!(report.spec_docs, 1);
    assert!(report.code_files >= 1);
}

#[test]
fn run_flags_unresolved_reference() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "docs/specs/README.md",
        "| DOC | Spec ID |\n|---|---|\n",
    );
    write(
        root,
        "crates/x/src/lib.rs",
        "//! # Spec References\n//! - [`SPEC-GONE-01~draft`](docs/specs/gone.md#SPEC-GONE-01~draft)\n",
    );
    let report = run(&LintOptions::new(root)).expect("runs");
    assert!(!report.is_clean());
    assert!(report
        .diagnostics
        .iter()
        .any(|d| d.check == "ref-path-missing"));
}

#[test]
fn run_missing_catalog_errors() {
    let dir = tempfile::tempdir().unwrap();
    let err = run(&LintOptions::new(dir.path())).unwrap_err();
    assert!(matches!(err, LintError::Catalog { .. }));
}

#[test]
fn run_strict_flags_stale_allowlist_entry() {
    // A fully clean, opted-in, cataloged spec — no real spec-header violation.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "docs/specs/README.md",
        "| DOC | Spec ID | Title | Subsystem |\n|---|---|---|---|\n| DOC-1 | `SPEC-X-01~draft` | [Foo](./foo.md) | x |\n",
    );
    write(
        root,
        "docs/specs/foo.md",
        "---\nspec_refs:\n  - id: SPEC-X-01~draft\n    path: docs/specs/foo.md\n    anchor: SPEC-X-01~draft\n---\n\n# DOC-1 — Foo\n\n**Status:** Draft\n**Subsystem:** x\n**Owner:** eng\n**Last-updated:** 2026-01-01\n**Spec ID:** SPEC-X-01~draft\n\n## 1. Section {#SPEC-X-01~draft}\n**ID:** SPEC-X-01~draft\nbody\n",
    );
    // A stale grandfather entry: foo.md has no spec-header violation, so this
    // entry matches nothing and MUST be flagged so it gets removed.
    write(
        root,
        ".sld-lint-allowlist.tsv",
        "docs/specs/foo.md\tspec-header\n",
    );

    let strict_report = run(&LintOptions {
        root: root.to_path_buf(),
        strict: true,
        allowlist_path: root.join(".sld-lint-allowlist.tsv"),
    })
    .expect("runs");
    assert!(
        !strict_report.is_clean(),
        "a stale allowlist entry must fail --strict"
    );
    assert!(
        strict_report
            .diagnostics
            .iter()
            .any(|d| d.check == "allowlist-stale"),
        "expected an allowlist-stale diagnostic: {:?}",
        strict_report.diagnostics
    );

    // Non-strict (default) mode must NOT flag it — spec-header is only checked
    // on opted-in files there, so absence proves nothing about staleness.
    let default_report = run(&LintOptions {
        root: root.to_path_buf(),
        strict: false,
        allowlist_path: root.join(".sld-lint-allowlist.tsv"),
    })
    .expect("runs");
    assert!(
        default_report.is_clean(),
        "default mode must not spuriously flag stale entries: {:?}",
        default_report.diagnostics
    );
}
