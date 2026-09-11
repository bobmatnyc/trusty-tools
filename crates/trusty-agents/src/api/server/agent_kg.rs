//! `GET /api/agents/:name/kg*` — the agent's EXPOSED knowledge graph, read
//! READ-ONLY out of its OKG tree (#4290, #6286, #7430).
//!
//! Why: these four routes back the Knowledge-Graph browser in the agents GUI.
//! Until #7430 they proxied trusty-memory's palace-scoped `kg_*` surface, so the
//! pane labelled "Knowledge Graph" showed the MEMORY knowledge graph — triples
//! asserted into a memory palace, a separate store with a separate lifecycle.
//! Epic #7425 item (f) settles what the exposed graph is: the OKG graph, triples
//! AND definitions, out of the assistant's own `okg/` tree, and never a memory
//! palace. This module now reads that tree and holds no memory client at all, so
//! nothing here can reach a palace even by mistake.
//!
//! **This module holds no graph logic of its own.**
//! [`crate::stores::okg_graph`] is the single reader; these handlers page and
//! envelope what it returns.
//!
//! **Read-only by owner decision (#4290).** Nothing here writes an entity; OKG
//! ingest has its own tools (`okg_ingest_*`) with their own confinement.
//!
//! **Never-fail posture** (identical contract to
//! [`crate::stores::resolve_store_statuses`], which this module deliberately
//! mirrors rather than inventing a second error vocabulary): an agent whose tree
//! does not exist yet, a tree that cannot be read, and an `agent.toml` that does
//! not parse ALL resolve to `200 OK` with `connected: false` plus a
//! machine-readable `reason` and an empty-but-well-typed `data`. A browser pane
//! must render an empty state, not an error toast, for the ordinary condition
//! "this assistant has ingested nothing yet". Only genuinely client-side faults
//! keep a non-200: `400` for an invalid agent name or a missing `subject`, `404`
//! for an unknown agent, `500` only when the agent's own config file cannot be
//! read off disk.
//!
//! What: four thin axum shims ([`agent_kg_subjects_route`],
//! [`agent_kg_all_route`], [`agent_kg_query_route`], [`agent_kg_count_route`])
//! over one testable core, [`kg_graph_at`], which takes the agents-dir list, the
//! assistants root and the knowledge dir explicitly (the injected-dependency
//! convention of `agent_stores::stores_at`). Response envelope, identical for
//! all four routes:
//!
//! ```json
//! { "tree": "izzie/okg", "source": "okg", "connected": true,
//!   "data": <per-route>, "definitions": [ … ] }
//! { "tree": null, "source": "okg", "connected": false, "reason": "…",
//!   "data": [], "definitions": [] }
//! ```
//!
//! plus `config_error` when the agent's `agent.toml` failed to parse. `data` is
//! an array for `/kg`, `/kg/subjects` and `/kg/all`, and the counts object for
//! `/kg/count`; its empty form (`[]` / `{"active": 0, "definition_count": 0}`)
//! is preserved on every degraded path so a client never has to branch on the
//! payload's TYPE, only on `connected`. `definitions` is ALWAYS an array — it is
//! the half of the graph that says what a subject is, and the owner's closure
//! condition for #7430 requires it beside the triples.
//!
//! **`tree` is an opaque LABEL, never a filesystem path** (#7430 security
//! review): the binding's own `okg://<agent>` URI, or the home-relative
//! `<agent>/<root>`. Every `reason` obeys the same rule, and the underlying
//! error text is logged instead. Serving the absolute root would hand every
//! viewer of the pane the operator's home-directory layout, which the retired
//! memory-palace envelope never disclosed and no client needs.
//! Test: `kg_subjects_route_lists_okg_subjects`,
//! `no_memory_drawer_or_palace_triple_can_reach_the_exposed_graph`.

use std::path::{Path, PathBuf};

