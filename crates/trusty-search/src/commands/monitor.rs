//! Handler for `trusty-search monitor status` and `monitor indexes`.
//!
//! Why: the `monitor web` / `monitor tui` subcommands surface the daemon
//! dashboard interactively, but scripts and CI need the same numbers as plain
//! text or JSON without launching a TUI. These handlers expose the daemon's
//! health and per-index stats as scriptable output (issue #33).
//! What: `handle_status` prints daemon health, version, uptime, and corpus
//! totals; `handle_indexes` prints either a table of every index or a single
//! index's detail. Both accept a `--json` flag and exit 1 (via `Err`) when the
//! daemon is unreachable.
//! Test: unit tests cover `fmt_count`; live behaviour is exercised by
//! `cargo run -p trusty-search -- monitor status` against a running daemon.

use anyhow::{bail, Result};
use clap::Subcommand;
use trusty_common::monitor::dashboard::{IndexRow, SearchData};
use trusty_search::service::daemon_client::DaemonClient;

/// Target surface for the `monitor` subcommand.
///
/// Why: operators want a quick browser link to the daemon's admin panel, the
/// trusty-search-specific terminal dashboard, OR the same dashboard data as
/// plain text / JSON so scripts and CI can read it without a TUI (issues #33,
/// #34).
/// What: `Web` prints (and opens) the daemon's `/ui` URL; `Tui` launches the
/// trusty-search-specific `trusty_common::monitor::search_tui` ratatui
/// dashboard; `Status` and `Indexes` print scriptable health and per-index
/// stats.
/// Test: `cargo run -p trusty-search -- monitor --help` lists every variant.
#[derive(Subcommand)]
pub enum MonitorTarget {
    /// Open the web dashboard URL in the terminal (or browser)
    Web,
    /// Launch the trusty-search terminal UI: indexes, reindex, and search monitor
    Tui,
    /// Print daemon status: health, version, uptime, and corpus totals
    ///
    /// Examples:
    ///   trusty-search monitor status
    ///   trusty-search monitor status --json
    Status {
        /// Emit the status as a JSON object instead of plain text
        #[arg(long)]
        json: bool,
    },
    /// List every index, or show one index's detail when an ID is given
    ///
    /// Examples:
    ///   trusty-search monitor indexes
    ///   trusty-search monitor indexes my-project
    ///   trusty-search monitor indexes --json
    Indexes {
        /// Optional index ID to show detail for (omit to list all)
        id: Option<String>,
        /// Emit the result as JSON instead of a plain-text table
        #[arg(long)]
        json: bool,
    },
}

/// Format a count with comma thousands separators (`18994` → `"18,994"`).
///
/// Why: chunk counts in the plain-text table read far easier grouped; the
/// dashboard's `format_count` abbreviates large numbers (`19.0k`), which loses
/// precision a script may want, so the CLI uses an exact comma-grouped form.
/// What: returns the decimal string of `n` with a `,` inserted every three
/// digits from the right.
/// Test: `fmt_count_groups_thousands`.
fn fmt_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

/// Fetch the trusty-search dashboard payload or fail with a clear error.
///
/// Why: every monitor subcommand needs the same health + index snapshot; this
/// centralises the daemon lookup and the unreachable-daemon error so each
/// handler stays terse.
/// What: resolves the daemon socket and reads it there (#9214: never TCP, no
/// default address). It does not start a daemon.
/// Test: `fetch_search_data_names_the_socket_when_no_daemon_answers`.
async fn fetch_search_data() -> Result<SearchData> {
    fetch_search_data_from(&DaemonClient::resolve()?).await
}

