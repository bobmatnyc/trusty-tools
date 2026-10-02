//! Handler for `trusty-search quantize` — the scalar-precision backfill
//! (issue #6822).
//!
//! Why: #6822 makes `f16` the default precision for indexes built from now on,
//! and that default applies at index CREATION only. Every index already on disk
//! keeps its `f32` vectors, and `reindex --force` does not change that — the
//! store object is built once at warm-boot and a reindex upserts into it, so a
//! forced reindex re-embeds at the old precision. This command is the explicit
//! conversion an operator runs once per existing index.
//!
//! What: always fetches the report-only dry run FIRST and prints it — index id,
//! root, chunk count, vector count, current and target precision, on-disk
//! bytes — then stops (`--dry-run`), or asks for confirmation (unless `--yes`)
//! before issuing the real `search.index.quantize` call. Both go over the
//! daemon's Unix socket (#6285); a missing socket fails closed, naming its path.
//!
//! Test: `tests::render_report_names_the_index_and_chunk_count`,
//! `tests::render_report_marks_unknown_counts`.

use super::explicit_target::{flag_only_index, IndexIdSource};
use super::format::format_with_commas;
use super::index_resolve::{print_index_header, resolve_index};
use anyhow::Result;
use clap::Args;
use colored::Colorize;
use trusty_search::service::daemon_client::{DaemonCallError, DaemonClient};
use trusty_search::service::rpc::writes::METHOD_INDEX_QUANTIZE;

/// Flags for `trusty-search quantize`.
///
/// Why they live here, not in `main.rs`'s `Commands` enum (#6822): `main.rs`
/// sits on a frozen line-cap budget, and a subcommand's flags belong beside the
/// handler that reads them regardless.
/// What: a `clap::Args` struct flattened into the `Quantize` variant, so the CLI
/// surface is identical to declaring the fields inline.
/// Test: exercised through `handle_quantize`; the renderer's own unit tests are
/// below.
#[derive(Args, Debug, Clone)]
pub struct QuantizeArgs {
    /// Target precision: `f16` (default), `f32` (undo), or `i8`
    #[arg(long = "to", default_value = "f16")]
    pub to: String,

    /// Report what would change and write nothing
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the confirmation prompt (for scripted fleet runs)
    #[arg(short = 'y', long)]
    pub yes: bool,
}

/// Handle `trusty-search quantize [--to f16] [--dry-run] [--yes]`.
///
/// Why: gives the #6822 default flip a path onto indexes that already exist.
/// What: resolves the index from a real `-i` flag or CWD detection —
/// `TRUSTY_INDEX` alone refuses before any network call (#8737) — renders the
/// dry-run report, then applies unless the caller only asked to look. `--yes`
/// skips the prompt for scripted fleet runs.
/// Test: the render helper's unit tests below; the route itself is covered by
/// `service::server::tests_quantize_6822`; the #8737 refusal by
/// `quantize_env_only_refuses_with_no_requests`.
pub async fn handle_quantize(
    explicit_index: &Option<String>,
    index_source: Option<IndexIdSource>,
    args: &QuantizeArgs,
    json: bool,
) -> Result<()> {
    let (to, dry_run, yes) = (args.to.as_str(), args.dry_run, args.yes);
    let explicit = flag_only_index("quantize", explicit_index, index_source)?;
    let (index_id, warned) = resolve_index(&explicit)?;
    print_index_header(&index_id, warned);

    // #6285: the socket, never the retiring HTTP listener.
    let client = DaemonClient::resolve()?;
    crate::commands::daemon_guard::ensure_daemon_up(&client).await?;

    // Always look before touching anything: the dry run is both the operator's
    // confirmation text and the pre-flight check that the index can be
    // converted at all.
    let preview = post_quantize(&client, &index_id, to, true).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&preview)?);
    } else {
        print!("{}", render_report(&preview));
    }

    if dry_run {
        if !json {
            println!("{} dry run — nothing was written", "•".cyan());
        }
        return Ok(());
    }
    if preview
        .pointer("/report/current")
        .and_then(|v| v.as_str())
        .is_some_and(|c| c == to_label(to))
    {
        if !json {
            println!(
                "{} already at {} — nothing to do",
                "✓".green(),
                to_label(to)
            );
        }
        return Ok(());
    }
    if !yes && !confirm(&format!("Re-encode this index's vectors to {to}?"))? {
        println!("{} aborted", "✗".yellow());
        return Ok(());
    }

    let applied = post_quantize(&client, &index_id, to, false).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&applied)?);
    } else {
        print!("{}", render_report(&applied));
        println!("{} converted", "✓".green());
    }
    Ok(())
}

