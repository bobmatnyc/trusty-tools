//! Tests for doc paths named in a PR body or found by search (#9193).

use super::*;

fn paths(body: &str) -> Vec<String> {
    extract_doc_paths(body, "acme", "billing")
}

/// Bare, backticked and linked ADR, spec and SLD paths are read, in order,
/// once each.
#[test]
fn extracts_adr_spec_and_sld_paths() {
    let body = "Implements docs/adr/0061-commits.md and `docs/specs/DOC-38-sld.md`.\n\
                See [the design](docs/design/ui.md), crates/trusty-review/docs/notes.md,\n\
                and docs/adr/0061-commits.md again.";
    assert_eq!(
        paths(body),
        [
            "docs/adr/0061-commits.md",
            "docs/specs/DOC-38-sld.md",
            "docs/design/ui.md",
            "crates/trusty-review/docs/notes.md",
        ]
    );
}

/// An SLD reference `PATH#SPEC-…~rev` reads the whole file at PATH.
#[test]
fn sld_anchor_is_split_off() {
    let body = "Governed by docs/specs/spec-linked-documentation.md#SPEC-SLD-02~draft.";
    assert_eq!(paths(body), ["docs/specs/spec-linked-documentation.md"]);
}

/// A blob URL of the reviewed repo names its path; the URL's ref is dropped,
/// since docs are always read at the head SHA.
#[test]
fn github_blob_url_ref_is_ignored() {
    let body = "Per https://github.com/acme/billing/blob/main/docs/adr/0002-rounding.md#context";
    assert_eq!(paths(body), ["docs/adr/0002-rounding.md"]);
}

/// #9193 Fail-Open Check (path validation): traversal, absolute, encoded and
/// foreign-repo paths are never candidates.
#[test]
fn traversal_absolute_and_foreign_repo_paths_are_rejected() {
    let body = "docs/adr/../../etc/x.md /docs/adr/abs.md docs/adr/%2e%2e/x.md \
                docs/adr\\win.md https://github.com/other/billing/blob/main/docs/adr/x.md \
                https://github.com/acme/other/blob/main/docs/adr/y.md \
                https://example.com/docs/adr/z.md docs/adr//double.md";
    assert!(paths(body).is_empty(), "{:?}", paths(body));
}

/// #9193 amendment 16: the reviewed repo is matched case-insensitively.
#[test]
fn foreign_repo_comparison_is_case_insensitive() {
    let body = "https://GitHub.com/ACME/Billing/blob/abc1234/docs/adr/0003-x.md";
    assert_eq!(paths(body), ["docs/adr/0003-x.md"]);
}

/// Only the doc directories are allowed: a README, a vendored file or a
/// CLAUDE.md named in the body is not a review doc.
#[test]
fn readme_and_node_modules_not_in_allowlist() {
    let body = "README.md node_modules/x/docs/adr/a.md docs/adr/CLAUDE.md docs/notes.md \
                crates/docs/x.md src/docs/adr/x.md";
    assert!(paths(body).is_empty(), "{:?}", paths(body));
    assert!(is_doc_path("docs/reference/x.md"));
    assert!(!is_doc_path("docs/adr/x.txt"));
}

/// A body of a thousand paths is scanned for at most 64 candidates.
#[test]
fn a_thousand_paths_scan_is_bounded() {
    let body: String = (0..1000)
        .map(|i| format!("docs/adr/{i:04}-x.md "))
        .collect();
    let got = paths(&body);
    assert_eq!(got.len(), MAX_DOC_PATH_CANDIDATES);
    assert_eq!(got[0], "docs/adr/0000-x.md");
}

/// #9193 amendment 5: an absolute hit inside the index root is made
/// relative; outside the root, or with no known root, it is dropped.
#[test]
fn search_hit_absolute_path_is_made_repo_relative() {
    let root = Some("/srv/repo");
    assert_eq!(
        repo_relative("/srv/repo/docs/adr/x.md", root).as_deref(),
        Some("docs/adr/x.md")
    );
    assert_eq!(
        repo_relative("/srv/repo/docs/adr/x.md", Some("/srv/repo/")).as_deref(),
        Some("docs/adr/x.md")
    );
    assert_eq!(
        repo_relative("docs/specs/y.md", None).as_deref(),
        Some("docs/specs/y.md")
    );
    assert_eq!(repo_relative("/srv/other/docs/adr/x.md", root), None);
    assert_eq!(repo_relative("/srv/repository/docs/adr/x.md", root), None);
    assert_eq!(repo_relative("/srv/repo/docs/adr/x.md", None), None);
    assert_eq!(repo_relative("/srv/repo/../etc/x.md", root), None);
}
