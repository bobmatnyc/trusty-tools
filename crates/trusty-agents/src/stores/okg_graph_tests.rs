//! Unit tests for the OKG graph reader (#7430).
//!
//! Why: this module is what makes the exposed graph OKG-only, so its two
//! guarantees need pinning directly: it reads a directory tree (never a memory
//! palace), and it returns BOTH halves the owner's closure condition names —
//! triples and definitions.
//! What: tree-reading, the edge/description split, fail-open on a malformed
//! file, and the three tree-resolution arms.
//! Test: This module IS the test.

use std::path::Path;

use super::*;

/// Write one entity file under `<root>/<collection>/<slug>.md`.
fn entity(root: &Path, collection: &str, slug: &str, content: &str) {
    let dir = root.join(collection);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{slug}.md")), content).unwrap();
}

/// A tree holding two people, one edge each way, and one edgeless organisation.
fn fixture_tree() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    entity(
        tmp.path(),
        "people",
        "bob",
        "---\ntype: Person\ntitle: Bob\ndescription: The owner.\nworks_at: \"[[Duetto]]\"\nknows:\n  - \"[[Ada]]\"\n---\n\nBody line.\n",
    );
    entity(
        tmp.path(),
        "people",
        "ada",
        "---\ntype: Person\ntitle: Ada\n---\n\nFirst programmer.\n",
    );
    entity(
        tmp.path(),
        "organizations",
        "duetto",
        "---\ntype: Organization\ntitle: Duetto\ndescription: A company with a [[Bob]] mention.\n---\n",
    );
    tmp
}

/// Why: the owner's closure condition for #7430 is BOTH halves — triples and
/// definitions — out of the OKG tree. A reader that returned only edges would
/// satisfy the word "graph" and fail the requirement.
/// What: asserts every edge target became a triple with its source file as
/// provenance, and that every entity has a definition carrying its type and
/// summary.
/// Test: itself.
#[test]
fn reads_triples_and_definitions_from_a_tree() {
    let tmp = fixture_tree();
    let graph = read_graph(tmp.path()).unwrap();

    assert_eq!(
        graph
            .triples
            .iter()
            .map(|t| (t.subject.as_str(), t.predicate.as_str(), t.object.as_str()))
            .collect::<Vec<_>>(),
        vec![("Bob", "knows", "Ada"), ("Bob", "works_at", "Duetto"),],
        "triples: {:?}",
        graph.triples
    );
    assert_eq!(graph.triples[0].provenance, "people/bob.md");

    let defs: Vec<_> = graph
        .definitions
        .iter()
        .map(|d| (d.subject.as_str(), d.kind.as_deref(), d.summary.as_deref()))
        .collect();
    assert_eq!(
        defs,
        vec![
            (
                "Duetto",
                Some("Organization"),
                Some("A company with a [[Bob]] mention.")
            ),
            ("Ada", Some("Person"), Some("First programmer.")),
            ("Bob", Some("Person"), Some("The owner.")),
        ],
        "definitions: {defs:?}"
    );
}

/// Why: OKF writes an edge as a snake_case key holding `[[wiki-links]]`, but a
/// `description` is prose that may mention an entity. Treating every key with a
/// link as an edge would mint `Duetto description Bob` out of that sentence.
/// What: the fixture's organization carries a `[[Bob]]` inside `description`
/// and must contribute no triple.
/// Test: itself.
#[test]
fn envelope_fields_are_not_edges() {
    let graph = read_graph(fixture_tree().path()).unwrap();
    assert!(
        !graph
            .triples
            .iter()
            .any(|t| t.predicate == "description" || t.subject == "Duetto"),
        "an envelope field produced an edge: {:?}",
        graph.triples
    );
}

/// Why: an OKG tree mid-ingest is mostly entities with no edges yet. A subject
/// list built from triples alone would render that tree as empty, which reads as
/// "nothing was ingested".
/// What: the edgeless organisation is listed with a zero count.
/// Test: itself.
#[test]
fn subject_counts_include_edgeless_entities() {
    let graph = read_graph(fixture_tree().path()).unwrap();
    let counts: Vec<_> = graph
        .subject_counts()
        .into_iter()
        .map(|c| (c.subject, c.count))
        .collect();
    assert_eq!(
        counts,
        vec![
            ("Ada".to_string(), 0),
            ("Bob".to_string(), 2),
            ("Duetto".to_string(), 0),
        ]
    );
}