/// Issue one `search.index.quantize` call and return its JSON result.
///
/// Why: the dry run and the applied run differ only by one field, so they share
/// one request builder — a second copy is how the two drift apart.
/// What: sends `{index_id, body: {quant, dry_run}}`. A refusal is surfaced with
/// the daemon's own class and message rather than a bare code, because every
/// refusal this route emits (unknown index, reindex in flight, no vector store)
/// is actionable only if the operator can read which one fired.
/// Test: `post_quantize_sends_the_socket_params_and_returns_the_result`,
/// `post_quantize_reports_the_daemons_refusal`.
async fn post_quantize(
    client: &DaemonClient,
    index_id: &str,
    to: &str,
    dry_run: bool,
) -> Result<serde_json::Value> {
    let params = serde_json::json!({
        "index_id": index_id,
        "body": { "quant": to, "dry_run": dry_run },
    });
    // A whole-arena re-encode can outrun the default call budget.
    let budget = std::time::Duration::from_secs(30 * 60);
    client
        .call_with_timeout(METHOD_INDEX_QUANTIZE, params, budget)
        .await
        .map_err(|e: DaemonCallError| match e.message() {
            Some(_) => anyhow::anyhow!("quantize {e}"),
            None => anyhow::Error::new(e),
        })
}

/// The label `RequantizeReport::current` carries for an operator-supplied value.
///
/// Why: `--to f32` and the report's `"f32 (none)"` are the same precision spelt
/// two ways, and comparing them directly would report "not converted" forever.
fn to_label(to: &str) -> &'static str {
    match crate::core::store_config::VectorQuant::parse_operator_value(to) {
        Some(q) => q.label(),
        None => "unknown",
    }
}

/// Render one quantize report as the operator-facing block.
///
/// Why: the acceptance criterion for #6822's backfill is a dry run that NAMES
/// the index and its chunk count, so the rendering is part of the contract, not
/// decoration — hence its own unit tests.
/// What: a fixed six-line block. Missing numbers render as `?` rather than `0`,
/// so an unreadable corpus count never reads as an empty index.
/// Test: `tests::render_report_names_the_index_and_chunk_count`.
fn render_report(body: &serde_json::Value) -> String {
    let s = |p: &str| {
        body.pointer(p)
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string()
    };
    let n = |p: &str| match body.pointer(p).and_then(|v| v.as_u64()) {
        Some(v) => format_with_commas(v),
        None => "?".to_string(),
    };
    format!(
        "index:    {}\nroot:     {}\nchunks:   {}\nvectors:  {}{}\nprecision: {} → {}\nsnapshot: {} → {} bytes\n",
        s("/index_id"),
        s("/root_path"),
        n("/chunk_count"),
        n("/report/vectors"),
        match body.pointer("/report/missing").and_then(|v| v.as_u64()) {
            Some(m) if m > 0 => format!(" ({m} unmapped, skipped)"),
            _ => String::new(),
        },
        s("/report/current"),
        s("/report/target"),
        n("/report/bytes_before"),
        n("/report/bytes_after"),
    )
}

