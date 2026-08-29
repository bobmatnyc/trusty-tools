//! Unit tests for the evidence-quality rules (#6082).
//!
//! Every case here is drawn from the graded self-audit of 2026-08-21, which
//! scored "authentication & secrets" WEAK and "dependencies" INADEQUATE for the
//! two defects these rules close.

use super::*;
use crate::grounding::evidence::DimensionEvidence;

/// The promotion this floor exists to stop: the auth dimension's HEADLINE
/// evidence was a call-graph test file at score 0.02, in a run whose genuine
/// hits scored 0.6–0.9.
#[test]
fn a_filler_row_beside_a_real_hit_is_dropped() {
    let floor = evidence_floor(0.9);
    assert!(!is_evidence(0.02, floor), "the observed noise promotion");
    assert!(!is_evidence(0.0, floor));
    assert!(
        !is_evidence(f32::NAN, floor),
        "a NaN score is never evidence"
    );
    assert!(is_evidence(0.62, floor), "a genuine hit from the same run");
    assert!(is_evidence(0.9, floor), "the best hit always survives");
}

/// #6082: the floor tracks the scale the daemon actually answers on.
///
/// `search.query` fuses its lanes with RRF, so no hit can score
/// above `1/61 ≈ 0.0164`. Against the absolute `0.15` this replaced, every
/// assertion below fails: the whole result set is rejected and the discovery leg
/// returns nothing however good the index is.
#[test]
fn rrf_scale_hits_clear_a_relative_floor() {
    // The head and tail of a real `hybrid` response over an 85 840-chunk index.
    let (best, tail) = (0.012_94_f32, 0.007_8_f32);
    let floor = evidence_floor(best);
    assert!(is_evidence(best, floor), "the best RRF hit is evidence");
    assert!(is_evidence(tail, floor), "so is the head of its tail");
    assert!(
        !is_evidence(best * 0.1, floor),
        "an order-of-magnitude weaker row is still filler"
    );
}

/// The floor is a share of the best hit, and never a number the caller must
/// know the daemon's scale to pick.
#[test]
fn the_floor_scales_with_the_best_hit() {
    assert!((evidence_floor(0.9) - 0.45).abs() < 1e-6);
    assert!((evidence_floor(0.012_94) - 0.006_47).abs() < 1e-6);
    for unusable in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert_eq!(
            evidence_floor(unusable),
            0.0,
            "an unusable best yields no floor, never one that rejects everything"
        );
    }
}

/// A test file matches the query that looks for the thing it tests, so without
/// demotion it outranks the production file it exercises.
#[test]
fn a_test_file_is_demoted_for_a_production_dimension() {
    for path in [
        "crates/trusty-search/src/service/call_chain/tests.rs",
        "crates/x/src/auth_test.rs",
        "crates/x/tests/integration.rs",
        "src/__tests__/login.ts",
        "api/auth.test.ts",
        "pkg/auth/server_test.go",
        "benches/hashing.rs",
    ] {
        assert!(
            demoted_for("authentication & secrets", path),
            "must be demoted: {path}"
        );
    }
    assert!(
        !demoted_for(
            "authentication & secrets",
            "crates/trusty-agents/src/api/server/auth.rs"
        ),
        "the production middleware the audit never read"
    );
}

/// Under "test coverage" the relationship inverts — a test file IS the evidence.
#[test]
fn a_test_file_is_first_class_for_test_coverage() {
    assert!(!demoted_for(
        TEST_DIMENSION,
        "crates/trusty-search/src/service/call_chain/tests.rs"
    ));
    assert!(!demoted_for(TEST_DIMENSION, "tests/integration.rs"));
}

/// A tree fixture with a root manifest, a member manifest, and a vendored one.
fn tree() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("root manifest");
    std::fs::write(root.join("deny.toml"), "").expect("deny");
    std::fs::create_dir_all(root.join("crates/api")).expect("member dir");
    std::fs::write(root.join("crates/api/Cargo.toml"), "").expect("member manifest");
    std::fs::create_dir_all(root.join("node_modules/left-pad")).expect("vendor dir");
    std::fs::write(root.join("node_modules/left-pad/package.json"), "{}").expect("vendored");
    std::fs::create_dir_all(root.join("target/debug")).expect("target dir");
    std::fs::write(root.join("target/debug/Cargo.toml"), "").expect("build artifact");
    tmp
}

