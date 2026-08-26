//! Handlers for `trusty-memory monitor status` and `monitor palaces`.
//!
//! Why: the `monitor web` / `monitor tui` subcommands surface the daemon
//! dashboard interactively, but scripts and CI need the same numbers as plain
//! text or JSON without launching a TUI. These handlers expose the daemon's
//! aggregate health and per-palace stats as scriptable output (issue #33).
//! What: `handle_status` prints daemon version and aggregate counts;
//! `handle_palaces` prints either a table of every palace or a single palace's
//! detail. Both accept a `--json` flag and exit 1 (via `Err`) when the daemon
//! is unreachable.
//! Test: unit tests cover `fmt_count`; live behaviour is exercised by
//! `cargo run -p trusty-memory -- monitor status` against a running daemon.

use anyhow::{Context, Result};
use trusty_common::monitor::dashboard::{MemoryData, PalaceRow, UNKNOWN_COUNT};
use trusty_common::monitor::memory_client::{resolve_memory_socket, MemoryClient};

/// Format a count with comma thousands separators (`8400` → `"8,400"`).
///
/// Why: vector and drawer counts in the plain-text table read far easier
/// grouped; an exact comma-grouped form keeps precision a script may want.
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

/// Format an optional count, printing `—` when the value is not known.
///
/// Why (issue #4682): `GET /api/v1/palaces` reports `0` for every count of a
/// palace whose handle is not resident (`cached: false`). Those zeros mean
/// *unknown*, not *empty*, and printing them as numbers made
/// `monitor palaces <id>` claim `vectors: 0` for a palace the API said had 912.
/// What: `Some(n)` formats as [`fmt_count`]; `None` yields the shared
/// `UNKNOWN_COUNT` placeholder.
/// Test: `fmt_opt_count_marks_unknown`.
fn fmt_opt_count(n: Option<u64>) -> String {
    match n {
        Some(n) => fmt_count(n),
        None => UNKNOWN_COUNT.to_string(),
    }
}

/// Fetch the full trusty-memory dashboard payload or fail with a clear error.
///
/// Why: every monitor subcommand needs the same status + palace snapshot; this
/// centralises the daemon-URL resolution and the unreachable-daemon error so
/// each handler stays terse.
/// What: derives the daemon's socket path, then calls `MemoryClient::fetch_all`.
/// A transport error becomes an `Err` so `main()` exits 1.
/// Test: covered indirectly by the handler tests; the live path needs a daemon.
async fn fetch_memory_data() -> Result<MemoryData> {
    let socket = resolve_memory_socket()?;
    let client = MemoryClient::new(socket.clone());
    client.fetch_all().await.map_err(|e| {
        anyhow::anyhow!(
            "could not reach trusty-memory daemon at {}: {e}",
            socket.display()
        )
    })
}

/// Print daemon status: version and aggregate palace/drawer/vector counts.
///
/// Why: the quickest scriptable health check — "is the daemon up and how big
/// are its palaces" — without parsing a table or launching the TUI.
/// What: fetches the dashboard payload and prints either a JSON object or a
/// plain-text summary. Returns `Err` when the daemon is unreachable.
/// Test: `cargo run -p trusty-memory -- monitor status` against a live daemon.
pub async fn handle_status(json: bool) -> Result<()> {
    let data = fetch_memory_data().await?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "status": "online",
                "version": data.version,
                "palace_count": data.palace_count,
                "total_drawers": data.total_drawers,
                "total_vectors": data.total_vectors,
                "total_kg_triples": data.total_kg_triples,
            })
        );
    } else {
        println!("trusty-memory  v{}", data.version);
        println!("status:        online");
        println!("palaces:       {}", data.palace_count);
        println!("drawers:       {}", fmt_count(data.total_drawers));
        println!("vectors:       {}", fmt_count(data.total_vectors));
        println!("kg triples:    {}", fmt_count(data.total_kg_triples));
    }
    Ok(())
}

/// Print the palace table, or a single palace's detail when `id` is given.
///
/// Why: operators want the same per-palace vector-count view the TUI shows,
/// but reachable from a shell pipeline.
/// What: with no `id`, prints an `ID / NAME / VECTORS` table (or a JSON array)
/// from the bulk list; with an `id`, reads `GET /api/v1/palaces/{id}` — the
/// route that actually opens the palace — and prints that one palace's detail
/// (or a JSON object), failing with a clear error when the id is unknown.
/// Test: `cargo run -p trusty-memory -- monitor palaces` against a live daemon.
///
/// # Spec References
/// - issue #4682 — the single-id path used to filter the peek-based bulk list,
///   so its counts depended on whether the palace happened to be LRU-resident.
pub async fn handle_palaces(id: Option<String>, json: bool) -> Result<()> {
    match id {
        // #4682: one palace => ask the route that opens it, never the bulk list.
        Some(id) => {
            let row = fetch_palace(&id).await?;
            print_palace_detail(&row, json);
            Ok(())
        }
        None => {
            let data = fetch_memory_data().await?;
            print_palace_table(&data.palaces, json);
            Ok(())
        }
    }
}

