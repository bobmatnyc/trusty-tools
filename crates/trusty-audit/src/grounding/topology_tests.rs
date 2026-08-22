//! Tests for the crate-topology grounding leg (#6147).
//!
//! The graph shapes are driven from literal `cargo metadata` documents so every
//! arm — a hub, a dev-only edge, a cycle cargo would itself reject — is
//! reachable without a checkout. One test does build a real fixture workspace on
//! disk and run cargo against it, which is what proves the document this module
//! parses is the document cargo actually emits.

use std::fs;
use std::path::Path;

use super::{CrateNode, Outcome, Topology, ground_into, measure, parse, write_into};

/// A `cargo metadata --no-deps` document over the named packages.
///
/// Each entry is `(name, [(dep, kind)])`, where `kind` is the JSON `kind` value
/// verbatim: `null` for a normal dependency, `"dev"` or `"build"` otherwise.
fn metadata(packages: &[(&str, &[(&str, &str)])]) -> String {
    let rendered: Vec<String> = packages
        .iter()
        .map(|(name, deps)| {
            let deps: Vec<String> = deps
                .iter()
                .map(|(dep, kind)| format!(r#"{{"name":"{dep}","kind":{kind}}}"#))
                .collect();
            format!(r#"{{"name":"{name}","dependencies":[{}]}}"#, deps.join(","))
        })
        .collect();
    format!(r#"{{"packages":[{}]}}"#, rendered.join(","))
}

fn node<'a>(topology: &'a Topology, name: &str) -> &'a CrateNode {
    topology
        .crates
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("{name} is not a member: {:?}", topology.crates))
}

/// The three headline numbers come from the document, not from a guess.
#[test]
fn edges_and_inbound_counts_come_from_the_metadata() {
    let json = metadata(&[
        ("core", &[]),
        ("mid", &[("core", "null")]),
        ("app", &[("core", "null"), ("mid", "null")]),
        // An external dependency shares no name with a member, so it is not an
        // internal edge however many packages declare it.
        ("tool", &[("serde", "null"), ("core", "null")]),
    ]);

    let topology = parse(&json).expect("parses");

    assert_eq!(topology.members, 4);
    assert_eq!(topology.edges, 4);
    assert_eq!(node(&topology, "core").inbound, 3);
    assert_eq!(node(&topology, "mid").inbound, 1);
    assert_eq!(node(&topology, "app").inbound, 0);
    assert_eq!(node(&topology, "app").deps, vec!["core", "mid"]);
    assert!(node(&topology, "tool").deps.iter().all(|d| d != "serde"));
    assert!(topology.cycles.is_empty(), "{:?}", topology.cycles);
}

/// A dev-dependency is not an architecture edge — cargo permits a cycle through
/// one, so counting them would report a routine test arrangement as a defect.
#[test]
fn dev_dependencies_are_not_architecture_edges() {
    let json = metadata(&[
        ("core", &[("harness", r#""dev""#)]),
        ("harness", &[("core", "null")]),
    ]);

    let topology = parse(&json).expect("parses");

    assert_eq!(topology.edges, 1);
    assert!(node(&topology, "core").deps.is_empty());
    assert_eq!(node(&topology, "harness").deps, vec!["core"]);
    assert!(
        topology.cycles.is_empty(),
        "a dev-dependency back-edge is not a cycle: {:?}",
        topology.cycles
    );
}

/// A build dependency IS an architecture edge: it is compiled and it constrains
/// the build order exactly as a normal dependency does.
#[test]
fn build_dependencies_are_architecture_edges() {
    let json = metadata(&[("core", &[]), ("app", &[("core", r#""build""#)])]);

    let topology = parse(&json).expect("parses");

    assert_eq!(topology.edges, 1);
    assert_eq!(node(&topology, "core").inbound, 1);
}

/// The deliberate cycle: three crates in a ring, and a fourth outside it.
#[test]
fn a_deliberate_cycle_is_detected() {
    let json = metadata(&[
        ("a", &[("b", "null")]),
        ("b", &[("c", "null")]),
        ("c", &[("a", "null")]),
        ("outside", &[("a", "null")]),
    ]);

    let topology = parse(&json).expect("parses");

    assert_eq!(
        topology.cycles,
        vec![vec!["a".to_string(), "b".to_string(), "c".to_string()]]
    );
    assert!(
        !topology
            .cycles
            .iter()
            .any(|c| c.contains(&"outside".to_string())),
        "a crate that only points INTO the ring is not in it: {:?}",
        topology.cycles
    );
}

/// Two independent rings are two cycles, not one.
#[test]
fn independent_cycles_are_reported_separately() {
    let json = metadata(&[
        ("a", &[("b", "null")]),
        ("b", &[("a", "null")]),
        ("y", &[("z", "null")]),
        ("z", &[("y", "null")]),
    ]);

    let topology = parse(&json).expect("parses");

    assert_eq!(
        topology.cycles,
        vec![
            vec!["a".to_string(), "b".to_string()],
            vec!["y".to_string(), "z".to_string()]
        ]
    );
}

/// The shared core is the most-depended-on members, capped, ties by name.
#[test]
fn the_shared_core_is_the_most_depended_on_members() {
    let json = metadata(&[
        ("core", &[]),
        ("util", &[]),
        ("a", &[("core", "null"), ("util", "null")]),
        ("b", &[("core", "null")]),
        ("c", &[("core", "null")]),
        ("lonely", &[]),
    ]);

    let topology = parse(&json).expect("parses");
    let core: Vec<(&str, usize)> = topology
        .shared_core()
        .iter()
        .map(|c| (c.name.as_str(), c.inbound))
        .collect();

    assert_eq!(core, vec![("core", 3), ("util", 1)]);
}

/// A document that is not JSON is a reason, not a panic.
#[test]
fn metadata_that_is_not_json_is_a_reason() {
    let cause = parse("error: could not read manifest").expect_err("must not parse");
    assert!(cause.contains("not readable as JSON"), "{cause}");
}

/// A JSON document with no `packages` array is a reason, not an empty graph.
#[test]
fn metadata_with_no_packages_array_is_a_reason() {
    let cause = parse(r#"{"version":1}"#).expect_err("must not parse");
    assert!(cause.contains("no `packages` array"), "{cause}");
}

/// A repository with no `Cargo.toml` is a declared skip, never a gap: it has no
/// crate topology to miss, and saying so in its report would be noise.
#[test]
fn a_directory_with_no_cargo_toml_is_a_declared_skip() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("package.json"), "{}").expect("write");

    match measure(dir.path()) {
        Outcome::NotAWorkspace(reason) => assert!(reason.contains("no Cargo.toml"), "{reason}"),
        other => panic!("expected a declared skip, got {other:?}"),
    }
}

/// A single-crate Rust repository is a declared skip for the same reason.
#[test]
fn a_single_crate_manifest_is_a_declared_skip() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"solo\"\nversion = \"0.1.0\"\n",
    )
    .expect("write");

    match measure(dir.path()) {
        Outcome::NotAWorkspace(reason) => assert!(reason.contains("`[workspace]`"), "{reason}"),
        other => panic!("expected a declared skip, got {other:?}"),
    }
}

/// A workspace whose members do not exist is a NAMED gap — it is a workspace,
/// so the report owes its reader an explanation for the missing section.
#[test]
fn a_workspace_cargo_cannot_read_is_unavailable() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"absent\"]\nresolver = \"2\"\n",
    )
    .expect("write");

    match measure(dir.path()) {
        Outcome::Unavailable(cause) => assert!(cause.contains("metadata"), "{cause}"),
        other => panic!("expected an unavailable graph, got {other:?}"),
    }
}

/// Write a three-crate fixture workspace: `core` <- `mid` <- `app`, plus one
/// dev-only back edge from `core` onto `app` that must not count.
fn fixture_workspace(root: &Path) {
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"core\", \"mid\", \"app\"]\nresolver = \"2\"\n",
    )
    .expect("root manifest");
    for (name, body) in [
        (
            "core",
            "[dev-dependencies]\nfixture-app = { path = \"../app\" }\n",
        ),
        (
            "mid",
            "[dependencies]\nfixture-core = { path = \"../core\" }\n",
        ),
        (
            "app",
            "[dependencies]\nfixture-core = { path = \"../core\" }\nfixture-mid = { path = \"../mid\" }\n",
        ),
    ] {
        let dir = root.join(name);
        fs::create_dir_all(dir.join("src")).expect("member dir");
        fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"fixture-{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n{body}"
            ),
        )
        .expect("member manifest");
        fs::write(dir.join("src/lib.rs"), "").expect("member source");
    }
}

/// The end-to-end producer arm: a real workspace on disk, measured by the real
/// cargo. This is what proves the document [`parse`] reads is the one cargo
/// emits — every other graph test drives a literal.
#[test]
fn a_fixture_workspace_on_disk_is_measured() {
    let dir = tempfile::tempdir().expect("tempdir");
    fixture_workspace(dir.path());

    let topology = match measure(dir.path()) {
        Outcome::Measured(topology) => topology,
        other => panic!("expected a measurement, got {other:?}"),
    };

    assert_eq!(topology.members, 3);
    assert_eq!(topology.edges, 3);
    assert_eq!(node(&topology, "fixture-core").inbound, 2);
    assert_eq!(node(&topology, "fixture-mid").inbound, 1);
    assert_eq!(
        node(&topology, "fixture-app").deps,
        vec!["fixture-core", "fixture-mid"]
    );
    assert!(
        node(&topology, "fixture-core").deps.is_empty(),
        "the dev-only back edge must not be an architecture edge: {:?}",
        node(&topology, "fixture-core").deps
    );
    assert!(topology.cycles.is_empty(), "{:?}", topology.cycles);
    assert_eq!(topology.shared_core()[0].name, "fixture-core");
}

/// A manifest with one repository entry, `{path}`-substituted by the caller.
const SAMPLE: &str = r#"[report]
title = "Audit"

[[repositories]]
name = "Demo"
path = "{path}"
"#;

fn sample_manifest(dir: &Path, checkout: &Path) -> std::path::PathBuf {
    let manifest = dir.join("manifest.toml");
    fs::write(
        &manifest,
        SAMPLE.replace("{path}", &checkout.display().to_string()),
    )
    .expect("write manifest");
    manifest
}

fn two_crate_topology() -> Topology {
    parse(&metadata(&[("core", &[]), ("app", &[("core", "null")])])).expect("parses")
}

/// The topology lands on the entry naming the checkout, and nothing else moves.
#[test]
fn the_topology_lands_on_the_matching_repository() {
    let dir = tempfile::tempdir().expect("tempdir");
    let checkout = dir.path().join("demo");
    fs::create_dir_all(&checkout).expect("checkout");
    let manifest = sample_manifest(dir.path(), &checkout);

    write_into(&manifest, &checkout, &two_crate_topology()).expect("writes");

    let text = fs::read_to_string(&manifest).expect("read back");
    assert!(text.contains("title = \"Audit\""), "{text}");
    assert!(text.contains("crate_topology"), "{text}");
}

/// The round trip: what was written parses back as the same four facts.
#[test]
fn a_written_topology_round_trips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let checkout = dir.path().join("demo");
    fs::create_dir_all(&checkout).expect("checkout");
    let manifest = sample_manifest(dir.path(), &checkout);
    let written = parse(&metadata(&[
        ("a", &[("b", "null")]),
        ("b", &[("a", "null")]),
        ("c", &[("a", "null")]),
    ]))
    .expect("parses");

    write_into(&manifest, &checkout, &written).expect("writes");

    let text = fs::read_to_string(&manifest).expect("read back");
    let doc: toml::Value = toml::from_str(&text).expect("valid TOML");
    let table = doc["repositories"][0]["crate_topology"]
        .as_table()
        .expect("a crate_topology table");
    assert_eq!(table["members"].as_integer(), Some(3));
    assert_eq!(table["edges"].as_integer(), Some(3));
    assert_eq!(
        table["cycles"][0]
            .as_array()
            .expect("a cycle")
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    let rows = table["crates"].as_array().expect("crate rows");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["name"].as_str(), Some("a"));
    assert_eq!(rows[0]["inbound"].as_integer(), Some(2));
    assert_eq!(
        rows[0]["deps"]
            .as_array()
            .expect("deps")
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>(),
        vec!["b"]
    );
}

