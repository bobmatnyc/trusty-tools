//! Unit + orchestration tests for the SLD linter.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::allowlist;
use crate::catalog;
use crate::checks;
use crate::discover;
use crate::gap::{self, CodeUnit};
use crate::report::{Diagnostic, Severity};
use crate::{run, LintError, LintOptions};
use trusty_common::sld::{syntax_for_extension, Reference};

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

#[test]
fn catalog_doc_number_bold_self_label_convention() {
    // DOC-32 (tool-output-interception-seam.md) and DOC-33
    // (tm-meta-harness-logging.md) give the H1 a plain title with no DOC-N and
    // instead self-label on a `**DOC-N** | Status: ...` lead line just below
    // it (verbatim structure of both real files). The H1-only fix regressed
    // this: doc_number_of returned None for both, silently turning off the
    // catalog-row check for them. The fallback must resolve both correctly.
    let doc33 = "# tm Meta-Harness Logging — Per-Delegation Observability, Verbosity CLI, and Log Pruning\n\n**DOC-33** | Status: `Draft` | Date: 2026-07-03\n\n**Status:** Draft\n";
    assert_eq!(catalog::doc_number_of(doc33), Some(33));

    let doc32 = "# Live Tool-Output Interception Seam for Native `tm` Sessions\n\n**DOC-32** | Status: `Draft` | Date: 2026-07-03\n\n**Status:** Draft\n";
    assert_eq!(catalog::doc_number_of(doc32), Some(32));
}

#[test]
fn catalog_doc_number_bold_label_ignores_bare_cross_reference() {
    // A bold MENTION of another DOC-N (no trailing `| Status:` marker) ahead
    // of the real self-label line must never win — only the `**DOC-N** | ...`
    // shape counts as a self-label.
    let md = "# Some Title\n\n**DOC-15** is related background reading.\n\n**DOC-33** | Status: `Draft` | Date: 2026-07-03\n";
    assert_eq!(catalog::doc_number_of(md), Some(33));

    // Also: a mid-line bold mention (not at the start of the line) must not
    // match at all, regardless of position.
    let mid_line = "# Some Title\n\nSee **DOC-15** for context.\n\n**DOC-33** | Status: `Draft`\n";
    assert_eq!(catalog::doc_number_of(mid_line), Some(33));
}

#[test]
fn catalog_doc_number_none_when_truly_unlabeled() {
    // SPEC-INSTALLER-01.md: self-labels via `**Specification ID:**`, never
    // `DOC-N` in any form, anywhere near the top.
    let spec_installer = "# SPEC-INSTALLER-01: trusty-installer Rename & Interactive Installer/Upgrader\n\n**Specification ID:** SPEC-INSTALLER-01  \n**Status:** Draft  \n";
    assert_eq!(catalog::doc_number_of(spec_installer), None);

    // trusty-memory-chat-session-manager.md: uses a non-DOC-N `Spec ID`.
    let chat_mgr = "# trusty-memory as a Dedicated Chat Session Manager\n\n**Status**: DRAFT / Proposed  \n**Spec ID**: `spec-001-chat-session-manager`  \n";
    assert_eq!(catalog::doc_number_of(chat_mgr), None);
}