/// Fetch one palace's live counts, or fail with a clear error.
///
/// Why (issue #4682): the bulk listing's counts are placeholder zeros for any
/// palace the daemon has not loaded. `memory.palace_get` opens the palace, so it
/// is the only source that answers "how big is this palace" deterministically.
/// What: derives the socket, calls [`MemoryClient::fetch_palace`], and wraps
/// transport / not-found failures with the socket path so `main()` exits 1 with
/// an actionable message.
/// Test: covered live by `trusty-memory monitor palaces <id>`; the projection is
/// unit-tested in `trusty-common`.
async fn fetch_palace(id: &str) -> Result<PalaceRow> {
    let socket = resolve_memory_socket()?;
    let client = MemoryClient::new(socket.clone());
    client.fetch_palace(id).await.with_context(|| {
        format!(
            "could not read palace '{id}' from trusty-memory at {}",
            socket.display()
        )
    })
}

/// Render every palace as a JSON array or an aligned plain-text table.
///
/// Why: shared by `handle_palaces` for the list case; isolating it keeps the
/// handler's branching readable.
/// What: emits a JSON array of `{id, name, vectors}` objects when `json`,
/// otherwise a header row plus one aligned row per palace. A palace the daemon
/// has not loaded reports `null` / `—` rather than `0` (issue #4682) — the bulk
/// list cannot know its counts.
/// Test: side-effect-only (prints); the alignment is verified via the live
/// command.
fn print_palace_table(palaces: &[PalaceRow], json: bool) {
    if json {
        let arr: Vec<serde_json::Value> = palaces
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id,
                    "name": p.name,
                    // #4682: `null`, not `0`, when the count is unknown.
                    "vectors": p.vectors(),
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(arr));
        return;
    }

    if palaces.is_empty() {
        println!("(no palaces)");
        return;
    }

    let id_w = palaces
        .iter()
        .map(|p| p.id.len())
        .max()
        .unwrap_or(0)
        .max(12);
    let name_w = palaces
        .iter()
        .map(|p| p.name.len())
        .max()
        .unwrap_or(0)
        .max(12);
    println!("{:<id_w$}  {:<name_w$}  VECTORS", "ID", "NAME");
    for p in palaces {
        println!(
            "{:<id_w$}  {:<name_w$}  {}",
            p.id,
            p.name,
            // #4682: `—` for a palace the daemon has not loaded.
            fmt_opt_count(p.vectors()),
        );
    }
}

/// Render one palace's detail as a JSON object or plain-text lines.
///
/// Why: shared by `handle_palaces` for the single-id case. Takes the row the
/// single-palace route returned rather than searching a list — resolving the
/// id is [`fetch_palace`]'s job now (issue #4682), so a miss is a transport
/// error, not a filter that silently found nothing.
/// What: prints `id` / `name` / `vectors`, rendering an unknown count as
/// `—` (text) or `null` (JSON) instead of `0`.
/// Test: `print_palace_detail_renders_unknown_vectors`.
fn print_palace_detail(row: &PalaceRow, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "id": row.id,
                "name": row.name,
                // #4682: `null`, not `0`, when the daemon could not load it.
                "vectors": row.vectors(),
            })
        );
    } else {
        println!("id:       {}", row.id);
        println!("name:     {}", row.name);
        println!("vectors:  {}", fmt_opt_count(row.vectors()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_count_groups_thousands() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(42), "42");
        assert_eq!(fmt_count(8_400), "8,400");
        assert_eq!(fmt_count(1_234_567), "1,234,567");
    }

    /// Why (issue #4682): a count the daemon never measured must not print as
    /// a number. `fmt_count` cannot express "unknown", which is exactly how a
    /// cold palace came to report `vectors: 0` while the API said `912`.
    /// What: asserts `None` renders as the placeholder and `Some(0)` — a
    /// genuinely empty palace — still renders as `0`, so the two stay
    /// distinguishable.
    /// Test: this test.
    #[test]
    fn fmt_opt_count_marks_unknown() {
        assert_eq!(fmt_opt_count(None), UNKNOWN_COUNT);
        assert_ne!(fmt_opt_count(None), "0");
        assert_eq!(fmt_opt_count(Some(0)), "0");
        assert_eq!(fmt_opt_count(Some(8_400)), "8,400");
    }

    /// Why (issue #4682): the owner's report — `monitor palaces <id>` printing
    /// `vectors: 0` for a palace that has 912. This pins the row-level rule the
    /// printer depends on: a row flagged `counts_unknown` yields `None`, never
    /// a number, no matter what the placeholder field holds.
    /// What: builds a cold row carrying a placeholder zero and a warm row with
    /// a real count, and asserts the accessor separates them.
    /// Test: this test.
    #[test]
    fn print_palace_detail_renders_unknown_vectors() {
        // A cold row as the daemon reports it: `cached: false`, counts zeroed.
        let cold = PalaceRow {
            id: "t-tmpugxp9v".into(),
            name: "t-tmpugxp9v".into(),
            vector_count: 0,
            counts_unknown: true,
            ..Default::default()
        };
        assert_eq!(cold.vectors(), None);
        assert_eq!(fmt_opt_count(cold.vectors()), UNKNOWN_COUNT);

        // The same palace after the single-palace route opened it.
        let warm = PalaceRow {
            id: "t-tmpugxp9v".into(),
            name: "t-tmpugxp9v".into(),
            vector_count: 912,
            ..Default::default()
        };
        assert_eq!(warm.vectors(), Some(912));
        assert_eq!(fmt_opt_count(warm.vectors()), "912");

        // Side-effect-only; exercised for panic-freedom on both shapes.
        print_palace_detail(&cold, false);
        print_palace_detail(&cold, true);
        print_palace_detail(&warm, true);
    }
}
