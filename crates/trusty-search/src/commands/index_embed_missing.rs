//! Handler for `trusty-search index embed-missing` (#10551).
//!
//! Why: an embedder fault in the semantic stage leaves an index `degraded` with
//! chunks that have no vector, and a full reindex was the only recovery. This
//! verb asks the daemon to embed only those chunks.
//! What: resolves the index from a real `-i` flag or CWD detection
//! (`TRUSTY_INDEX` alone refuses before any network call, #8737), calls
//! `search.index.embed_missing` over the daemon socket and prints what the
//! daemon did. `--dry-run` reports the gap and queues nothing. The embed runs
//! in the daemon; `trusty-search status` shows it finish.
//! Test: `render_names_the_action_and_the_counts`,
//! `render_marks_unmeasured_counts`.

use super::explicit_target::{flag_only_index, IndexIdSource};
use super::index_resolve::{print_index_header, resolve_index};
use anyhow::{Context, Result};
use colored::Colorize;
use trusty_search::service::rpc::writes::METHOD_INDEX_EMBED_MISSING;

/// Entry point for `trusty-search index embed-missing [--dry-run]`.
///
/// Why: see module docs.
/// What: one socket call; `--json` prints the daemon body verbatim.
/// Test: the render helper's unit tests below; the daemon side is covered by
/// `service::embed_missing::tests`.
pub async fn handle_embed_missing(
    explicit_index: &Option<String>,
    index_source: Option<IndexIdSource>,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    let explicit = flag_only_index("index embed-missing", explicit_index, index_source)?;
    let (index_id, warned) = resolve_index(&explicit)?;
    print_index_header(&index_id, warned);
    let client = super::daemon_rpc::connect().await?;
    let params = serde_json::json!({ "index_id": index_id, "body": { "dry_run": dry_run } });
    let body = super::daemon_rpc::call(&client, METHOD_INDEX_EMBED_MISSING, params)
        .await
        .with_context(|| format!("could not resume embedding for index '{index_id}'"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        println!("{}", render(&body));
    }
    Ok(())
}

/// Render one daemon answer as the operator-facing line.
///
/// Why: the action decides what the operator does next, so it leads.
/// What: the action label, then `missing of chunks`; a count the daemon could
/// not measure renders as `?` rather than `0`.
/// Test: `render_names_the_action_and_the_counts`,
/// `render_marks_unmeasured_counts`.
fn render(body: &serde_json::Value) -> String {
    let n = |k: &str| {
        body.get(k)
            .and_then(|v| v.as_u64())
            .map_or_else(|| "?".to_string(), |v| v.to_string())
    };
    let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("?");
    let tail = match action {
        "queued" => " — embedding only those; `trusty-search status` shows it finish",
        "would_queue" => " — dry run, nothing queued",
        "already_running" => " — a pass is already running",
        "settled" => " — semantic stage is ready",
        _ => "",
    };
    format!(
        "{} {action}: {} of {} chunks missing a vector{tail}",
        "\u{2713}".green(),
        n("missing"),
        n("chunks"),
    )
}

#[cfg(test)]
mod tests {
    use super::render;

    /// Test: `render_names_the_action_and_the_counts`.
    #[test]
    fn render_names_the_action_and_the_counts() {
        let out = render(&serde_json::json!({
            "action": "queued", "missing": 3052, "chunks": 147465,
        }));
        assert!(out.contains("queued: 3052 of 147465 chunks"), "{out}");
    }

    /// Test: `render_marks_unmeasured_counts`.
    #[test]
    fn render_marks_unmeasured_counts() {
        let out = render(&serde_json::json!({ "action": "already_running" }));
        assert!(out.contains("? of ? chunks"), "{out}");
    }
}