#[test]
fn catalog_doc_number_detects_doc28_self_label_despite_collision_note() {
    // mpm-cutover-resume-native-optimization.md is NOT actually unlabeled —
    // on disk it carries the identical `**DOC-28** | Status: ...` lead line
    // as DOC-32/DOC-33 (verified against the real file). It self-labels DOC-28,
    // which COLLIDES with the canonical DOC-28 (trusty-mpm-self-awareness.md) —
    // a pre-existing, documented condition (docs/specs/README.md's "DOC-28
    // self-label collision" catalog note; DOC-38 §4.1 follow-up F3), not an
    // "unlabeled" file. Resolving Some(28) here is the textually honest
    // self-label read and is inert for check_catalog_row: 28 IS in the
    // catalog set (the row just points at a different file), so no new
    // diagnostic fires — verified by `bash scripts/check_sld.sh --strict`
    // staying 0/0 with this change in place.
    let mpm_cutover = "# trusty-mpm Cutover: Resume Bridge + Native Optimization\n\n**DOC-28** | Status: `Draft` | Date: 2026-06-26\n\n## Summary\n";
    assert_eq!(catalog::doc_number_of(mpm_cutover), Some(28));
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

// ── gap report ───────────────────────────────────────────────────────────────

fn make_ref(id: &str, path: &str, anchor: &str, line: usize) -> Reference {
    Reference {
        id: id.to_string(),
        path: path.to_string(),
        anchor: anchor.to_string(),
        line,
    }
}

#[test]
fn gap_detect_units_rust() {
    let src = "//! module doc\npub fn visible() {}\npub(crate) fn hidden() {}\nfn private() {}\npub struct S;\npub async fn a() {}\npub const fn cf() {}\n";
    let units = gap::detect_units("x.rs", src, "rs");
    let names: Vec<&str> = units.iter().map(|u| u.name.as_str()).collect();
    assert_eq!(names, vec!["visible", "S", "a", "cf"]);
    assert_eq!(units[3].kind, "fn"); // `pub const fn cf` — const is a modifier, not the item.
}

#[test]
fn gap_detect_units_python() {
    let src = "def visible():\n    pass\n\n\ndef _private():\n    pass\n\n\nclass Public:\n    def method(self):\n        \"\"\"def not_a_unit(): pass\"\"\"\n        pass\n";
    let units = gap::detect_units("x.py", src, "py");
    let names: Vec<&str> = units.iter().map(|u| u.name.as_str()).collect();
    // `_private` (underscore) and the indented `method`/docstring example are
    // both out of scope for this module-level-only pragmatic scan.
    assert_eq!(names, vec!["visible", "Public"]);
}

#[test]
fn gap_detect_units_ts() {
    let src = "export function visible() {}\nfunction internal() {}\nexport class Widget {}\nexport interface Shape {}\n";
    let units = gap::detect_units("x.ts", src, "ts");
    let names: Vec<&str> = units.iter().map(|u| u.name.as_str()).collect();
    assert_eq!(names, vec!["visible", "Widget", "Shape"]);
}

#[test]
fn gap_detect_units_unsupported_ext() {
    assert!(gap::detect_units("x.toml", "pub fn not_real() {}\n", "toml").is_empty());
}

#[test]
fn gap_preceding_doc_block_contiguous_run() {
    let syntax = syntax_for_extension("rs").unwrap();
    let content = "//! line one\n//! line two\npub fn f() {}\n";
    assert_eq!(gap::preceding_doc_block_start(content, 3, &syntax), 1);
}

#[test]
fn gap_preceding_doc_block_stops_at_blank_line() {
    let syntax = syntax_for_extension("rs").unwrap();
    // A blank separator line detaches the doc comment from the item below it
    // (mirrors rustdoc's own attachment rule) — no block directly above line 3.
    let content = "//! detached doc\n\npub fn f() {}\n";
    assert_eq!(gap::preceding_doc_block_start(content, 3, &syntax), 3);
}

#[test]
fn gap_preceding_doc_block_none_when_no_comment_directly_above() {
    let syntax = syntax_for_extension("rs").unwrap();
    let content = "let x = 1;\npub fn f() {}\n";
    assert_eq!(gap::preceding_doc_block_start(content, 2, &syntax), 2);
}

#[test]
fn gap_backward_gaps_flags_undocumented_unit() {
    let syntax = syntax_for_extension("rs").unwrap();
    let content = "let x = 1;\nlet y = 2;\npub fn f() {}\n";
    let units = vec![CodeUnit {
        path: "x.rs".into(),
        line: 3,
        kind: "fn".into(),
        name: "f".into(),
    }];
    // No references at all: the unit is a gap.
    assert_eq!(gap::backward_gaps(content, &syntax, &units, &[]), units);
}

#[test]
fn gap_backward_gaps_clears_documented_unit() {
    let syntax = syntax_for_extension("rs").unwrap();
    let content = "//! # Spec References\n//! - [`SPEC-X-01~draft`](docs/specs/x.md#SPEC-X-01~draft)\npub fn f() {}\n";
    let units = vec![CodeUnit {
        path: "x.rs".into(),
        line: 3,
        kind: "fn".into(),
        name: "f".into(),
    }];
    // The reference on line 2 sits in the contiguous comment run directly
    // above line 3's `pub fn`.
    let refs = vec![make_ref(
        "SPEC-X-01~draft",
        "docs/specs/x.md",
        "SPEC-X-01~draft",
        2,
    )];
    assert!(gap::backward_gaps(content, &syntax, &units, &refs).is_empty());
}

#[test]
fn gap_backward_gaps_second_unit_needs_its_own_reference() {
    let syntax = syntax_for_extension("rs").unwrap();
    let content = "//! # Spec References\n//! - [`SPEC-X-01~draft`](docs/specs/x.md#SPEC-X-01~draft)\npub fn first() {}\n\npub fn second() {}\n";
    let units = vec![
        CodeUnit {
            path: "x.rs".into(),
            line: 3,
            kind: "fn".into(),
            name: "first".into(),
        },
        CodeUnit {
            path: "x.rs".into(),
            line: 5,
            kind: "fn".into(),
            name: "second".into(),
        },
    ];
    // Only `first` (line 3) has a directly preceding reference (line 2);
    // `second` (line 5) has a blank line, then `first`'s own declaration,
    // directly above it — no comment block of its own — so it is still a gap.
    let refs = vec![make_ref(
        "SPEC-X-01~draft",
        "docs/specs/x.md",
        "SPEC-X-01~draft",
        2,
    )];
    let gaps = gap::backward_gaps(content, &syntax, &units, &refs);
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].name, "second");
}