/// A manifest naming a different checkout is refused rather than written to the
/// wrong repository — a topology attributed to the wrong repo is worse than none.
#[test]
fn a_topology_with_no_matching_entry_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let declared = dir.path().join("declared");
    fs::create_dir_all(&declared).expect("checkout");
    let manifest = sample_manifest(dir.path(), &declared);

    let cause = write_into(&manifest, &dir.path().join("other"), &two_crate_topology())
        .expect_err("must refuse");

    assert!(cause.contains("no `[[repositories]]` entry"), "{cause}");
}

/// A declared skip writes nothing and says nothing: a non-Rust repository's
/// report is byte-identical to what it was before this leg existed.
#[test]
fn a_declared_skip_writes_nothing_and_says_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let checkout = dir.path().join("demo");
    fs::create_dir_all(&checkout).expect("checkout");
    fs::write(checkout.join("package.json"), "{}").expect("write");
    let manifest = sample_manifest(dir.path(), &checkout);
    let before = fs::read_to_string(&manifest).expect("read");

    let gaps = ground_into(&manifest, &checkout, "Demo");

    assert!(gaps.is_empty(), "{gaps:?}");
    assert_eq!(fs::read_to_string(&manifest).expect("read"), before);
}

/// A workspace whose graph cannot be read is one gap naming the repository and
/// what its report will therefore not carry.
#[test]
fn an_unreadable_graph_is_a_named_gap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let checkout = dir.path().join("demo");
    fs::create_dir_all(&checkout).expect("checkout");
    fs::write(
        checkout.join("Cargo.toml"),
        "[workspace]\nmembers = [\"absent\"]\nresolver = \"2\"\n",
    )
    .expect("write");
    let manifest = sample_manifest(dir.path(), &checkout);

    let gaps = ground_into(&manifest, &checkout, "Demo");

    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].starts_with("Demo: "), "{}", gaps[0]);
    assert!(gaps[0].contains("crate topology"), "{}", gaps[0]);
}

/// A measured workspace reaches the manifest through `ground_into` with no gap.
#[test]
fn a_measured_workspace_reaches_the_manifest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let checkout = dir.path().join("demo");
    fs::create_dir_all(&checkout).expect("checkout");
    fixture_workspace(&checkout);
    let manifest = sample_manifest(dir.path(), &checkout);

    let gaps = ground_into(&manifest, &checkout, "Demo");

    assert!(gaps.is_empty(), "{gaps:?}");
    let text = fs::read_to_string(&manifest).expect("read back");
    assert!(text.contains("members = 3"), "{text}");
    assert!(text.contains("fixture-core"), "{text}");
}
