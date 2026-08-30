//! Why (#4474): keeps the crate's statement of "what full test coverage is"
//! true. `default = []` and 47 opt-in features mean no single `cargo test -p
//! trusty-common` run covers this crate, so which combination does was tribal
//! knowledge — and it went wrong repeatedly. `--features inference-client`
//! looked thorough while never compiling `inference::bedrock`; `credentials`,
//! `session-naming` and `memory-core` each shipped a PR whose prescribed gate
//! never ran their tests. #4901 stopped the bare run from passing vacuously;
//! this stops the runs that DO name features from doing it more quietly.
//!
//! What: reads the lanes and exemptions from `[package.metadata.
//! trusty-test-coverage]`, resolves each lane's transitive feature closure from
//! the `[features]` table, and fails when the union plus the exemptions is not
//! exactly the declared feature set. Adding a feature without placing it in a
//! lane or exempting it with a reason is therefore a test failure, not a silent
//! coverage hole. `scripts/test_trusty_common_lanes.sh` runs the same rows, so
//! the runner cannot drift from the statement it executes.
//!
//! This is an integration test target with no `required-features`, so it is
//! compiled and run by EVERY invocation — including `--features
//! unconditional-only`. That is the point: the check cannot itself be gated out
//! by the mechanism it exists to police.
//!
//! Test: the four `#[test]` functions below.

use std::collections::{BTreeMap, BTreeSet};

use toml::Value;

/// Why: every assertion here is about this crate's own manifest, so read it
/// from the one path Cargo guarantees rather than a relative guess that depends
/// on the test binary's working directory.
/// What: parses `$CARGO_MANIFEST_DIR/Cargo.toml` into a `toml::Value`.
/// Test: any parse failure fails all four tests below with the path in the
/// message.
fn manifest() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    text.parse::<Value>()
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// Why: the feature graph is what a lane's closure is resolved against.
/// What: returns `feature -> features it enables`, keeping only entries that
/// name another feature of this crate. `dep:foo` (an optional dependency) and
/// `crate/feat` (a dependency's own feature) are dropped — neither gates a
/// module here, so neither is something a lane can cover.
/// Test: `lanes_and_exemptions_name_only_declared_features` fails if this ever
/// returns a name the manifest does not declare.
fn feature_graph(manifest: &Value) -> BTreeMap<String, Vec<String>> {
    let table = manifest
        .get("features")
        .and_then(Value::as_table)
        .expect("[features] table");
    table
        .iter()
        .map(|(name, enables)| {
            let edges = enables
                .as_array()
                .unwrap_or_else(|| panic!("feature `{name}` must be an array"))
                .iter()
                .filter_map(Value::as_str)
                .filter(|e| !e.starts_with("dep:") && !e.contains('/'))
                .map(str::to_owned)
                .collect();
            (name.clone(), edges)
        })
        .collect()
}

/// Why: a lane names only its roots — `memory-core` already pulls in
/// `credentials`, `embedder`, `embedder-bundled-ort` and `redb-open`, and
/// listing those again would make the manifest a second place to keep the
/// feature graph correct.
/// What: walks `graph` from `seeds` and returns every feature reachable,
/// `seeds` included.
/// Test: `every_declared_feature_is_covered_by_a_lane_or_exempted`.
fn closure(graph: &BTreeMap<String, Vec<String>>, seeds: &[String]) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<String> = seeds.to_vec();
    while let Some(feature) = stack.pop() {
        if !seen.insert(feature.clone()) {
            continue;
        }
        if let Some(edges) = graph.get(&feature) {
            stack.extend(edges.iter().cloned());
        }
    }
    seen
}

/// The `[package.metadata.trusty-test-coverage]` table, already validated to be
/// present and well-shaped.
struct Coverage {
    /// Lane name → the features that lane names on the command line.
    lanes: Vec<(String, Vec<String>)>,
    /// Exempt feature → the reason no lane runs it.
    exempt: Vec<(String, String)>,
}

/// Why: one shaped read, so a malformed row fails once with a useful message
/// instead of four times with `Option::unwrap` panics.
/// What: reads `lanes` and `exempt` out of the package metadata table.
/// Test: all four tests below go through this.
fn coverage(manifest: &Value) -> Coverage {
    let table = manifest
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("trusty-test-coverage"))
        .and_then(Value::as_table)
        .expect("[package.metadata.trusty-test-coverage] table — see #4474");

    let lanes = table
        .get("lanes")
        .and_then(Value::as_array)
        .expect("`lanes` array")
        .iter()
        .map(|lane| {
            let name = lane
                .get("name")
                .and_then(Value::as_str)
                .expect("every lane needs a `name`")
                .to_owned();
            let features = lane
                .get("features")
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("lane `{name}` needs a `features` array"))
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            (name, features)
        })
        .collect();

    let exempt = table
        .get("exempt")
        .and_then(Value::as_array)
        .expect("`exempt` array")
        .iter()
        .map(|row| {
            let feature = row
                .get("feature")
                .and_then(Value::as_str)
                .expect("every exemption needs a `feature`")
                .to_owned();
            let reason = row
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            (feature, reason)
        })
        .collect();

    Coverage { lanes, exempt }
}