#[test]
fn gap_backward_gaps_ref_inside_previous_unit_body_does_not_cover_next_unit() {
    // Regression for the PR #3783 review finding: a reference sitting
    // anywhere between the previous unit and this one (e.g. inside the
    // previous unit's own body, documenting ITS internals) must NOT count as
    // covering the NEXT, unrelated unit just because it falls in that span.
    let syntax = syntax_for_extension("rs").unwrap();
    let content = "pub struct A {\n    field: u32,\n    // # Spec References\n    // - [`SPEC-X-01~draft`](docs/specs/x.md#SPEC-X-01~draft)\n}\n\npub fn b_unrelated() {}\n";
    let units = vec![
        CodeUnit {
            path: "x.rs".into(),
            line: 1,
            kind: "struct".into(),
            name: "A".into(),
        },
        CodeUnit {
            path: "x.rs".into(),
            line: 7,
            kind: "fn".into(),
            name: "b_unrelated".into(),
        },
    ];
    let refs = vec![make_ref(
        "SPEC-X-01~draft",
        "docs/specs/x.md",
        "SPEC-X-01~draft",
        4,
    )];
    let gaps = gap::backward_gaps(content, &syntax, &units, &refs);
    assert_eq!(
        gaps.iter().map(|u| u.name.as_str()).collect::<Vec<_>>(),
        vec!["A", "b_unrelated"],
        "the internal reference at line 4 documents A's own body, not \
         b_unrelated, and must not silently clear b_unrelated as covered"
    );
}

#[test]
fn gap_forward_gap_detects_unlinked_section() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // A single anchored section, no code file at all: nothing can possibly
    // link to it, so it must surface as exactly one forward gap.
    write(
        root,
        "docs/specs/x.md",
        "## Orphan {#SPEC-X-01~draft}\nbody\n",
    );

    let report = gap::run_gap_report(root);

    assert_eq!(report.spec_sections_scanned, 1);
    assert_eq!(report.forward_gaps.len(), 1);
    assert_eq!(report.forward_gaps[0].id, "SPEC-X-01~draft");
    assert_eq!(report.forward_gaps[0].path, "docs/specs/x.md");
    assert_eq!(report.forward_gaps[0].line, 1);
}