use axum::{
    Json,
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::agent_patch::resolve_agent_paths;
use super::state::AppState;
use crate::stores::StoresConfig;
use crate::stores::okg_graph::{self, OkgDefinition, OkgGraph};

/// Page size when the caller names none.
const DEFAULT_KG_LIMIT: usize = 50;
/// Page-size ceiling. Mirrors the retired upstream's `MAX_KG_LIST_LIMIT` so a
/// client that already clamped to 200 keeps working unchanged.
const MAX_KG_LIMIT: usize = 200;

/// Query parameters accepted across all four KG routes.
#[derive(Debug, Default, Deserialize)]
pub(super) struct KgParams {
    limit: Option<usize>,
    offset: Option<usize>,
    subject: Option<String>,
}

impl KgParams {
    /// The clamped page size.
    fn limit(&self) -> usize {
        self.limit
            .unwrap_or(DEFAULT_KG_LIMIT)
            .clamp(1, MAX_KG_LIMIT)
    }

    /// The page offset.
    fn offset(&self) -> usize {
        self.offset.unwrap_or(0)
    }
}

/// One read of the exposed graph, resolved from the route that was hit.
///
/// `pub(super)` so `super::tests::agent_kg` can drive [`kg_graph_at`] with an
/// arbitrary read without a test-only shim on the production type.
#[derive(Debug, Clone)]
pub(super) enum KgRead {
    /// Every subject with its triple count, paged.
    Subjects { limit: usize },
    /// Every triple, paged.
    All { limit: usize, offset: usize },
    /// One subject's triples, unpaged.
    Subject(String),
    /// Graph totals.
    Count,
}

impl KgRead {
    /// The `data` shape this read reports on every degraded path.
    fn empty(&self) -> Value {
        match self {
            Self::Count => json!({ "active": 0, "definition_count": 0 }),
            _ => json!([]),
        }
    }

    /// Project a graph into this read's `data` plus its accompanying
    /// definitions.
    ///
    /// Why: the two halves are selected together — a page of triples is only
    /// readable beside the definitions of the subjects on it, and shipping every
    /// definition with every page would send the whole tree four times.
    /// What: `(data, definitions)`. `Count` reports totals and no definition
    /// bodies, because a count pane renders neither.
    /// Test: `kg_all_route_pages_triples_with_their_definitions`,
    /// `kg_count_route_counts_both_halves`.
    fn project(&self, graph: &OkgGraph) -> (Value, Vec<OkgDefinition>) {
        match self {
            Self::Subjects { limit } => {
                let page: Vec<_> = graph.subject_counts().into_iter().take(*limit).collect();
                let subjects: Vec<String> = page.iter().map(|c| c.subject.clone()).collect();
                (json!(page), graph.definitions_for(&subjects))
            }
            Self::All { limit, offset } => {
                let page: Vec<_> = graph
                    .triples
                    .iter()
                    .skip(*offset)
                    .take(*limit)
                    .cloned()
                    .collect();
                let subjects: Vec<String> = page.iter().map(|t| t.subject.clone()).collect();
                (json!(page), graph.definitions_for(&subjects))
            }
            Self::Subject(subject) => {
                let page: Vec<_> = graph
                    .triples
                    .iter()
                    .filter(|t| &t.subject == subject)
                    .cloned()
                    .collect();
                (
                    json!(page),
                    graph.definitions_for(std::slice::from_ref(subject)),
                )
            }
            Self::Count => (
                json!({
                    "active": graph.triples.len(),
                    "definition_count": graph.definitions.len(),
                }),
                Vec::new(),
            ),
        }
    }
}

/// `GET /api/agents/:name/kg/subjects?limit=N` — HTTP entry point.
///
/// Why: the browser's left-hand list needs a count badge per subject, and needs
/// to list an entity that has a definition but no edges yet.
/// What: `data` is `[{subject, count}, …]`; `definitions` carries those
/// subjects' definitions.
/// Test: `kg_subjects_route_lists_okg_subjects`.
// #7430: per-agent entry onto the OKG tree's subject list (was trusty-memory's
// palace-scoped list).
pub(super) async fn agent_kg_subjects_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
    Query(p): Query<KgParams>,
) -> Response {
    graph_at_defaults(&name, KgRead::Subjects { limit: p.limit() }).await
}

/// `GET /api/agents/:name/kg/all?limit=N&offset=N` — HTTP entry point.
///
/// Why: the browser's "All" mode pages across every triple regardless of
/// subject.
/// What: `data` is `[triple, …]` for that page; `definitions` carries the
/// definitions of the subjects appearing on it.
/// Test: `kg_all_route_pages_triples_with_their_definitions`.
// #7430: per-agent entry onto the OKG tree's triples.
pub(super) async fn agent_kg_all_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
    Query(p): Query<KgParams>,
) -> Response {
    graph_at_defaults(
        &name,
        KgRead::All {
            limit: p.limit(),
            offset: p.offset(),
        },
    )
    .await
}

