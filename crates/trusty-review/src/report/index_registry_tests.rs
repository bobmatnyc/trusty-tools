//! Tests for report-pipeline index resolution (#6677).
//!
//! Why: the defect was structural — one derivation, one exact id match, no way
//! for a ready index registered under another id to be found. The three
//! resolution outcomes are asserted against a fake registry here rather than a
//! live daemon, and a grep-level test keeps the two report call sites on this
//! one resolver so a future second matcher goes red instead of shipping.
//! What: the derivation, the three variants, the exactness of the root match,
//! the fail-open read, and the call-site check.
//! Test: included as `#[cfg(test)] mod tests` from `index_registry.rs`.

use super::*;

/// A registry entry, `root_path` and all.
fn index(id: &str, root_path: Option<&str>) -> IndexInfo {
    IndexInfo {
        id: id.to_string(),
        name: None,
        root_path: root_path.map(str::to_string),
    }
}

// ── Derivation ───────────────────────────────────────────────────────────────

/// #6149: the renderer and the audit are separate processes agreeing on one id.
/// Two checkouts of one repository must not derive the same one — that is the
/// collision that had this crate reading another tree's measurements.
#[test]
fn derive_index_id_distinguishes_same_named_checkouts() {
    let engagement = Path::new("/w/dogfood/repos/local/northwind-web");
    let working = Path::new("/home/me/northwind-web");

    let a = derive_index_id(engagement).expect("id");
    let b = derive_index_id(working).expect("id");
    assert_ne!(a, b, "{a} vs {b}");
    assert!(b.starts_with("northwind-web-"), "still readable: {b}");
    assert_eq!(derive_index_id(Path::new("/")), None);
}

/// The agreement is a call, not a copy: this crate's id IS trusty-common's, so
/// the audit that indexed under it and this renderer cannot drift.
#[test]
fn derive_index_id_is_the_shared_derivation() {
    for path in ["/home/me/northwind-web", "/w/repos/acme-api", "/"] {
        let path = Path::new(path);
        assert_eq!(
            derive_index_id(path),
            trusty_common::derive_checkout_index_id(path),
            "{}",
            path.display()
        );
    }
}

// ── The three resolution outcomes ────────────────────────────────────────────

/// Case 1: the daemon holds the derived id. Nothing changes.
#[test]
fn a_registered_derived_id_is_used_as_is() {
    let repo = Path::new("/w/repos/northwind-web");
    let derived = derive_index_id(repo).expect("id");
    // A root_path match under another id is present and must NOT win: an
    // exact derived hit is the addressing both processes already agree on.
    let indexes = vec![
        index(&derived, Some("/w/repos/northwind-web")),
        index("northwind-checkout", Some("/w/repos/northwind-web")),
    ];

    let resolved = resolve_report_index(repo, &indexes);
    assert_eq!(resolved, ReportIndex::Derived(derived.clone()));
    assert_eq!(resolved.id(), Some(derived.as_str()));
}

/// Case 2: the derived id is absent and a registered index IS this checkout —
/// the field case, where `trusty-tools-checkout` served the tree the derived
/// `trusty-tools-4e2cf878` could never address.
#[test]
fn a_root_path_match_substitutes_for_an_unregistered_derived_id() {
    let repo = Path::new("/w/repos/trusty-tools");
    let derived = derive_index_id(repo).expect("id");
    let indexes = vec![
        index("unrelated", Some("/w/repos/other")),
        index("trusty-tools-checkout", Some("/w/repos/trusty-tools")),
    ];

    let resolved = resolve_report_index(repo, &indexes);
    assert_eq!(
        resolved,
        ReportIndex::Substituted {
            id: "trusty-tools-checkout".to_string(),
            derived: Some(derived),
        }
    );
    assert_eq!(resolved.into_id().as_deref(), Some("trusty-tools-checkout"));
}

