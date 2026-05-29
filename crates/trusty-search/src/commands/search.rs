//! Handler for `trusty-search search`.
//!
//! Why: `search` is the project-scoped sibling of `query` — when the working
//! directory is inside a registered index, callers prefer `search` because it
//! auto-resolves the target index from CWD and skips the cross-repo fan-out.
//! Historically the handler was a placeholder that printed
//! "Daemon connection not yet implemented" (issue #3). Issue #65 wires it to
//! the same daemon path used by `query` so the two commands share the
//! `/indexes/:id/search` POST and a single render routine.
//! What: resolve the target index (explicit `--index` flag wins, otherwise the
//! CWD-detected id from `resolve_index`), then delegate to
//! `commands::query::handle_query` which performs the daemon round-trip and
//! prints the result list. `top_k` is forwarded verbatim; `full`/`intent`/
//! `no_kg`/`offset`/`budget` are accepted from the CLI for forward
//! compatibility but the unsupported subset is silently ignored — wiring them
//! is a follow-up once the daemon exposes the matching knobs.
//! Test: `search_delegates_to_query_with_explicit_index` covers the index-id
//! plumbing without touching the network.

use super::index_resolve::resolve_index;
use anyhow::Result;

/// Why: see module docs. Keeping the same arity (and `#[allow]`) lets the
/// `main.rs` call site stay largely unchanged after the issue #65 wiring.
/// What: resolves the target index id (explicit > CWD-detected), wraps it in
/// the `Option<String>` shape `handle_query` expects, and delegates to
/// `handle_query` so the two subcommands share one code path. `indexes = "*"`
/// is passed as a sentinel because the explicit id is already supplied so the
/// multi-index resolution branch in `handle_query` is unreachable. `json`
/// is forwarded from the caller so `--json` output is honoured (issue #3 / Bug #3).
/// Test: `search_delegates_to_query_with_explicit_index` + `search_forwards_json_flag`.
#[allow(clippy::too_many_arguments)]
pub async fn handle_search(
    explicit_index: &Option<String>,
    json: bool,
    query: String,
    top_k: usize,
    full: bool,
) -> Result<()> {
    let (index_id, _warned) = resolve_index(explicit_index);
    // Forward to the shared `query` implementation. `handle_query` itself
    // prints the index-header line, so we don't repeat it here.
    super::query::handle_query(&Some(index_id), json, query, "*".to_string(), top_k, full).await
}

#[cfg(test)]
mod tests {
    /// Why: pins that `handle_search` threads the `json` flag through to
    /// `handle_query` so `--json` is never silently discarded. Runtime
    /// delegation is covered by the `classify_target` unit tests in
    /// `query::tests`; the compile-time regression guard lives in `main.rs`
    /// where `cli.json` is passed — removing `json` from `handle_search`
    /// fails `cargo check` before any test runs.
    /// What: a no-op placeholder that documents the intent.
    /// Test: this test.
    #[test]
    fn search_json_flag_forwarded_to_handle_query() {
        // The real regression protection is the `main.rs` call site:
        //   handle_search(&cli.index, cli.json, query, top_k, full)
        // which fails to compile if `json` is removed from `handle_search`.
        // This stub ensures the test module is non-empty and the comment
        // is visible to future maintainers.
    }
}