/// `GET /api/agents/:name/kg?subject=<s>` — HTTP entry point.
///
/// Why: the detail pane fetches one subject's edges after a click, which is far
/// cheaper than filtering a full `/kg/all` page client-side.
/// What: `data` is that subject's `[triple, …]`; `definitions` is its
/// definition, or empty when the subject is only ever an edge TARGET. `subject`
/// is required, and its absence is a `400` — never a silent full-graph fetch.
/// Test: `kg_query_route_returns_the_subjects_triples_and_definition`,
/// `kg_query_route_requires_subject`.
// #7430: per-agent entry onto one OKG subject.
pub(super) async fn agent_kg_query_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
    Query(p): Query<KgParams>,
) -> Response {
    let Some(subject) = p.subject.filter(|s| !s.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "the `subject` query parameter is required" })),
        )
            .into_response();
    };
    graph_at_defaults(&name, KgRead::Subject(subject)).await
}

/// `GET /api/agents/:name/kg/count` — HTTP entry point.
///
/// Why: the browser header shows an "N triples" badge.
/// What: `data` is `{"active": N, "definition_count": D}` — the ONE route whose
/// payload is an object rather than an array, and the one that reports both
/// halves as totals.
/// Test: `kg_count_route_counts_both_halves`.
// #7430: per-agent entry onto the OKG tree's totals.
pub(super) async fn agent_kg_count_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    graph_at_defaults(&name, KgRead::Count).await
}

/// Shared shim body: resolve the roots the same way every other OKG surface in
/// this crate does, then delegate.
async fn graph_at_defaults(name: &str, read: KgRead) -> Response {
    kg_graph_at(
        &crate::agents::agents_dir_candidates(),
        name,
        crate::assistants::assistants_root().ok().as_deref(),
        &crate::tools::okg::knowledge_dir(),
        read,
    )
    .await
}

/// Core read against explicit agents dirs, assistants root and knowledge dir.
///
/// Why: same testability rationale as `agent_stores::stores_at` — the roots are
/// injected so tests point at a tempdir instead of the developer's live home.
/// What: resolves the agent's OKG tree from `StoresConfig::primary()`, reads the
/// graph, and wraps the result in the module doc's envelope. `400` invalid name,
/// `404` unknown agent, `500` only when the agent's own config cannot be read;
/// every tree/read failure is a `200` carrying `connected: false` + `reason`. A
/// malformed `agent.toml` degrades rather than guessing a tree, because the
/// binding it failed to parse is what names which tree to show.
/// Test: `kg_subjects_route_lists_okg_subjects`,
/// `kg_route_empty_state_when_the_tree_is_absent`,
/// `kg_route_unknown_agent_404`, `kg_route_rejects_traversal_name`,
/// `kg_route_degrades_on_malformed_toml`,
/// `no_envelope_discloses_a_filesystem_path`.
pub(super) async fn kg_graph_at(
    dirs: &[PathBuf],
    name: &str,
    assistants_root: Option<&Path>,
    knowledge_dir: &Path,
    read: KgRead,
) -> Response {
    if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid agent name" })),
        )
            .into_response();
    }
    let Some((path, _package_dir)) = resolve_agent_paths(dirs, name) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown agent", "name": name })),
        )
            .into_response();
    };
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(?e, agent = name, path = %path.display(), "kg_graph_at: read failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to read agent config" })),
            )
                .into_response();
        }
    };

    let (stores, config_error) = parse_stores(&raw);
    if let Some(err) = config_error {
        return envelope(
            None,
            Err(format!(
                "this agent's `agent.toml` does not parse, so the tree it binds is unknown: {err}"
            )),
            &read,
            Some(err),
        );
    }
    let Some(assistants_root) = assistants_root else {
        return envelope(
            None,
            Err("the assistants root is not resolvable (no home directory)".to_string()),
            &read,
            None,
        );
    };

    let tree = match okg_graph::resolve_okg_root(name, &stores, assistants_root, knowledge_dir) {
        Ok(tree) => tree,
        Err(reason) => return envelope(None, Err(reason), &read, None),
    };
    // #7430: the envelope carries the binding's own opaque label, never
    // `tree.root` — an absolute path would disclose the operator's home layout
    // to every viewer of the pane. The same rule governs every `reason` below,
    // so the upstream error text is logged rather than rendered.
    let label = Some(tree.label.clone());
    if !tree.root.is_dir() {
        return envelope(
            label,
            Err(format!(
                "this assistant has no OKG tree `{}` yet — nothing has been ingested",
                tree.label
            )),
            &read,
            None,
        );
    }
    match okg_graph::read_graph(&tree.root) {
        Ok(graph) => envelope(label, Ok(read.project(&graph)), &read, None),
        Err(e) => {
            tracing::warn!(?e, agent = name, tree = %tree.label, "kg_graph_at: tree unreadable");
            envelope(
                label,
                Err(format!("the OKG tree `{}` is unreadable", tree.label)),
                &read,
                None,
            )
        }
    }
}