/// [`fetch_search_data`] against `client`.
///
/// Why: lets a test point the read at a socket of its choosing.
/// What: `search.health` gives the version and uptime and must answer, and so
/// must `search.indexes.list`; either failure is an error naming its cause. A
/// failed `search.index.status` degrades to an all-zero row, as the HTTP read
/// did. Rows are sorted by id.
/// Test: `fetch_search_data_names_the_socket_when_no_daemon_answers`,
/// `fetch_search_data_fails_when_the_index_list_fails`.
async fn fetch_search_data_from(client: &DaemonClient) -> Result<SearchData> {
    let health = client
        .health()
        .await
        .map_err(|e| anyhow::anyhow!("could not reach trusty-search daemon: {e}"))?;
    // #9214: a failed index list is an error, never "online, 0 indexes".
    let names = super::doctor_checks::fetch_index_names(client)
        .await
        .map_err(|e| anyhow::anyhow!("could not list trusty-search indexes: {e}"))?;
    let indexes = super::doctor_checks::fetch_index_statuses(client, &names)
        .await
        .into_iter()
        .map(|(id, body)| index_row(id, &body.unwrap_or_default()))
        .collect();
    Ok(SearchData {
        version: health
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        uptime_secs: health
            .get("uptime_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        indexes,
    })
}

/// One dashboard row from a `search.index.status` body; absent fields default.
fn index_row(id: String, body: &serde_json::Value) -> IndexRow {
    IndexRow {
        id,
        chunk_count: body
            .get("chunk_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        root_path: body
            .get("root_path")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        disk_bytes: body.get("disk_bytes").and_then(|v| v.as_u64()),
        last_indexed: body
            .get("last_indexed")
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
        ..Default::default()
    }
}

/// Print daemon status: health, version, index count, and total chunks.
///
/// Why: the quickest scriptable health check — "is the daemon up and how big
/// is its corpus" — without parsing a table or launching the TUI.
/// What: fetches the dashboard payload and prints either a JSON object or a
/// four-line plain-text summary. Returns `Err` when the daemon is unreachable.
/// Test: `cargo run -p trusty-search -- monitor status` against a live daemon.
pub async fn handle_status(json: bool) -> Result<()> {
    let data = fetch_search_data().await?;
    let total_chunks: u64 = data.indexes.iter().map(|i| i.chunk_count).sum();

    if json {
        println!(
            "{}",
            serde_json::json!({
                "status": "online",
                "version": data.version,
                "uptime_secs": data.uptime_secs,
                "index_count": data.indexes.len(),
                "total_chunks": total_chunks,
            })
        );
    } else {
        println!("trusty-search  v{}", data.version);
        println!("status:        online");
        println!(
            "uptime:        {}",
            trusty_common::monitor::dashboard::format_uptime(data.uptime_secs)
        );
        println!("indexes:       {}", data.indexes.len());
        println!("total chunks:  {}", fmt_count(total_chunks));
    }
    Ok(())
}

/// Print the index table, or a single index's detail when `id` is given.
///
/// Why: operators want the same per-index chunk-count view the TUI shows, but
/// reachable from a shell pipeline.
/// What: with no `id`, prints a `NAME / CHUNKS / PATH` table (or a JSON array);
/// with an `id`, prints that one index's detail (or a JSON object), failing
/// with a clear error when the id is not registered.
/// Test: `cargo run -p trusty-search -- monitor indexes` against a live daemon.
pub async fn handle_indexes(id: Option<String>, json: bool) -> Result<()> {
    let data = fetch_search_data().await?;
    match id {
        Some(id) => print_index_detail(&data.indexes, &id, json),
        None => {
            print_index_table(&data.indexes, json);
            Ok(())
        }
    }
}

/// Render every index as a JSON array or an aligned plain-text table.
///
/// Why: shared by `handle_indexes` for the list case; isolating it keeps the
/// handler's branching readable.
/// What: emits a JSON array of `{name, chunks, path}` objects when `json`,
/// otherwise a header row plus one aligned row per index.
/// Test: side-effect-only (prints); the alignment is verified by eye via the
/// live command.
fn print_index_table(indexes: &[IndexRow], json: bool) {
    if json {
        let arr: Vec<serde_json::Value> = indexes
            .iter()
            .map(|i| {
                serde_json::json!({
                    "name": i.id,
                    "chunks": i.chunk_count,
                    "path": i.root_path,
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(arr));
        return;
    }

    if indexes.is_empty() {
        println!("(no indexes registered)");
        return;
    }

    // Width the NAME column to the longest id (min 12) so the CHUNKS and PATH
    // columns line up regardless of index-name length.
    let name_w = indexes
        .iter()
        .map(|i| i.id.len())
        .max()
        .unwrap_or(0)
        .max(12);
    let chunk_w = indexes
        .iter()
        .map(|i| fmt_count(i.chunk_count).len())
        .max()
        .unwrap_or(0)
        .max(6);
    println!("{:<name_w$}  {:>chunk_w$}  PATH", "NAME", "CHUNKS");
    for i in indexes {
        println!(
            "{:<name_w$}  {:>chunk_w$}  {}",
            i.id,
            fmt_count(i.chunk_count),
            i.root_path,
        );
    }
}

/// Render one index's detail as a JSON object or plain-text lines.
///
/// Why: shared by `handle_indexes` for the single-id case.
/// What: looks up `id` in `indexes`; on a hit prints the detail, on a miss
/// returns an `Err` listing the unknown id so `main()` exits 1.
/// Test: side-effect-only (prints); the not-found path is covered by the live
/// command behaviour.
fn print_index_detail(indexes: &[IndexRow], id: &str, json: bool) -> Result<()> {
    let Some(row) = indexes.iter().find(|i| i.id == id) else {
        bail!("no index named '{id}' is registered");
    };
    if json {
        println!(
            "{}",
            serde_json::json!({
                "name": row.id,
                "chunks": row.chunk_count,
                "path": row.root_path,
            })
        );
    } else {
        println!("name:    {}", row.id);
        println!("chunks:  {}", fmt_count(row.chunk_count));
        println!("path:    {}", row.root_path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_count_groups_thousands() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(7), "7");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1_200), "1,200");
        assert_eq!(fmt_count(18_994), "18,994");
        assert_eq!(fmt_count(1_000_000), "1,000,000");
    }

    #[test]
    fn print_index_detail_errors_on_unknown_id() {
        let rows = vec![IndexRow {
            id: "known".into(),
            chunk_count: 10,
            root_path: "/tmp/known".into(),
            ..Default::default()
        }];
        assert!(print_index_detail(&rows, "missing", false).is_err());
        assert!(print_index_detail(&rows, "known", true).is_ok());
    }

    /// #9214: no daemon on the socket is an error naming the socket, never a
    /// dial of the default TCP address.
    #[tokio::test]
    async fn fetch_search_data_names_the_socket_when_no_daemon_answers() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let socket = dir.path().join("absent.sock");
        let err = fetch_search_data_from(&DaemonClient::at(&socket))
            .await
            .expect_err("no daemon answers")
            .to_string();
        assert!(err.contains(&socket.display().to_string()), "{err}");
    }

    /// #9214: a failed `search.indexes.list` is an error naming the cause,
    /// never an "online, 0 indexes" snapshot.
    #[tokio::test]
    async fn fetch_search_data_fails_when_the_index_list_fails() {
        use trusty_search::service::rpc::reads::METHOD_INDEXES_LIST;
        let daemon = crate::commands::mock_socket::mock_daemon(|method, _| {
            if method == METHOD_INDEXES_LIST {
                Err(trusty_common::uds::server::RpcError::new(
                    trusty_common::uds::server::CODE_INTERNAL_ERROR,
                    "registry unreadable",
                ))
            } else {
                Ok(serde_json::json!({"version": "9.9.9", "uptime_secs": 1}))
            }
        })
        .await;
        let err = fetch_search_data_from(&daemon.client)
            .await
            .expect_err("a failed index list is an error")
            .to_string();
        assert!(err.contains("registry unreadable"), "{err}");
    }

    /// `index_row` reads the status fields and defaults the absent ones.
    #[test]
    fn index_row_reads_the_status_body() {
        let body = serde_json::json!({"chunk_count": 7, "root_path": "/r", "disk_bytes": 9});
        let row = index_row("a".into(), &body);
        assert_eq!((row.chunk_count, row.root_path.as_str()), (7, "/r"));
        assert_eq!(row.disk_bytes, Some(9));
        assert_eq!(index_row("b".into(), &serde_json::json!({})).chunk_count, 0);
    }
}