/// The defect: the dimension read three semantic hits and never `Cargo.toml`,
/// then reported no manifest-declared dependencies for a 134-dependency
/// workspace. Manifests now lead, and the vendored trees are not manifests of
/// this repository.
#[test]
fn manifests_lead_the_dependencies_dimension() {
    let tmp = tree();
    let found = dependency_manifests(tmp.path());
    let paths: Vec<&str> = found.iter().map(|f| f.path.as_str()).collect();

    assert_eq!(
        paths,
        vec!["Cargo.toml", "deny.toml", "crates/api/Cargo.toml"],
        "root manifests lead, then members; vendored and build trees are skipped"
    );
    assert!(
        found[0].reason.contains("build manifest present"),
        "the reason names the enumeration, not a query: {}",
        found[0].reason
    );

    // Prepending keeps the semantic hits, behind the manifests, deduplicated.
    let mut dimensions = vec![DimensionEvidence {
        dimension: DEPENDENCY_DIMENSION.to_owned(),
        files: vec![
            FileEvidence {
                path: "src/client.rs".to_owned(),
                reason: "trusty-search hit".to_owned(),
            },
            FileEvidence {
                path: "Cargo.toml".to_owned(),
                reason: "trusty-search hit".to_owned(),
            },
        ],
    }];
    lead_with_manifests(&mut dimensions, tmp.path(), 8);
    assert_eq!(
        dimensions[0]
            .files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Cargo.toml",
            "deny.toml",
            "crates/api/Cargo.toml",
            "src/client.rs"
        ],
        "one entry per path, manifests first"
    );
}

/// Search finding nothing for the dimension is the exact case that produced the
/// false claim, so the dimension has to be created rather than skipped.
#[test]
fn a_dependencies_dimension_is_created_when_search_found_none() {
    let tmp = tree();
    let mut dimensions = vec![DimensionEvidence {
        dimension: "error handling".to_owned(),
        files: vec![FileEvidence {
            path: "src/err.rs".to_owned(),
            reason: "trusty-search hit".to_owned(),
        }],
    }];
    lead_with_manifests(&mut dimensions, tmp.path(), 2);

    let deps = dimensions
        .iter()
        .find(|d| d.dimension == DEPENDENCY_DIMENSION)
        .expect("the dimension is created");
    assert_eq!(deps.files.len(), 2, "the cap still bounds it");
    assert_eq!(deps.files[0].path, "Cargo.toml");
    assert_eq!(
        dimensions[0].files.len(),
        1,
        "every other dimension is untouched"
    );
}

/// A tree with no build file is a real state; inventing an entry for it would be
/// the same lie in the other direction.
#[test]
fn a_tree_with_no_manifest_is_left_alone() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("README.md"), "no build file here").expect("write");
    assert!(dependency_manifests(tmp.path()).is_empty());

    let mut dimensions: Vec<DimensionEvidence> = Vec::new();
    lead_with_manifests(&mut dimensions, tmp.path(), 8);
    assert!(dimensions.is_empty(), "no dimension is invented");
}

/// A 21-member workspace has 22 `Cargo.toml` files; handing all of them over
/// spends the dimension's whole share on near-identical member manifests.
#[test]
fn dependency_manifests_are_capped() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("Cargo.toml"), "").expect("root");
    for i in 0..(MAX_MANIFESTS + 6) {
        let dir = tmp.path().join(format!("crates/m{i:02}"));
        std::fs::create_dir_all(&dir).expect("member dir");
        std::fs::write(dir.join("Cargo.toml"), "").expect("member");
    }
    let found = dependency_manifests(tmp.path());
    assert_eq!(found.len(), MAX_MANIFESTS);
    assert_eq!(found[0].path, "Cargo.toml", "the root manifest still leads");
}