/// Build the module doc's response envelope.
fn envelope(
    tree: Option<String>,
    result: Result<(Value, Vec<OkgDefinition>), String>,
    read: &KgRead,
    config_error: Option<String>,
) -> Response {
    let mut body = match result {
        Ok((data, definitions)) => json!({
            "tree": tree,
            "source": GRAPH_SOURCE,
            "connected": true,
            "data": data,
            "definitions": definitions,
        }),
        Err(reason) => json!({
            "tree": tree,
            "source": GRAPH_SOURCE,
            "connected": false,
            "reason": reason,
            "data": read.empty(),
            "definitions": [],
        }),
    };
    if let Some(err) = config_error {
        body["config_error"] = Value::String(err);
    }
    (StatusCode::OK, Json(body)).into_response()
}

/// What the exposed graph is made of, stated in the payload itself.
///
/// Why (#7430): the owner's requirement is a property of the DATA, not of the
/// route's name. A client, a test, or a future reader can assert on this rather
/// than inferring the source from which daemon happened to answer.
pub(super) const GRAPH_SOURCE: &str = "okg";

/// Parse just the `[[stores]]` table out of a raw `agent.toml`.
///
/// Identical partial-read rationale to `agent_stores::parse_stores`: a
/// directory-package `agent.toml` omits `[system_prompt]`, so reading through
/// the full `AgentConfig` would reject it.
// #4278: shared with `chat_history`, which needs the same partial read.
pub(super) fn parse_stores(raw: &str) -> (StoresConfig, Option<String>) {
    #[derive(Deserialize)]
    struct Partial {
        #[serde(default)]
        stores: StoresConfig,
    }
    match toml::from_str::<Partial>(raw) {
        Ok(p) => (p.stores, None),
        Err(e) => (StoresConfig::default(), Some(e.to_string())),
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    /// Why: the caller's page size is user input reaching a `take`. A zero would
    /// render an empty pane on a non-empty tree, and an unbounded value would
    /// materialise the whole tree into one response.
    /// Test: itself.
    #[test]
    fn limit_is_clamped_and_defaulted() {
        assert_eq!(KgParams::default().limit(), DEFAULT_KG_LIMIT);
        assert_eq!(
            KgParams {
                limit: Some(0),
                ..Default::default()
            }
            .limit(),
            1
        );
        assert_eq!(
            KgParams {
                limit: Some(10_000),
                ..Default::default()
            }
            .limit(),
            MAX_KG_LIMIT
        );
    }

    /// Why: a client must branch only on `connected`, never on `data`'s TYPE.
    /// The count read is the one whose payload is an object, so its empty shape
    /// must stay an object.
    /// Test: itself.
    #[test]
    fn empty_shapes_keep_each_reads_type() {
        assert!(KgRead::Subjects { limit: 1 }.empty().is_array());
        assert!(
            KgRead::All {
                limit: 1,
                offset: 0
            }
            .empty()
            .is_array()
        );
        assert!(KgRead::Subject("x".into()).empty().is_array());
        assert_eq!(
            KgRead::Count.empty(),
            json!({ "active": 0, "definition_count": 0 })
        );
    }

    #[test]
    fn parse_stores_reads_the_primary_binding() {
        let raw = "[agent]\nname = \"izzie\"\n\n[[stores]]\nname = \"bob-kb\"\nroot = \"okg\"\n";
        let (stores, err) = parse_stores(raw);
        assert!(err.is_none());
        assert_eq!(
            stores.primary().and_then(|b| b.root.as_deref()),
            Some("okg")
        );
    }

    #[test]
    fn parse_stores_reports_bad_toml_without_a_binding() {
        let (stores, err) = parse_stores("not = = toml");
        assert!(stores.primary().is_none());
        assert!(err.is_some());
    }
}