/// Why: one hand-edited file must not blank the pane — the same fail-open
/// posture `KbStore::list` takes.
/// What: an unparseable frontmatter file is skipped and its siblings still read.
/// Test: itself.
#[test]
fn malformed_entity_is_skipped() {
    let tmp = fixture_tree();
    entity(
        tmp.path(),
        "people",
        "broken",
        "---\n: : not yaml : :\n---\n",
    );
    let graph = read_graph(tmp.path()).unwrap();
    assert!(graph.definitions.iter().any(|d| d.subject == "Bob"));
    assert!(!graph.definitions.iter().any(|d| d.slug == "broken"));
}

/// An absent tree is an empty graph, not an error — a home exists before its
/// first ingest.
#[test]
fn an_absent_tree_reads_as_an_empty_graph() {
    let tmp = tempfile::tempdir().unwrap();
    let graph = read_graph(&tmp.path().join("never-created")).unwrap();
    assert_eq!(graph, OkgGraph::default());
}

fn stores_from(toml_src: &str) -> StoresConfig {
    #[derive(serde::Deserialize)]
    struct Partial {
        #[serde(default)]
        stores: StoresConfig,
    }
    toml::from_str::<Partial>(toml_src).unwrap().stores
}

/// Why: #4325 gave each assistant its own home-confined tree. A binding
/// declaring `root` must resolve there and nowhere else.
/// What: `root = "okg"` under an injected assistants root.
/// Test: itself.
#[test]
fn resolves_the_home_tree_for_a_rooted_binding() {
    let assistants = tempfile::tempdir().unwrap();
    let knowledge = tempfile::tempdir().unwrap();
    let stores = stores_from("[[stores]]\nname = \"k\"\nroot = \"okg\"\n");
    let tree = resolve_okg_root("izzie", &stores, assistants.path(), knowledge.path()).unwrap();
    assert_eq!(tree.root, assistants.path().join("izzie").join("okg"));
    assert_eq!(tree.label, "izzie/okg");
}

/// Why: a binding with no `root` still addresses `okg://<agent>` in the shared
/// knowledge pool, which is what `stores::binding` resolves for the index feed.
/// Resolving it differently here would let the browser and the ingest describe
/// different directories.
/// Test: itself.
#[test]
fn resolves_the_shared_pool_for_a_plain_binding() {
    let assistants = tempfile::tempdir().unwrap();
    let knowledge = tempfile::tempdir().unwrap();
    let stores = stores_from("[[stores]]\nname = \"k\"\nindex = \"k\"\n");
    let tree = resolve_okg_root("izzie", &stores, assistants.path(), knowledge.path()).unwrap();
    assert_eq!(tree.root, knowledge.path().join("izzie"));
    assert_eq!(tree.label, "okg://izzie");
}

/// Why: an agent that declares no `[[stores]]` still has the #4325 default tree
/// at `<home>/okg`. Reporting "no store bound" there would hide a real graph.
/// Test: itself.
#[test]
fn resolves_the_home_tree_when_no_store_is_bound() {
    let assistants = tempfile::tempdir().unwrap();
    let knowledge = tempfile::tempdir().unwrap();
    let tree = resolve_okg_root(
        "izzie",
        &StoresConfig::default(),
        assistants.path(),
        knowledge.path(),
    )
    .unwrap();
    assert_eq!(tree.root, assistants.path().join("izzie").join("okg"));
    assert_eq!(tree.label, "izzie/okg");
}

/// Why (#7430 security review): the label is what a client sees, so no
/// resolution arm may derive it from the resolved path. The `./` and nested
/// spellings are the ones most likely to pick up a prefix by accident.
/// What: every arm, including a `root` written `./knowledge` and one nested two
/// deep, must yield a relative, `/`-free label.
/// Test: itself.
#[test]
fn every_resolution_arm_labels_the_tree_without_a_path() {
    let assistants = tempfile::tempdir().unwrap();
    let knowledge = tempfile::tempdir().unwrap();
    let cases = [
        ("[[stores]]\nname = \"k\"\nroot = \"okg\"\n", "izzie/okg"),
        (
            "[[stores]]\nname = \"k\"\nroot = \"./knowledge\"\n",
            "izzie/knowledge",
        ),
        (
            "[[stores]]\nname = \"k\"\nroot = \"okg/personal\"\n",
            "izzie/okg/personal",
        ),
        ("[[stores]]\nname = \"k\"\nindex = \"k\"\n", "okg://izzie"),
    ];
    for (src, expected) in cases {
        let tree = resolve_okg_root(
            "izzie",
            &stores_from(src),
            assistants.path(),
            knowledge.path(),
        )
        .unwrap();
        assert_eq!(tree.label, expected, "for {src:?}");
        assert!(!tree.label.starts_with('/'), "for {src:?}");
        assert!(
            !tree
                .label
                .contains(&assistants.path().display().to_string()),
            "the label leaked the assistants root for {src:?}: {}",
            tree.label
        );
    }
}
