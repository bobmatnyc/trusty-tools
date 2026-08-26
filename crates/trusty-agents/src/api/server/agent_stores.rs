//! `GET /api/agents/:name/stores` — the agent's OKG store bindings, resolved
//! live against the running daemons (#3816/#3864, epic #3052).
//!
//! Why: The gear panel's "OKG Stores" tab previously rendered a hardcoded
//! client-side placeholder ("agent.toml has no OKG store binding field yet")
//! because there was neither a config field nor a route to read one. This is
//! the read side of that field: it parses the agent's `[[stores]]` bindings
//! off disk and asks trusty-search / trusty-memory whether each one actually
//! exists, so the card can say CONNECTED with a chunk count or NOT CONNECTED
//! with a reason instead of asserting the same thing for every agent.
//! What: [`agent_stores_route`] is the axum shim; [`stores_at`] is the
//! testable core taking the agents-dir list and both daemon base URLs
//! explicitly (same injected-dependency convention as `agent_patch::*_at` and
//! `workstreams::list_workstreams_at`). Response shape:
//! `{"stores": [StoreStatus, …], "issues": […], "search_indexes": […],
//! "enforce_search_indexes": bool}` — empty arrays for an agent that binds
//! nothing, which is a valid state, not an error. `search_indexes` /
//! `enforce_search_indexes` (#3232/#4009, epic #4007) are the second,
//! UNCURATED knowledge tier: arbitrary trusty-search indexes attached to the
//! agent, reported next to the curated OKG stores rather than mixed into
//! them. Those two fields are an INTERIM RAW surface licensed by DOC-58 §5 —
//! the resolved declared list and the knob state, with NO daemon probe behind
//! either. The future `/knowledge.attached_indexes[]` route is the probed,
//! authoritative surface for the GUI; both must report the same id set, which
//! is why the overlap rule ([`attached_indexes_deduped`]) is applied
//! server-side here. A daemon being down is
//! likewise reported per-store, never as a route failure: this endpoint
//! returns `200` whenever the agent resolves at all.
//! Test: `super::tests::agent_stores`.

use std::path::PathBuf;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};

use super::agent_patch::resolve_agent_paths;
use super::state::AppState;
use crate::agents::ToolsConfig;
use crate::stores::{StoresConfig, resolve_store_statuses};

/// `GET /api/agents/:name/stores` — HTTP entry point.
///
/// Why/What: see the module doc. trusty-search's base URL comes from the
/// `http_addr` convention it still writes; trusty-memory's socket is derived by
/// the same `daemon_socket_path` call the daemon makes (#6286). Both are
/// threaded into [`stores_at`] so tests can substitute a mock.
/// Test: `super::tests::agent_stores::stores_route_reports_connected_binding_with_stats`.
pub(super) async fn agent_stores_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    stores_at(
        &crate::agents::agents_dir_candidates(),
        &name,
        trusty_common::resolve_daemon_base_url("trusty-search").as_deref(),
        trusty_common::memory_rpc::resolve_memory_socket()
            .ok()
            .as_deref(),
    )
    .await
}

/// Maximum accepted agent-name length. Matches `agent_create::is_valid_slug`'s
/// cap, so a name this route accepts is one the create path could have written.
const MAX_AGENT_NAME_LEN: usize = 64;

