//! Native CLI verbs for the per-lane MCP search tools (#11625).
//!
//! Why: `search_lexical`, `search_semantic` and `search_kg` were reachable
//! only through MCP. The daemon already selects a lane through
//! `SearchQuery.stage` on `search.query`, so the CLI needs no new method.
//! What: [`LaneVerb`] is flattened into `Commands`. [`run`] probes
//! `search.index.status` when the lane needs a stage, then builds the params
//! with the MCP tools' own `lane_query_params` and sends `search.query`. A
//! lane whose stage is not ready is refused before any query is sent.
//! Test: `search_lane_verbs_tests.rs`.

use anyhow::Result;
use clap::{Subcommand, ValueEnum};
use serde_json::{json, Value};
use trusty_search::mcp::tools::lane::{lane_query_params, SearchLane};
use trusty_search::service::daemon_client::DaemonClient;
use trusty_search::service::rpc::queries::METHOD_QUERY;
use trusty_search::service::rpc::reads::METHOD_INDEX_STATUS;

#[cfg(test)]
#[path = "search_lane_verbs_tests.rs"]
mod tests;

/// The ranking mode, as the MCP `mode` argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LaneMode {
    /// Prefer source code.
    Code,
    /// Prefer prose docs.
    Text,
    /// Prefer structured data.
    Data,
}

/// The arguments every lane verb takes.
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct LaneArgs {
    /// Search query
    pub query: String,
    /// Number of results (daemon default 10)
    #[arg(short = 'k', long)]
    pub top_k: Option<u64>,
    /// Only chunks whose path starts with this prefix
    #[arg(long)]
    pub path_prefix: Option<String>,
    /// Ranking mode
    #[arg(long, value_enum)]
    pub mode: Option<LaneMode>,
    /// Drop archived/deprecated/legacy chunks instead of downranking them
    #[arg(long)]
    pub exclude_archived: bool,
    /// Candidate depth of each lexical lane, 1-10000
    #[arg(long)]
    pub lexical_limit: Option<u64>,
}

/// The three lane verbs, flattened into the top-level `Commands` enum.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum LaneVerb {
    /// BM25 and literal search only (MCP `search_lexical`)
    ///
    /// Examples:
    ///   trusty-search lexical apply_archive_downrank -k 5
    #[command(display_order = 17)]
    Lexical(LaneArgs),

    /// Embedding search; refused until vectors are ready (MCP `search_semantic`)
    ///
    /// Examples:
    ///   trusty-search semantic "code that verifies JWTs" --mode code
    #[command(display_order = 18)]
    Semantic(LaneArgs),

    /// Search with symbol-graph expansion; refused until the KG is ready
    /// (MCP `search_kg`)
    ///
    /// Examples:
    ///   trusty-search kg validate_token --refine-query "token refresh callers"
    #[command(display_order = 19)]
    Kg {
        #[command(flatten)]
        args: LaneArgs,
        /// Rerank expanded neighbours by similarity to this description
        #[arg(long)]
        refine_query: Option<String>,
    },
}

impl LaneVerb {
    /// The lane and the MCP-shaped arguments this verb stands for.
    ///
    /// Test: `lane_flags_reach_the_body`.
    pub fn lane_and_args(&self) -> (SearchLane, Value) {
        let (lane, a, refine) = match self {
            Self::Lexical(a) => (SearchLane::Lexical, a, None),
            Self::Semantic(a) => (SearchLane::Semantic, a, None),
            Self::Kg { args, refine_query } => (SearchLane::Graph, args, refine_query.as_ref()),
        };
        let mut v = json!({ "query": a.query });
        if let Some(k) = a.top_k {
            v["top_k"] = Value::from(k);
        }
        if let Some(p) = &a.path_prefix {
            v["path_prefix"] = Value::from(p.as_str());
        }
        if let Some(m) = a.mode {
            let m = match m {
                LaneMode::Code => "code",
                LaneMode::Text => "text",
                LaneMode::Data => "data",
            };
            v["mode"] = Value::from(m);
        }
        if a.exclude_archived {
            v["exclude_archived"] = Value::Bool(true);
        }
        if let Some(n) = a.lexical_limit {
            v["lexical_limit"] = Value::from(n);
        }
        if let Some(r) = refine {
            v["refine_query"] = Value::from(r.as_str());
        }
        (lane, v)
    }
}

/// Probe readiness, then send the lane's `search.query`.
///
/// Why: the daemon does not refuse `stage=graph` on a KG that is not ready,
/// so the CLI must refuse first, on the same rule MCP uses (#11625).
/// What: reads `search.index.status` when the lane needs a capability, then
/// hands it to `lane_query_params`, which fails closed on a missing
/// capability or a missing `search_capabilities`. Only on success is
/// `search.query` sent.
/// Test: `each_lane_verb_pins_its_stage`, `a_not_ready_lane_sends_no_query`,
/// `a_status_without_capabilities_is_refused`.
///
/// # Errors
///
/// "stage not ready: …" for a refusal; the `daemon_rpc` errors otherwise.
pub(crate) async fn run(client: &DaemonClient, verb: &LaneVerb, index: &str) -> Result<Value> {
    let (lane, args) = verb.lane_and_args();
    let status = match lane.required_capability() {
        Some(_) => Some(
            super::daemon_rpc::call(client, METHOD_INDEX_STATUS, json!({ "index_id": index }))
                .await?,
        ),
        None => None,
    };
    let params = lane_query_params(lane, index, status.as_ref(), &args)?;
    super::daemon_rpc::call(client, METHOD_QUERY, params).await
}

/// One text line per hit: `file:start-end  (score, match_reason)`.
pub(crate) fn lines(body: &Value) -> Option<Vec<String>> {
    let rows = body.get("results")?.as_array()?;
    Some(
        rows.iter()
            .map(|r| {
                format!(
                    "{}:{}-{}  ({:.3}, {})",
                    r.get("file").and_then(Value::as_str).unwrap_or("?"),
                    r.get("start_line").and_then(Value::as_u64).unwrap_or(0),
                    r.get("end_line").and_then(Value::as_u64).unwrap_or(0),
                    r.get("score").and_then(Value::as_f64).unwrap_or(0.0),
                    r.get("match_reason").and_then(Value::as_str).unwrap_or("?"),
                )
            })
            .collect(),
    )
}

/// Execute one of the [`LaneVerb`] subcommands.
///
/// Why: one entry point keeps index resolution and output identical to the
/// other search verbs.
/// What: resolves the index (`-i` wins, else the cwd project), starts the
/// daemon if needed, runs the verb, and prints `--json` as the method result
/// or one line per hit. Any failure is `Err`, which `main()` turns into exit
/// code 1.
/// Test: `a_not_ready_lane_sends_no_query`.
pub async fn handle(verb: LaneVerb, explicit: &Option<String>, json_out: bool) -> Result<()> {
    let (index, warned) = super::index_resolve::resolve_index(explicit)?;
    super::index_resolve::print_index_header(&index, warned);
    let client = super::daemon_rpc::connect().await?;
    let body = run(&client, &verb, &index).await?;
    match (json_out, lines(&body)) {
        (true, _) => println!("{body}"),
        (false, Some(l)) if l.is_empty() => println!("(no matches)"),
        (false, Some(l)) => l.iter().for_each(|l| println!("{l}")),
        (false, None) => println!(
            "{}",
            serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string())
        ),
    }
    Ok(())
}