#[test]
fn gap_run_report_backward_and_forward() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // A spec doc with two sections: one referenced by code (linked), one not
    // (a forward gap).
    write(
        root,
        "docs/specs/x.md",
        "## Linked {#SPEC-X-01~draft}\nbody\n\n## Orphan {#SPEC-X-02~draft}\nbody\n",
    );

    // A code file with two public units: one documented (backward-clean), one
    // not (a backward gap). Only references `SPEC-X-01~draft`, leaving
    // `SPEC-X-02~draft` with no inbound code link.
    write(
        root,
        "crates/x/src/lib.rs",
        "//! # Spec References\n//! - [`SPEC-X-01~draft`](docs/specs/x.md#SPEC-X-01~draft)\npub fn documented() {}\npub fn undocumented() {}\n",
    );

    let report = gap::run_gap_report(root);

    assert_eq!(report.units_scanned, 2);
    assert!(
        report
            .backward_gaps
            .iter()
            .any(|u| u.name == "undocumented"),
        "expected `undocumented` as a backward gap: {:?}",
        report.backward_gaps
    );
    assert!(
        !report.backward_gaps.iter().any(|u| u.name == "documented"),
        "did not expect `documented` as a backward gap: {:?}",
        report.backward_gaps
    );

    assert_eq!(report.spec_sections_scanned, 2);
    assert!(
        report
            .forward_gaps
            .iter()
            .any(|s| s.id == "SPEC-X-02~draft"),
        "expected SPEC-X-02~draft as a forward gap: {:?}",
        report.forward_gaps
    );
    assert!(
        !report
            .forward_gaps
            .iter()
            .any(|s| s.id == "SPEC-X-01~draft"),
        "did not expect SPEC-X-01~draft as a forward gap: {:?}",
        report.forward_gaps
    );

    assert!(!report.is_strict_clean());
}

#[test]
fn gap_run_report_valid_linked_pair_is_fully_clean() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "docs/specs/x.md",
        "## Section {#SPEC-X-01~draft}\nbody\n",
    );
    write(
        root,
        "crates/x/src/lib.rs",
        "//! # Spec References\n//! - [`SPEC-X-01~draft`](docs/specs/x.md#SPEC-X-01~draft)\npub fn f() {}\n",
    );

    let report = gap::run_gap_report(root);
    assert!(
        report.backward_gaps.is_empty(),
        "{:?}",
        report.backward_gaps
    );
    assert!(report.forward_gaps.is_empty(), "{:?}", report.forward_gaps);
    assert!(
        report.broken_references.is_empty(),
        "{:?}",
        report.broken_references
    );
    assert!(report.is_strict_clean());
}

#[test]
fn gap_run_report_folds_in_broken_references() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "crates/x/src/lib.rs",
        "//! # Spec References\n//! - [`SPEC-GONE-01~draft`](docs/specs/gone.md#SPEC-GONE-01~draft)\n",
    );

    let report = gap::run_gap_report(root);
    assert!(report
        .broken_references
        .iter()
        .any(|d| d.check == "ref-path-missing"));
    assert!(!report.is_strict_clean());
}

#[test]
fn gap_is_strict_clean_ignores_advisory_severity() {
    let mut report = gap::GapReport::default();
    report.broken_references.push(Diagnostic::warning(
        "a.rs",
        1,
        "ref-revision-drift",
        "stale",
    ));
    assert!(
        report.is_strict_clean(),
        "an advisory-only broken reference must not fail --strict"
    );
    report
        .broken_references
        .push(Diagnostic::error("a.rs", 1, "ref-path-missing", "gone"));
    assert!(!report.is_strict_clean());
}

#[test]
fn gap_to_json_round_trips_counts() {
    let report = gap::GapReport {
        units_scanned: 5,
        spec_sections_scanned: 2,
        ..gap::GapReport::default()
    };
    let json = report.to_json();
    assert_eq!(json["units_scanned"], 5);
    assert_eq!(json["spec_sections_scanned"], 2);
    assert_eq!(json["backward_gaps"].as_array().unwrap().len(), 0);
}

#[test]
fn gap_summary_lists_top_offenders() {
    let mut report = gap::GapReport::default();
    for i in 0..3 {
        report.backward_gaps.push(CodeUnit {
            path: "crates/x/src/lib.rs".into(),
            line: i + 1,
            kind: "fn".into(),
            name: format!("f{i}"),
        });
    }
    report.backward_gaps.push(CodeUnit {
        path: "crates/y/src/lib.rs".into(),
        line: 1,
        kind: "fn".into(),
        name: "g".into(),
    });
    let summary = report.summary();
    assert!(summary.contains("backward gaps"));
    assert!(summary.contains("crates/x/src/lib.rs"));
    assert!(summary.contains("   3  crates/x/src/lib.rs"));
}
