//! `GET /api/agents/:name/kg*` tests (#4290, #6286, #7430).
//!
//! Why: this module's contract has two halves, and both have been wrong before.
//! The exposed graph must come from the assistant's OKG tree and nothing else —
//! before #7430 these routes served trusty-memory's palace-scoped `kg_*` surface
//! instead, so the pane labelled "Knowledge Graph" rendered the MEMORY graph.
//! And the route must NEVER fail for a condition the browser should render as an
//! empty state. These tests drive `kg_graph_at` directly against
//! `tempfile::TempDir` roots (the `agent_stores.rs` pattern, so they don't race
//! sibling tests on cwd/`$HOME`), plus full-router tests proving all four routes
//! are wired.
//!
//! What: OKG triples AND definitions for each of the four reads; paging; the
//! absent-tree, unresolvable-root and malformed-TOML empty states; unknown agent
//! → 404; traversal name → 400; missing `subject` → 400; the read-only posture;
//! and `no_memory_drawer_or_palace_triple_can_reach_the_exposed_graph`, the
//! #7430 regression gate.
//! Test: This module IS the test.

use std::path::Path;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::api::server::agent_kg::{KgRead, kg_graph_at};
use crate::api::server::routes::build_router;
use crate::api::server::state::AppState;

/// An agent binding its own home-confined OKG tree (#4325's default layout).
const BOUND_FIXTURE: &str = r#"[agent]
name = "izzie"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"

[[stores]]
name = "bob-kb"
index = "bob-kb"
root = "okg"
palace = "owner-profile"
"#;

/// One test's four injected roots.
struct Fixtures {
    agents: tempfile::TempDir,
    assistants: tempfile::TempDir,
    knowledge: tempfile::TempDir,
}

impl Fixtures {
    /// Write `<agents>/<name>.toml` and return the roots. The OKG tree is NOT
    /// created — `with_tree` does that.
    fn new(name: &str, fixture: &str) -> Self {
        let agents = tempfile::tempdir().unwrap();
        std::fs::write(agents.path().join(format!("{name}.toml")), fixture).unwrap();
        Self {
            agents,
            assistants: tempfile::tempdir().unwrap(),
            knowledge: tempfile::tempdir().unwrap(),
        }
    }

    /// The OKG tree `name` resolves to under these roots.
    fn tree(&self, name: &str) -> std::path::PathBuf {
        self.assistants.path().join(name).join("okg")
    }

    /// Populate `name`'s tree with two people and one organisation.
    fn with_tree(self, name: &str) -> Self {
        let root = self.tree(name);
        write_entity(
            &root,
            "people",
            "bob",
            "---\ntype: Person\ntitle: Bob\ndescription: The owner.\nworks_at: \"[[Duetto]]\"\n---\n",
        );
        write_entity(
            &root,
            "people",
            "ada",
            "---\ntype: Person\ntitle: Ada\ndescription: A colleague.\nknows: \"[[Bob]]\"\n---\n",
        );
        write_entity(
            &root,
            "organizations",
            "duetto",
            "---\ntype: Organization\ntitle: Duetto\ndescription: A company.\n---\n",
        );
        self
    }

    async fn read(&self, name: &str, read: KgRead) -> Value {
        let resp = kg_graph_at(
            &[self.agents.path().to_path_buf()],
            name,
            Some(self.assistants.path()),
            self.knowledge.path(),
            read,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK, "expected a 200 envelope");
        body_json(resp).await
    }
}