/// Case 3: neither matches. The derived id still goes to the daemon, so the
/// not-indexed fallback and `IndexAbsent` behave exactly as they did.
#[test]
fn no_match_keeps_the_derived_id() {
    let repo = Path::new("/w/repos/northwind-web");
    let derived = derive_index_id(repo).expect("id");
    let indexes = vec![
        index("unrelated", Some("/w/repos/other")),
        index("no-root-path", None),
    ];

    let resolved = resolve_report_index(repo, &indexes);
    assert_eq!(
        resolved,
        ReportIndex::Unresolved {
            derived: Some(derived.clone()),
        }
    );
    assert_eq!(resolved.id(), Some(derived.as_str()));
}

/// An empty registry — an unreachable daemon, or a machine with no index — is
/// case 3 with nothing to match against.
#[test]
fn an_empty_registry_keeps_the_derived_id() {
    let repo = Path::new("/w/repos/northwind-web");
    let derived = derive_index_id(repo).expect("id");
    assert_eq!(
        resolve_report_index(repo, &[]).into_id(),
        Some(derived),
        "an empty list must resolve exactly as it did before #6677"
    );
}

/// A path that derives to nothing and matches nothing yields no id at all, so
/// the analyze walk skips the repository as it always has.
#[test]
fn a_path_that_derives_nothing_resolves_to_nothing() {
    assert_eq!(
        resolve_report_index(Path::new("/"), &[index("some-index", Some("/w/repos/x"))]).into_id(),
        None
    );
}

/// The root match is EXACT: an index rooted at the parent covers the path and
/// describes a wider tree, which is the stale-scope failure #6137 catches.
#[test]
fn a_parent_rooted_index_is_not_substituted() {
    let repo = Path::new("/w/repos/northwind-web");
    let indexes = vec![index("monorepo", Some("/w/repos"))];

    assert!(
        matches!(
            resolve_report_index(repo, &indexes),
            ReportIndex::Unresolved { .. }
        ),
        "a parent-rooted index must not stand in for the checkout"
    );
}

/// Two spellings of one directory are one root: the registry stores canonical
/// paths and a manifest can hand over anything.
#[test]
fn the_root_comparison_is_canonicalised() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("sub/..").join("sub");
    std::fs::create_dir_all(dir.path().join("sub")).expect("mkdir");
    let canonical = dir.path().join("sub").canonicalize().expect("canonicalize");
    let indexes = vec![index("registered", Some(&canonical.display().to_string()))];

    assert!(
        matches!(
            resolve_report_index(&repo, &indexes),
            ReportIndex::Substituted { .. }
        ),
        "an uncanonical spelling of the same directory must still match"
    );
}

// ── The fail-open read ───────────────────────────────────────────────────────

/// A daemon that is not there reads as an empty registry, never an error — the
/// pass then addresses the derived id, which is the pre-#6677 path.
#[tokio::test]
async fn an_unreachable_daemon_reads_an_empty_registry() {
    // Bind, read the port, drop: nothing listens there for the length of this
    // test, which is what makes the read fail rather than hang.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);

    let indexes = fetch_registered_indexes(&format!("http://127.0.0.1:{port}")).await;
    assert!(indexes.is_empty(), "a dead daemon must read as no indexes");
}

// ── One resolver, both call sites ────────────────────────────────────────────

/// Neither report call site derives or matches an index id of its own (#6677).
///
/// Why: the defect was two call sites each doing their own thing with the
/// derived id. A second matcher — anywhere in either file — is the regression,
/// and it is invisible to a behavioural test that stubs the source. This reads
/// the two files at compile time and fails on the derivation escaping this
/// module.
#[test]
fn neither_report_call_site_derives_its_own_index_id() {
    for (name, src) in [
        (
            "report/analyze_adapter.rs",
            include_str!("analyze_adapter.rs"),
        ),
        (
            "report/investigate/trace.rs",
            include_str!("investigate/trace.rs"),
        ),
    ] {
        assert!(
            !src.contains("derive_checkout_index_id"),
            "{name} derives its own index id; it must resolve through \
             report::index_registry (#6677)"
        );
        assert!(
            src.contains("resolve_report_index"),
            "{name} must take its index id from index_registry::resolve_report_index (#6677)"
        );
    }
}