/// Why (#4474): this is the assertion the issue asks for. A feature added
/// without a lane is a module whose tests nothing runs, and the only signal
/// today is a test count nobody compares against anything.
/// What: unions every lane's transitive closure with the exempt set and
/// requires it to equal the declared feature set exactly.
/// Test: this test. Demonstrated by adding a feature and watching it fail.
#[test]
fn every_declared_feature_is_covered_by_a_lane_or_exempted() {
    let manifest = manifest();
    let graph = feature_graph(&manifest);
    let coverage = coverage(&manifest);

    let mut covered = BTreeSet::new();
    for (_, features) in &coverage.lanes {
        covered.extend(closure(&graph, features));
    }
    covered.extend(coverage.exempt.iter().map(|(f, _)| f.clone()));

    let declared: BTreeSet<String> = graph.keys().cloned().collect();
    let uncovered: Vec<&String> = declared.difference(&covered).collect();

    assert!(
        uncovered.is_empty(),
        "these trusty-common features are in no coverage lane and carry no exemption: {uncovered:?}\n\
         No `cargo test -p trusty-common` invocation runs their tests, so they can regress green.\n\
         Add each to a lane in [package.metadata.trusty-test-coverage] in crates/trusty-common/Cargo.toml,\n\
         or add an `exempt` row saying why no lane can run it. See #4474."
    );
}

/// Why: an exemption or a lane naming a feature that no longer exists is a
/// coverage claim about nothing, and it hides the feature that replaced it.
/// What: requires every name in `lanes` and `exempt` to be a declared feature.
/// Test: this test.
#[test]
fn lanes_and_exemptions_name_only_declared_features() {
    let manifest = manifest();
    let declared: BTreeSet<String> = feature_graph(&manifest).keys().cloned().collect();
    let coverage = coverage(&manifest);

    let named = coverage
        .lanes
        .iter()
        .flat_map(|(lane, features)| features.iter().map(move |f| (lane.as_str(), f)))
        .chain(coverage.exempt.iter().map(|(f, _)| ("exempt", f)));

    let stale: Vec<String> = named
        .filter(|(_, feature)| !declared.contains(*feature))
        .map(|(owner, feature)| format!("{owner}: {feature}"))
        .collect();

    assert!(
        stale.is_empty(),
        "coverage rows name features this crate no longer declares: {stale:?}\n\
         Fix the names in [package.metadata.trusty-test-coverage] in crates/trusty-common/Cargo.toml."
    );
}

/// Why: an exemption is a decision not to test something. Without a stated
/// reason it is indistinguishable from an oversight, and the next reader has no
/// basis for removing it.
/// What: requires every `exempt` row to carry a non-empty `reason`.
/// Test: this test.
#[test]
fn every_exemption_states_a_reason() {
    let manifest = manifest();
    let coverage = coverage(&manifest);
    let unreasoned: Vec<&String> = coverage
        .exempt
        .iter()
        .filter(|(_, reason)| reason.trim().is_empty())
        .map(|(feature, _)| feature)
        .collect();

    assert!(
        unreasoned.is_empty(),
        "these coverage exemptions state no reason: {unreasoned:?}\n\
         Say why no lane can run the feature, so the exemption can be removed when that stops being true."
    );
}

/// Why (#4901): the zero-feature guard in `src/lib.rs` discounts
/// `CARGO_FEATURE_DEFAULT` because Cargo activates `default` on every build.
/// That is correct only while `default` is empty; a non-empty `default` would
/// make a bare run indistinguishable from a deliberate feature selection and
/// disarm the guard.
/// What: asserts `default = []` through a real TOML parse. `build.rs` asserts
/// the same fact by text scan and turns a mismatch into a `cfg(test)`
/// `compile_error!`; this is the readable half, and the two disagreeing is
/// itself the signal that the scan misread the manifest.
/// Test: this test.
#[test]
fn default_feature_set_is_empty() {
    let manifest = manifest();
    let default = manifest
        .get("features")
        .and_then(|f| f.get("default"))
        .and_then(Value::as_array)
        .expect("[features] must declare `default`");

    assert!(
        default.is_empty(),
        "trusty-common declares `default = {default:?}`, but the #4901 zero-feature guard \
         in src/lib.rs assumes it is empty. A non-empty `default` sets CARGO_FEATURE_* on a \
         bare `cargo test -p trusty-common`, which makes the guard stop firing and restores \
         the vacuous green. Either keep `default` empty, or rewrite the guard in build.rs to \
         compare the enabled feature set against the default set."
    );
}