/// Why: keep the y/N prompt isolated, mirroring `cleanup::confirm`.
/// What: prints `<prompt> [y/N] `, reads one line, returns `true` only for
/// `y`/`yes` (case-insensitive).
fn confirm(prompt: &str) -> Result<bool> {
    use std::io::{BufRead, Write};
    print!("{} [y/N] ", prompt);
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let answer = line.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #6822: the dry-run block must name the index and its chunk count — that
    /// is the confirmation an operator acts on.
    #[test]
    fn render_report_names_the_index_and_chunk_count() {
        let body = serde_json::json!({
            "index_id": "trusty-tools",
            "root_path": "/Users/x/Projects/trusty-tools",
            "chunk_count": 201_206,
            "report": {
                "current": "f32 (none)",
                "target": "f16",
                "vectors": 200_000,
                "missing": 0,
                "bytes_before": 586_000_000u64,
                "bytes_after": 586_000_000u64,
                "applied": false,
                "dry_run": true,
            }
        });
        let out = render_report(&body);
        assert!(out.contains("trusty-tools"), "{out}");
        assert!(out.contains("201,206"), "{out}");
        assert!(out.contains("f32 (none) → f16"), "{out}");
        assert!(!out.contains("unmapped"), "{out}");
    }

    /// An unreadable count must render as `?`, never as `0` — a corpus that
    /// failed to open is not an empty index (#4333's reasoning, one level out).
    #[test]
    fn render_report_marks_unknown_counts() {
        let body = serde_json::json!({
            "index_id": "x",
            "root_path": "/x",
            "chunk_count": serde_json::Value::Null,
            "report": { "current": "f16", "target": "f16", "vectors": 3, "missing": 2 }
        });
        let out = render_report(&body);
        assert!(out.contains("chunks:   ?"), "{out}");
        assert!(out.contains("(2 unmapped, skipped)"), "{out}");
    }

    /// #6285: the CLI sends `search.index.quantize` over the socket with the
    /// params the daemon decodes, and returns the daemon's result unchanged.
    #[tokio::test]
    async fn post_quantize_sends_the_socket_params_and_returns_the_result() {
        let daemon = crate::commands::mock_socket::mock_daemon(|method, params| {
            assert_eq!(method, METHOD_INDEX_QUANTIZE);
            Ok(serde_json::json!({ "echo": params }))
        })
        .await;
        let got = post_quantize(&daemon.client, "idx", "f16", true)
            .await
            .expect("the mock answers");
        assert_eq!(got["echo"]["index_id"], "idx");
        assert_eq!(got["echo"]["body"]["quant"], "f16");
        assert_eq!(got["echo"]["body"]["dry_run"], true);
    }

    /// #6285: a refusal reaches the operator with the daemon's class and its
    /// own message, the same information the `HTTP 409: <error>` line carried.
    #[tokio::test]
    async fn post_quantize_reports_the_daemons_refusal() {
        let daemon = crate::commands::mock_socket::mock_daemon(|_, _| {
            Err(trusty_common::uds::server::RpcError::new(
                trusty_search::service::rpc::error::CODE_CONFLICT,
                "a reindex is in progress for this index",
            ))
        })
        .await;
        let err = post_quantize(&daemon.client, "idx", "f16", false)
            .await
            .expect_err("the mock refuses")
            .to_string();
        assert!(
            err.contains("conflict") && err.contains("a reindex is in progress"),
            "{err}"
        );
    }

    /// #6285: with no daemon on the socket the call fails closed and names the
    /// socket; it never retries over TCP.
    #[tokio::test]
    async fn post_quantize_fails_closed_on_a_missing_socket() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let socket = dir.path().join("none.sock");
        let err = post_quantize(&DaemonClient::at(&socket), "idx", "f16", true)
            .await
            .expect_err("nothing is listening")
            .to_string();
        assert!(err.contains(&socket.display().to_string()), "{err}");
    }

    /// `--to f32` and the report's `"f32 (none)"` name one precision; the
    /// already-at-target short circuit compares through this mapping.
    #[test]
    fn to_label_maps_operator_spellings_onto_report_labels() {
        assert_eq!(to_label("f32"), "f32 (none)");
        assert_eq!(to_label("f16"), "f16");
        assert_eq!(to_label("i8"), "i8");
        assert_eq!(to_label("fp8"), "unknown");
    }
}