/// Is `name` a safe single path segment to join onto an agents directory?
///
/// Why: this used to be a DENYLIST — `/`, `\`, `.`, `..` (audit 2026-08-19,
/// finding 12). A denylist has to anticipate every hostile spelling, and the
/// name is joined straight onto a filesystem path by `resolve_agent_paths`.
/// It admitted NUL bytes, control characters, whitespace-only names,
/// unbounded lengths, and Unicode look-alikes for the separators it did
/// reject (`＼` U+FF3C, `∕` U+2215, `․` U+2024) — every one of which reaches
/// the join. An allowlist inverts the burden: only characters that can never
/// change a path's meaning get through, so an unanticipated spelling fails
/// closed.
/// What: non-empty, at most [`MAX_AGENT_NAME_LEN`] bytes, and every character
/// is ASCII alphanumeric, `_`, or `-`. That admits every bundled agent name
/// (see `agent_name_allowlist_accepts_every_bundled_agent`) and every name
/// `POST /api/agents` can create.
/// Test: `agent_name_allowlist_rejects_traversal_and_unicode_tricks`,
/// `agent_name_allowlist_accepts_every_bundled_agent`.
pub(super) fn is_valid_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_AGENT_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Core store-resolution logic against explicit agents dirs + daemon URLs.
///
/// Why: Same testability rationale as `agent_patch::patch_agent_at`.
/// What: `400` for an invalid name, `404` for an unknown agent, `500` only
/// when the resolved config file cannot be read. Malformed TOML is NOT a
/// `500` here — it degrades to "no bindings" plus a `config_error` field, so
/// a hand-edited file breaking the stores table still lets the panel render
/// the rest of the agent. Every per-store failure is inside the `StoreStatus`
/// entries, not the HTTP status.
/// Test: `stores_route_reports_connected_binding_with_stats`, `stores_route_unknown_agent_404`,
/// `stores_route_empty_for_unbound_agent`,
/// `stores_route_degrades_on_malformed_toml`.
pub(super) async fn stores_at(
    dirs: &[PathBuf],
    name: &str,
    search_base: Option<&str>,
    memory_socket: Option<&std::path::Path>,
) -> Response {
    if !is_valid_agent_name(name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid agent name" })),
        )
            .into_response();
    }
    let Some((path, _package_dir)) = resolve_agent_paths(dirs, name) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "unknown agent", "name": name })),
        )
            .into_response();
    };
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(?e, agent = name, path = %path.display(), "stores_at: read failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "failed to read agent config" })),
            )
                .into_response();
        }
    };

    let (stores, tools, config_error) = parse_stores(&raw);
    let statuses = resolve_store_statuses(name, &stores, search_base, memory_socket).await;
    let mut body = serde_json::json!({
        "stores": statuses,
        "issues": stores.validate(),
        // #3232/#4009 (epic #4007): the agent's TIER-2 knowledge — arbitrary
        // trusty-search indexes attached via `[tools].search_indexes`,
        // deliberately reported alongside (not inside) `stores`, because they
        // carry none of a store's structure: no OKG tree, no palace, no
        // curation. `enforce_search_indexes` reports whether that list gates
        // `vector_search` or is declarative-only (the default), so a client
        // never has to infer the posture from the list's presence.
        //
        // DOC-58 §5 (interim raw surface): both fields are RAW and
        // non-authoritative — the declared list verbatim, with NO daemon
        // probe. The future `/knowledge.attached_indexes[]` route is the
        // probed, authoritative surface for the GUI; the two id sets must
        // stay consistent, which is why the dedupe rule lives here rather
        // than in each client.
        "search_indexes": attached_indexes_deduped(&stores, &tools),
        "enforce_search_indexes": tools.search_indexes_enforced(),
    });
    if let Some(err) = config_error {
        body["config_error"] = serde_json::Value::String(err);
    }
    (StatusCode::OK, Json(body)).into_response()
}

/// The attached tier-2 indexes MINUS any id already bound as an OKG store
/// (#3232/#4009, DOC-58 §5's dedupe-by-id rule).
///
/// Why: Declaring the same index id in BOTH `[[stores]]` and
/// `[tools].search_indexes` is legal — an operator may reasonably restate the
/// bound index in the attach list, and enforcement (#4009) accepts it from
/// either tier. But this response must present each id exactly ONCE, under
/// its BOUND role: an id that is a curated store is a store, and echoing it
/// again as an uncurated attachment would make the GUI render the same corpus
/// twice with two different provenances. `VectorSearchTool::allowed_index_ids`
/// resolves the identical overlap the identical way (bound first, deduped), so
/// what this route reports and what the tool accepts cannot disagree.
/// What: `ToolsConfig::resolved_search_indexes` (already trimmed/deduped)
/// filtered against every binding's `resolved_index`.
///
/// `pub(super)` since #3935: `agent_knowledge::knowledge_at` reports the
/// identical id set under the identical field name (`search_indexes`) so the
/// two routes can never disagree about which ids are curated vs. attached.
/// Test: `attached_indexes_deduped_presents_overlap_under_its_bound_role`.
pub(super) fn attached_indexes_deduped(stores: &StoresConfig, tools: &ToolsConfig) -> Vec<String> {
    tools
        .resolved_search_indexes()
        .into_iter()
        .filter(|id| {
            !stores
                .bindings
                .iter()
                .any(|b| b.resolved_index() == id.as_str())
        })
        .collect()
}

