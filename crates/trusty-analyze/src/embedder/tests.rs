//! Regression guards for the #5067 boot stall.
//!
//! Why: the defect was not a wrong value, it was work the daemon performed on
//! a path that did not need it — `NeuralEmbedder::new()` ran at every boot and
//! blocked on an untimed Hugging Face request. A timing assertion would be
//! flaky and would only catch the symptom on a slow network, so these tests
//! assert the *absence of the capability to do the work at all*: no
//! model-loading dependency in the manifest, no ORT backend feature to turn
//! one on, and no second `EmbedderKind` to request one.
//!
//! What: two manifest guards plus a wire-label guard.
//!
//! Test: this module.

use super::*;

/// The crate's own `Cargo.toml`, read at test time.
fn manifest() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Lines of the manifest that are neither blank nor a comment.
///
/// Why: the manifest keeps `#`-prefixed prose explaining *why* fastembed was
/// removed, and that prose names the thing it removed. Matching raw text would
/// make the guard fail on its own explanation.
fn manifest_directives() -> Vec<String> {
    manifest()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

/// Why (#5067): `fastembed` is what pulled in hf-hub, and hf-hub is what made
/// the untimed request that stalled boot for 31m46s. If the dependency is not
/// in the manifest, no boot path can pay that cost — this is the strongest
/// available statement of the fix, and it fails on the pre-fix tree.
/// What: asserts no directive line declares `fastembed`.
/// Test: this test.
#[test]
fn analyze_declares_no_in_process_model_deps() {
    let offenders: Vec<String> = manifest_directives()
        .into_iter()
        .filter(|l| l.starts_with("fastembed"))
        .collect();
    assert!(
        offenders.is_empty(),
        "trusty-analyze must not depend on an in-process embedding model \
         loader; #5067 removed fastembed because loading it blocked daemon \
         boot on an untimed network call. Found: {offenders:?}"
    );
}

/// Why (#5067): the `bundled-ort` / `load-dynamic` / `cuda` features existed
/// only to choose an ONNX Runtime backend for the neural embedder, and
/// `bundled-ort` was in the DEFAULT set — which is why every shipped release
/// asset carried the stall. Their absence is what keeps the default build free
/// of ONNX Runtime, and what lets the installer drop `trusty-analyze` from its
/// glibc-aware asset routing.
/// What: asserts none of the three feature names is declared.
/// Test: this test.
#[test]
fn analyze_declares_no_ort_backend_features() {
    let banned = ["bundled-ort", "load-dynamic", "cuda"];
    let declared: Vec<String> = manifest_directives()
        .into_iter()
        .filter(|l| {
            banned
                .iter()
                .any(|b| l.starts_with(&format!("{b} ")) || l.starts_with(&format!("{b}=")))
        })
        .collect();
    assert!(
        declared.is_empty(),
        "trusty-analyze must declare no ONNX Runtime backend feature; #5067 \
         removed them along with the neural embedder. Found: {declared:?}"
    );
}

/// Why (#5067): `EmbedderKind::Neural` was the only way to ask for the removed
/// backend. Keeping the variant would let a request select an embedder that no
/// longer exists and silently receive BOW vectors instead — the fail-open shape
/// the pre-fix daemon already had at startup.
/// What: asserts the default is `Bow`, its wire label is `"bow"`, and `"neural"`
/// no longer deserializes into the enum.
/// Test: this test.
#[test]
fn embedder_kind_has_only_bow() {
    assert_eq!(EmbedderKind::default(), EmbedderKind::Bow);
    assert_eq!(EmbedderKind::Bow.as_str(), "bow");
    assert!(
        serde_json::from_str::<EmbedderKind>("\"neural\"").is_err(),
        "`neural` must no longer deserialize — a caller asking for the removed \
         backend has to be told, not silently given BOW vectors"
    );
}