fn write_entity(root: &Path, collection: &str, slug: &str, content: &str) {
    let dir = root.join(collection);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{slug}.md")), content).unwrap();
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn subjects_read() -> KgRead {
    KgRead::Subjects { limit: 200 }
}

// ---------------------------------------------------------------------------
// The #7430 regression gate
// ---------------------------------------------------------------------------

/// Why (#7430, epic #7425 item (f)): the owner's requirement is that the
/// exposed graph carries OKG triples and definitions and never memory content.
/// Before this change these routes called `memory.kg_subjects_with_counts`,
/// `memory.kg_all`, `memory.kg_count` and the `kg_query` tool, so every subject
/// the pane listed came out of a memory palace — this test's first assertion
/// (`data` reflecting the OKG tree) failed against that code for all four reads,
/// and so did the `definitions` assertion, which had no field to read at all.
///
/// What: the agent binds a palace AND has an OKG tree. Only the tree's content
/// may appear: the palace-only subject must be absent from every read, the
/// envelope must declare `source: "okg"`, and both halves of the graph must be
/// present. The palace named in the fixture is deliberately one a developer's
/// live trusty-memory might really hold — the point is that no daemon is
/// consulted, so its content cannot arrive however the daemon is configured.
/// Test: itself.
#[tokio::test]
async fn no_memory_drawer_or_palace_triple_can_reach_the_exposed_graph() {
    let f = Fixtures::new("izzie", BOUND_FIXTURE).with_tree("izzie");

    for read in [
        subjects_read(),
        KgRead::All {
            limit: 50,
            offset: 0,
        },
        KgRead::Subject("Bob".to_string()),
        KgRead::Count,
    ] {
        let body = f.read("izzie", read.clone()).await;
        assert_eq!(
            body["source"], "okg",
            "the envelope must name its source: {body}"
        );
        assert_eq!(body["connected"], true, "{body}");
        assert!(
            body.get("palace").is_none(),
            "a memory palace has no place in the exposed graph envelope: {body}"
        );
        let rendered = body.to_string();
        assert!(
            !rendered.contains("owner-profile"),
            "the bound PALACE id reached the payload for {read:?}: {rendered}"
        );
        assert!(
            !rendered.contains("drawer"),
            "a memory drawer reached the payload for {read:?}: {rendered}"
        );
    }

    // Both halves, on the read that carries the graph itself.
    let body = f
        .read(
            "izzie",
            KgRead::All {
                limit: 50,
                offset: 0,
            },
        )
        .await;
    assert!(
        !body["data"].as_array().unwrap().is_empty(),
        "triples must be present: {body}"
    );
    assert!(
        !body["definitions"].as_array().unwrap().is_empty(),
        "definitions must be present beside the triples: {body}"
    );
}

// ---------------------------------------------------------------------------
// Each read
// ---------------------------------------------------------------------------

#[tokio::test]
async fn kg_subjects_route_lists_okg_subjects() {
    let f = Fixtures::new("izzie", BOUND_FIXTURE).with_tree("izzie");
    let body = f.read("izzie", subjects_read()).await;
    assert_eq!(
        body["data"],
        json!([
            { "subject": "Ada", "count": 1 },
            { "subject": "Bob", "count": 1 },
            { "subject": "Duetto", "count": 0 },
        ]),
        "{body}"
    );
    assert_eq!(
        body["tree"],
        f.tree("izzie").display().to_string(),
        "the envelope names the tree it read"
    );
    assert_eq!(
        body["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|d| d["type"].as_str())
            .collect::<Vec<_>>(),
        vec!["Organization", "Person", "Person"],
    );
}

/// Why: a page of triples is only readable beside the definitions of the
/// subjects on it, so the two are selected together rather than the client
/// making a second round trip per subject.
/// Test: itself.
#[tokio::test]
async fn kg_all_route_pages_triples_with_their_definitions() {
    let f = Fixtures::new("izzie", BOUND_FIXTURE).with_tree("izzie");

    let first = f
        .read(
            "izzie",
            KgRead::All {
                limit: 1,
                offset: 0,
            },
        )
        .await;
    assert_eq!(
        first["data"],
        json!([{
            "subject": "Ada",
            "predicate": "knows",
            "object": "Bob",
            "provenance": "people/ada.md",
        }]),
        "{first}"
    );
    assert_eq!(
        first["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|d| d["subject"].as_str())
            .collect::<Vec<_>>(),
        vec!["Ada"],
        "only the page's own subjects are defined: {first}"
    );

    let second = f
        .read(
            "izzie",
            KgRead::All {
                limit: 1,
                offset: 1,
            },
        )
        .await;
    assert_eq!(second["data"][0]["subject"], "Bob", "{second}");
}

#[tokio::test]
async fn kg_query_route_returns_the_subjects_triples_and_definition() {
    let f = Fixtures::new("izzie", BOUND_FIXTURE).with_tree("izzie");
    let body = f.read("izzie", KgRead::Subject("Bob".to_string())).await;
    assert_eq!(
        body["data"],
        json!([{
            "subject": "Bob",
            "predicate": "works_at",
            "object": "Duetto",
            "provenance": "people/bob.md",
        }]),
        "{body}"
    );
    assert_eq!(body["definitions"][0]["summary"], "The owner.");
}

/// Why: the header badge reports the graph's size, and #7430 makes that both
/// halves — a tree of definitions with no edges yet is not an empty graph.
/// Test: itself.
#[tokio::test]
async fn kg_count_route_counts_both_halves() {
    let f = Fixtures::new("izzie", BOUND_FIXTURE).with_tree("izzie");
    let body = f.read("izzie", KgRead::Count).await;
    assert_eq!(body["data"], json!({ "active": 2, "definition_count": 3 }));
    assert_eq!(body["definitions"], json!([]));
}

// ---------------------------------------------------------------------------
// Empty state + degradation — none of these may be an HTTP error
// ---------------------------------------------------------------------------

#[tokio::test]
async fn kg_route_empty_state_when_the_tree_is_absent() {
    let f = Fixtures::new("izzie", BOUND_FIXTURE);
    let body = f.read("izzie", subjects_read()).await;
    assert_eq!(body["connected"], false);
    assert_eq!(body["data"], json!([]), "the empty shape is still an array");
    assert_eq!(body["definitions"], json!([]));
    assert!(
        body["reason"].as_str().unwrap().contains("no OKG tree"),
        "reason was: {}",
        body["reason"]
    );
}

#[tokio::test]
async fn kg_count_route_empty_state_keeps_object_shape() {
    // The one read whose payload is an object: a client must never have to
    // branch on `data`'s TYPE, only on `connected`.
    let f = Fixtures::new("izzie", BOUND_FIXTURE);
    let body = f.read("izzie", KgRead::Count).await;
    assert_eq!(body["connected"], false);
    assert_eq!(body["data"], json!({ "active": 0, "definition_count": 0 }));
}

/// Why: an agent that declares no `[[stores]]` still has the #4325 default tree.
/// Reporting "no store bound" there would hide a real graph.
/// Test: itself.
#[tokio::test]
async fn kg_route_reads_the_default_tree_for_an_unbound_agent() {
    let f = Fixtures::new("plain", "[agent]\nname = \"plain\"\n").with_tree("plain");
    let body = f.read("plain", subjects_read()).await;
    assert_eq!(body["connected"], true, "{body}");
    assert_eq!(body["data"][0]["subject"], "Ada");
}

/// Why: the binding a malformed `agent.toml` failed to parse is what names which
/// tree to show, so guessing one could render another assistant's graph. A
/// hand-edit typo degrades to an empty state, never a 500 and never a guess.
/// Test: itself.
#[tokio::test]
async fn kg_route_degrades_on_malformed_toml() {
    let f = Fixtures::new("broken", "not = = toml");
    let body = f.read("broken", subjects_read()).await;
    assert!(body["tree"].is_null());
    assert_eq!(body["connected"], false);
    assert!(body["config_error"].is_string());
}

/// Why: a binding naming a tree URI nothing can resolve is a configuration
/// fault, and the reason must say so rather than showing an empty graph that
/// looks like "nothing ingested".
/// Test: itself.
#[tokio::test]
async fn kg_route_degrades_on_an_unresolvable_tree_uri() {
    let f = Fixtures::new(
        "ghosty",
        "[agent]\nname = \"ghosty\"\n\n[[stores]]\nname = \"g\"\ntree = \"https://example.com/kb\"\n",
    );
    let body = f.read("ghosty", subjects_read()).await;
    assert_eq!(body["connected"], false);
    assert!(
        body["reason"]
            .as_str()
            .unwrap()
            .contains("does not resolve"),
        "reason was: {}",
        body["reason"]
    );
}

// ---------------------------------------------------------------------------
// Client-side faults that DO keep a non-200
// ---------------------------------------------------------------------------

#[tokio::test]
async fn kg_route_unknown_agent_404() {
    let dir = tempfile::tempdir().unwrap();
    let roots = tempfile::tempdir().unwrap();
    let resp = kg_graph_at(
        &[dir.path().to_path_buf()],
        "nobody",
        Some(roots.path()),
        roots.path(),
        subjects_read(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn kg_route_rejects_traversal_name() {
    let dir = tempfile::tempdir().unwrap();
    let roots = tempfile::tempdir().unwrap();
    let resp = kg_graph_at(
        &[dir.path().to_path_buf()],
        "../etc",
        Some(roots.path()),
        roots.path(),
        subjects_read(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn kg_query_route_requires_subject() {
    // A missing `subject` must be a loud 400, never a silent full-graph fetch.
    let app: Router = build_router(AppState::default());
    let req = Request::builder()
        .uri("/api/agents/definitely-not-an-agent-4290/kg")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert!(
        body["error"].as_str().unwrap().contains("subject"),
        "error was: {}",
        body["error"]
    );
}

// ---------------------------------------------------------------------------
// Router wiring + read-only posture
// ---------------------------------------------------------------------------

/// Every one of the four routes is reachable through `build_router` — an
/// unknown agent must 404 FROM THE HANDLER (`{"error": "unknown agent"}`),
/// not from an unrouted path.
#[tokio::test]
async fn kg_routes_are_wired_into_router() {
    for path in [
        "/api/agents/definitely-not-an-agent-4290/kg?subject=x",
        "/api/agents/definitely-not-an-agent-4290/kg/subjects",
        "/api/agents/definitely-not-an-agent-4290/kg/all",
        "/api/agents/definitely-not-an-agent-4290/kg/count",
    ] {
        let app: Router = build_router(AppState::default());
        let req = Request::builder().uri(path).body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "for {path}");
        let body = body_json(resp).await;
        assert_eq!(body["error"], "unknown agent", "for {path}");
    }
}

/// Owner decision (#4290): the exposed graph is READ-ONLY. Writing an entity has
/// its own confined OKG ingest tools, so those verbs must not resolve here.
#[tokio::test]
async fn kg_write_verbs_are_not_proxied() {
    for (method, path) in [
        (Method::POST, "/api/agents/izzie/kg"),
        (Method::DELETE, "/api/agents/izzie/kg/triples/abc"),
    ] {
        let app: Router = build_router(AppState::default());
        let req = Request::builder()
            .method(method.clone())
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert!(
            resp.status() == StatusCode::METHOD_NOT_ALLOWED
                || resp.status() == StatusCode::NOT_FOUND
                || resp.status() == StatusCode::FORBIDDEN,
            "{method} {path} resolved to a handler ({}) — the KG route must stay read-only",
            resp.status()
        );
    }
}