/// Parse just the `[[stores]]` table out of a raw `agent.toml`.
///
/// Why: Reading the whole file through `AgentConfig` would require `[llm]`
/// and `[system_prompt]` to be present and valid — true for a flat agent but
/// NOT for a directory package, whose `agent.toml` deliberately omits
/// `system_prompt.content` (it lives in `persona.md`). Deserializing only the
/// field this endpoint needs keeps the route working for both file layouts,
/// matching `parse_agent_toml`'s partial-read precedent.
/// What: `(bindings, tools, None)` on success; `(empty, empty, Some(message))`
/// when the file isn't valid TOML or its `stores` value has an unusable shape.
/// #3232: `[tools]` rides along in the same partial read because the tier-2
/// attached indexes live there, and this endpoint reports BOTH tiers — one
/// parse, one place for the two to disagree about the same file.
/// Test: `parse_stores_reads_array_form`, `parse_stores_reports_bad_toml`,
/// `parse_stores_reads_attached_search_indexes`.
fn parse_stores(raw: &str) -> (StoresConfig, ToolsConfig, Option<String>) {
    #[derive(serde::Deserialize)]
    struct Partial {
        #[serde(default)]
        stores: StoresConfig,
        #[serde(default)]
        tools: ToolsConfig,
    }
    match toml::from_str::<Partial>(raw) {
        Ok(p) => (p.stores, p.tools, None),
        Err(e) => (
            StoresConfig::default(),
            ToolsConfig::default(),
            Some(e.to_string()),
        ),
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn parse_stores_reads_array_form() {
        let raw = r#"
[agent]
name = "izzie"

[[stores]]
name = "bob-kb"
index = "bob-kb"
palace = "owner-profile"
"#;
        let (stores, _tools, err) = parse_stores(raw);
        assert!(err.is_none());
        assert_eq!(stores.default_search_index(), Some("bob-kb"));
    }

    #[test]
    fn parse_stores_ignores_other_sections() {
        // A package `agent.toml` has no `[system_prompt]` — parsing must not
        // require one (see this fn's doc comment).
        let raw = "[agent]\nname = \"x\"\n[tools]\nallow = [\"a\"]\n";
        let (stores, _tools, err) = parse_stores(raw);
        assert!(err.is_none());
        assert!(stores.bindings.is_empty());
    }

    /// #3232/#4009: the Knowledge endpoint reports BOTH tiers off one parse —
    /// the curated `[[stores]]` binding and the arbitrary attached indexes —
    /// and an agent may declare either without the other.
    #[test]
    fn parse_stores_reads_attached_search_indexes() {
        let raw = r#"
[agent]
name = "cto-assistant"

[[stores]]
name = "cto-assistant"

[tools]
search_indexes = ["apex", "  ", "apex"]
enforce_search_indexes = true
"#;
        let (stores, tools, err) = parse_stores(raw);
        assert!(err.is_none());
        assert_eq!(stores.default_search_index(), Some("cto-assistant"));
        assert_eq!(tools.resolved_search_indexes(), vec!["apex".to_string()]);
        assert!(tools.search_indexes_enforced());

        // Tier-2 without tier-1, and the unattached default.
        let (stores, tools, err) =
            parse_stores("[agent]\nname = \"x\"\n[tools]\nsearch_indexes = [\"apex\"]\n");
        assert!(err.is_none());
        assert!(stores.bindings.is_empty());
        assert_eq!(tools.resolved_search_indexes(), vec!["apex".to_string()]);
        assert!(!tools.search_indexes_enforced());
    }

    /// DOC-58 §5 dedupe-by-id: an id declared in BOTH tiers is legal and is
    /// presented ONCE, under its bound role — it stays in `stores` and is
    /// filtered out of `search_indexes`, so the GUI never renders the same
    /// corpus twice with two provenances.
    #[test]
    fn attached_indexes_deduped_presents_overlap_under_its_bound_role() {
        let raw = r#"
[agent]
name = "cto-assistant"

[[stores]]
name = "cto-assistant"

[tools]
search_indexes = ["cto-assistant", "apex"]
"#;
        let (stores, tools, err) = parse_stores(raw);
        assert!(
            err.is_none(),
            "restating the bound index is legal, not an error"
        );
        assert_eq!(stores.default_search_index(), Some("cto-assistant"));
        assert_eq!(
            attached_indexes_deduped(&stores, &tools),
            vec!["apex".to_string()],
            "the bound id is presented under its store role, not echoed as an attachment"
        );
        // The overlap is still queryable — the tool's allowlist accepts it
        // from the bound tier, so the two surfaces agree on the id SET.
        let allowed = crate::tools::memory::VectorSearchTool::new()
            .with_default_index(stores.default_search_index().map(str::to_string))
            .with_attached_indexes(tools.resolved_search_indexes())
            .allowed_index_ids();
        assert_eq!(
            allowed,
            vec!["cto-assistant".to_string(), "apex".to_string()]
        );

        // A binding whose explicit `index` differs from its `name` dedupes on
        // the RESOLVED index, which is the id `vector_search` actually sends.
        let raw = r#"
[agent]
name = "x"

[[stores]]
name = "store-label"
index = "real-index"

[tools]
search_indexes = ["real-index", "store-label"]
"#;
        let (stores, tools, _) = parse_stores(raw);
        assert_eq!(
            attached_indexes_deduped(&stores, &tools),
            vec!["store-label".to_string()],
            "dedupe keys on resolved_index, not the store's display name"
        );
    }

    /// audit 2026-08-19 (finding 12): the name reaches `dir.join(name)`, so
    /// anything that can alter a path's meaning must fail closed.
    ///
    /// Why: the previous denylist named four spellings and let everything else
    /// through. These cases are the ones it missed — Unicode separator
    /// look-alikes, percent-encoded traversal, NUL and control bytes,
    /// whitespace-only names, and an unbounded length — alongside the four it
    /// did catch, so neither class can regress.
    /// Test: itself.
    #[test]
    fn agent_name_allowlist_rejects_traversal_and_unicode_tricks() {
        let rejected = [
            // What the old denylist already caught.
            "",
            ".",
            "..",
            "../etc/passwd",
            "a/b",
            "a\\b",
            // What it did not.
            "\u{FF3C}etc",      // fullwidth reverse solidus
            "a\u{2215}b",       // division slash
            "a\u{2044}b",       // fraction slash
            "a\u{2024}b",       // one-dot leader
            "..%2fetc",         // percent-encoded traversal
            "%2e%2e%2fetc",     // fully encoded `../`
            "izzie\0",          // NUL byte
            "izzie\n",          // control character
            "izzie ",           // trailing space
            "   ",              // whitespace only
            "izzie.toml",       // extension smuggling
            "cto\u{2011}agent", // non-breaking hyphen
            "\u{0430}ssistant", // Cyrillic а homoglyph
        ];
        for name in rejected {
            assert!(
                !is_valid_agent_name(name),
                "must reject {name:?} — it is joined onto a filesystem path"
            );
        }
        assert!(
            !is_valid_agent_name(&"a".repeat(MAX_AGENT_NAME_LEN + 1)),
            "length must be bounded"
        );
        assert!(is_valid_agent_name(&"a".repeat(MAX_AGENT_NAME_LEN)));
    }

    /// audit 2026-08-19 (finding 12): tightening to an allowlist must not lock
    /// any shipped agent out of its own stores panel.
    ///
    /// Why: a validator that rejects real names is a worse outage than the
    /// traversal it prevents. Reading the roster off disk (rather than pasting
    /// a list here) means a newly bundled agent whose name the allowlist would
    /// reject fails this test the day it is added.
    /// What: walks the bundled `.trusty-agents/agents/` directory, deriving the
    /// name the router sees for both layouts `resolve_agent_paths` supports —
    /// a directory package (`<name>/agent.toml`) and a flat `<name>.toml`.
    /// Test: itself.
    #[test]
    fn agent_name_allowlist_accepts_every_bundled_agent() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".trusty-agents")
            .join("agents");
        let mut checked = 0usize;
        for entry in std::fs::read_dir(&dir).expect("bundled agents dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            let name = if path.is_dir() {
                if !path.join("agent.toml").is_file() {
                    continue;
                }
                path.file_name()
                    .expect("dir name")
                    .to_string_lossy()
                    .into_owned()
            } else if path.extension().is_some_and(|e| e == "toml") {
                path.file_stem()
                    .expect("file stem")
                    .to_string_lossy()
                    .into_owned()
            } else {
                continue;
            };
            assert!(
                is_valid_agent_name(&name),
                "bundled agent {name:?} must remain reachable"
            );
            checked += 1;
        }
        assert!(
            checked >= 20,
            "expected the full bundled roster, only saw {checked} names in {}",
            dir.display()
        );
    }

    #[test]
    fn parse_stores_reports_bad_toml() {
        let (stores, _tools, err) = parse_stores("this is not = = toml");
        assert!(stores.bindings.is_empty());
        assert!(err.is_some());
    }
}
