//! `open-mpm memories <export|import|list>` — cross-machine memory sharing.
//!
//! Why: Teams want to share project knowledge captured by one engineer's
//! agent runs with the rest of the team via git. The export format is plain
//! JSONL committed to `.open-mpm/shared-memories.jsonl`; teammates pull,
//! `import` runs (manually or auto on startup), and the foreign sessions
//! become recallable via `memory_recall` with `scope: "imported"`.
//! What: Three subcommands — `export` writes every memory in the current
//! session (or `--session <id>`) as one JSONL record per line including the
//! embedding vector + machine_id; `import` reads such a file and inserts
//! every record into the local store with `imported: true` stamped on the
//! payload; `list` enumerates the sessions reachable under a chosen `--scope`.
//! Test: See `tests` module — round-trip export → import preserves payloads,
//! import flag-stamps imported=true, scope routing parses correctly.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::memory::store::{MemoryStore, Segment};
use crate::memory::{FastEmbedder, RedbUsearchStore, embed::ALL_MINI_LM_L6_V2_DIM};

/// File name for the committed cross-machine JSONL.
///
/// Why: Living at the repo root inside `.open-mpm/` means `git pull` brings
/// teammates' memories with the same ergonomics as code; auto-import keys
/// off this fixed path so the workflow is "drop the file in, it shows up".
pub const SHARED_MEMORIES_FILENAME: &str = "shared-memories.jsonl";

/// Tracker file recording which shared-memories files have already been
/// imported (by sha256 of contents). Lives under `.open-mpm/state/` so it's
/// gitignored alongside other runtime state.
const IMPORT_TRACKER_FILENAME: &str = "memories_imported.json";

/// Parsed CLI command for `memories ...`.
///
/// Why: Exposed as a structured enum (not just a clap parser) so tests can
/// assert parse outcomes via equality without re-running clap.
/// What: Each variant maps 1:1 to a clap subcommand below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Export {
        session: Option<String>,
        output: Option<PathBuf>,
        segment: Option<String>,
    },
    Import {
        input: Option<PathBuf>,
        from_committed: bool,
    },
    List {
        scope: String,
    },
}

/// Clap front-end for `open-mpm memories ...`.
///
/// Why: Replaces the hand-rolled `parse_args` flag scanner with derive-based
/// clap parsing so help text, error messages, and value validation come for
/// free. The downstream dispatcher continues to consume the structured
/// `Command` enum.
/// What: A single `Subcommand` enum mirroring `Command`; converted in
/// `parse_args` via a small `From` step.
/// Test: All existing `parse_args` unit tests still pass through this path.
#[derive(Debug, Parser)]
#[command(no_binary_name = true)]
struct MemoriesCli {
    #[command(subcommand)]
    cmd: MemoriesSubcommand,
}

#[derive(Debug, Subcommand)]
enum MemoriesSubcommand {
    /// Export this session's memories as JSONL.
    Export {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        output: Option<PathBuf>,
        /// Optional segment filter — only export records from this segment.
        /// Valid values: context, brief, history, agent-memory, code-index.
        #[arg(long)]
        segment: Option<String>,
    },
    /// Import memories from a JSONL file (defaults to the committed file).
    Import {
        #[arg(long)]
        input: Option<PathBuf>,
        #[arg(long = "from-committed")]
        from_committed: bool,
    },
    /// List sessions (scope: session|all|imported).
    List {
        #[arg(long, default_value = "session")]
        scope: String,
    },
}

/// One JSONL record. `embedding` is included so the receiving machine can
/// insert without recomputing — saves a model load and keeps results
/// deterministic across machines that may use different embedders later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRecord {
    pub id: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub machine_id: String,
    pub exported_at: String,
    pub embedding: Vec<f32>,
    /// Echoes the entire original payload so importers can preserve any
    /// extra fields (tags, paths, created_at, etc.) the producer stamped.
    #[serde(default)]
    pub payload: serde_json::Value,
    /// Optional graph-native segment tier this record belongs to.
    ///
    /// Why: With Context/Brief/History segments now first-class, the export
    /// format needs to carry the tier so an importer can re-route to the
    /// matching segment on the receiving machine. Absent on records from
    /// older exports — those default to `Segment::AgentMemory` on import.
    /// What: Snake-case name (e.g., `"context"`, `"brief"`, `"history"`,
    /// `"agent_memory"`).
    /// Test: `memory_export_includes_segment_field` and
    /// `memory_import_routes_by_segment` exercise the round-trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment: Option<String>,
}

/// Tracker JSON shape.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ImportTracker {
    /// Map of file path → {hash, imported_at, count}.
    #[serde(default)]
    files: std::collections::BTreeMap<String, ImportEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ImportEntry {
    hash: String,
    imported_at: String,
    count: usize,
}

/// Parse argv tail into a `Command` via clap.
///
/// Why: Keep this function's signature stable so existing tests and callers
/// (`run_memories_command`, `main.rs` dispatch) continue to work.
/// What: Delegates to clap's derive parser, converts errors to `anyhow`,
/// and post-validates the `--scope` allowlist (clap doesn't enforce it
/// without a `value_parser`, and we want the original error wording).
/// Test: See `tests` module — every previous `parse_args` case still passes.
pub fn parse_args(args: &[&str]) -> Result<Command> {
    if args.is_empty() {
        bail!("usage: memories <export|import|list> [args...]");
    }
    let parsed = MemoriesCli::try_parse_from(args).map_err(|e| anyhow::anyhow!("{e}"))?;
    let cmd = match parsed.cmd {
        MemoriesSubcommand::Export {
            session,
            output,
            segment,
        } => {
            // Validate segment name early so the user gets a clean error
            // before we open any stores.
            if let Some(s) = &segment
                && Segment::from_name(s).is_none()
            {
                bail!(
                    "invalid --segment: {s} (expected context|brief|history|agent-memory|code-index)"
                );
            }
            Command::Export {
                session,
                output,
                segment,
            }
        }
        MemoriesSubcommand::Import {
            input,
            from_committed,
        } => Command::Import {
            input,
            from_committed,
        },
        MemoriesSubcommand::List { scope } => {
            if !matches!(scope.as_str(), "session" | "all" | "imported") {
                bail!("invalid --scope: {scope} (expected session|all|imported)");
            }
            Command::List { scope }
        }
    };
    Ok(cmd)
}

/// Entry point for `open-mpm memories ...`.
pub async fn run_memories_command(args: &[String]) -> Result<()> {
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let cmd = parse_args(&arg_refs)?;
    let cwd = std::env::current_dir().context("failed to get cwd")?;

    match cmd {
        Command::Export {
            session,
            output,
            segment,
        } => {
            let session_id = session.unwrap_or_else(current_session_id);
            let out_path =
                output.unwrap_or_else(|| cwd.join(".open-mpm").join(SHARED_MEMORIES_FILENAME));
            let segment_filter = segment.as_deref().and_then(Segment::from_name);
            let n = export_session(&cwd, &session_id, &out_path, segment_filter).await?;
            let seg_label = segment
                .as_deref()
                .map(|s| format!(" (segment={s})"))
                .unwrap_or_default();
            println!(
                "[open-mpm] Exported {n} memories from session '{session_id}'{seg_label} → {}",
                out_path.display()
            );
            Ok(())
        }
        Command::Import {
            input,
            from_committed,
        } => {
            // Why: Both the `--from-committed` and default branches currently
            // resolve to the same shared-memories file under `.open-mpm/`.
            // The flag is kept on the CLI for forward compatibility (e.g. if
            // we later split the committed vs. local file paths) but it has
            // no effect today — flatten the branches to satisfy
            // `clippy::if_same_then_else` without changing behavior.
            let _ = from_committed;
            let in_path = if let Some(p) = input {
                p
            } else {
                cwd.join(".open-mpm").join(SHARED_MEMORIES_FILENAME)
            };
            let n = import_file(&cwd, &in_path).await?;
            println!(
                "[open-mpm] Imported {n} memories from {}",
                in_path.display()
            );
            Ok(())
        }
        Command::List { scope } => {
            list_sessions(&cwd, &scope).await?;
            Ok(())
        }
    }
}

/// Resolve the active session_id: prefer the run id env var, fall back to the
/// most recent registered session, finally "default".
pub fn current_session_id() -> String {
    if let Ok(rid) = std::env::var("OPEN_MPM_RUN_ID")
        && !rid.is_empty()
    {
        return rid;
    }
    "default".to_string()
}

/// Hostname (cross-platform). Falls back to "unknown" if the OS lookup fails.
fn local_machine_id() -> String {
    hostname::get()
        .ok()
        .and_then(|s| s.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Export records from `session_id` to a JSONL file (one record per line).
///
/// Why: Cross-machine sharing needs a stable, line-oriented format that
/// teammates can pull via git and import in one shot. With graph-native
/// segments now first-class, callers can scope exports to a single tier
/// (e.g. just `Brief`, just `History`) so a teammate can pick up the
/// active sprint without re-importing the entire knowledge base.
/// What: Walks every segment listed in `segments_to_export()` (one segment
/// when `segment_filter = Some(seg)`, all four memory tiers otherwise),
/// stamps each line with its `segment` name, and writes to `output`.
/// Test: `memory_export_includes_segment_field` and
/// `memory_export_filters_by_segment`.
pub async fn export_session(
    project_root: &Path,
    session_id: &str,
    output: &Path,
    segment_filter: Option<Segment>,
) -> Result<usize> {
    let session_dir = project_root
        .join(".open-mpm")
        .join("state")
        .join("sessions")
        .join(session_id);

    if !session_dir.exists() {
        bail!(
            "no session store at {} — has any memory been written for session '{}'?",
            session_dir.display(),
            session_id
        );
    }

    let store = RedbUsearchStore::open(&session_dir, ALL_MINI_LM_L6_V2_DIM)
        .with_context(|| format!("opening session store at {}", session_dir.display()))?;

    let machine = local_machine_id();
    let now = Utc::now().to_rfc3339();

    if let Some(parent) = output.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    let mut f = tokio::fs::File::create(output)
        .await
        .with_context(|| format!("creating {}", output.display()))?;

    // Default exports cover every memory-graph tier; CodeIndex stays out of
    // the default export because it's a build artifact, not portable knowledge.
    let segments: &[Segment] = match segment_filter {
        Some(Segment::AgentMemory) => &[Segment::AgentMemory],
        Some(Segment::CodeIndex) => &[Segment::CodeIndex],
        Some(Segment::Context) => &[Segment::Context],
        Some(Segment::Brief) => &[Segment::Brief],
        Some(Segment::History) => &[Segment::History],
        None => &[
            Segment::AgentMemory,
            Segment::Context,
            Segment::Brief,
            Segment::History,
        ],
    };

    let mut count = 0usize;
    for seg in segments {
        let records = store.list_segment(*seg).await?;
        let seg_name = segment_name(*seg);
        for (id, vector, payload) in records {
            // Skip auxiliary rows produced by MemoryGraph (edge:, run:, children:).
            if is_auxiliary_id(&id) {
                continue;
            }
            let content = payload
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let tag = payload
                .get("tag")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let sid = payload
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let rec = ExportRecord {
                id,
                content,
                tag,
                session_id: sid,
                machine_id: machine.clone(),
                exported_at: now.clone(),
                embedding: vector,
                payload,
                segment: Some(seg_name.to_string()),
            };
            let line = serde_json::to_string(&rec).context("serializing export record")?;
            f.write_all(line.as_bytes()).await?;
            f.write_all(b"\n").await?;
            count += 1;
        }
    }
    f.flush().await?;
    Ok(count)
}

/// Map a `Segment` to its serde-rename snake_case name.
///
/// Why: ExportRecord stores the segment as a string (forward-compatible with
/// future segments) but we want a canonical name everywhere we emit.
/// What: Mirrors `#[serde(rename_all = "snake_case")]` on `Segment`.
/// Test: Implicit in export round-trip tests.
fn segment_name(seg: Segment) -> &'static str {
    match seg {
        Segment::AgentMemory => "agent_memory",
        Segment::CodeIndex => "code_index",
        Segment::Context => "context",
        Segment::Brief => "brief",
        Segment::History => "history",
    }
}

/// Skip auxiliary keys (`edge:`, `run:`, etc.) that are infra rows, not
/// shareable memories. Mirrors `MemoryGraph::is_auxiliary_key`.
fn is_auxiliary_id(id: &str) -> bool {
    id.starts_with("edge:")
        || id.starts_with("run:")
        || id.starts_with("children:")
        || id.starts_with("kv-index")
        || id == "kv-index"
}

/// Import every JSONL record from `input` into the local imported-sessions
/// store. Each payload is stamped with `imported: true` and the original
/// `machine_id`/`session_id` are preserved.
///
/// Returns the number of records inserted (zero if the tracker says this
/// file's content hash is unchanged since the last import).
pub async fn import_file(project_root: &Path, input: &Path) -> Result<usize> {
    if !input.exists() {
        bail!("input file does not exist: {}", input.display());
    }

    let bytes = tokio::fs::read(input)
        .await
        .with_context(|| format!("reading {}", input.display()))?;
    let hash = sha256_hex(&bytes);

    let state_dir = project_root.join(".open-mpm").join("state");
    tokio::fs::create_dir_all(&state_dir).await.ok();
    let tracker_path = state_dir.join(IMPORT_TRACKER_FILENAME);
    let mut tracker = read_tracker(&tracker_path).await;

    let key = input.to_string_lossy().to_string();
    if let Some(prev) = tracker.files.get(&key)
        && prev.hash == hash
    {
        // Already imported the exact same contents.
        return Ok(0);
    }

    // Imported memories live in their own session dir so they're easy to
    // find/clean and obviously-not-local during inspection.
    let imported_dir = state_dir.join("sessions").join("imported");
    tokio::fs::create_dir_all(&imported_dir).await.ok();
    let store = RedbUsearchStore::open(&imported_dir, ALL_MINI_LM_L6_V2_DIM)
        .with_context(|| format!("opening imported store at {}", imported_dir.display()))?;

    let mut count = 0usize;
    let text = String::from_utf8_lossy(&bytes);
    for (lineno, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let rec: ExportRecord = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(line = lineno + 1, error = %e, "skipping malformed JSONL line");
                continue;
            }
        };
        // Build payload: start from the original payload (preserves any
        // producer-stamped fields), then overwrite/insert our import markers.
        let mut payload = rec.payload.clone();
        if !payload.is_object() {
            payload = json!({});
        }
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("content".to_string(), json!(rec.content));
            if let Some(t) = &rec.tag {
                obj.insert("tag".to_string(), json!(t));
            }
            if let Some(sid) = &rec.session_id {
                obj.insert("session_id".to_string(), json!(sid));
            }
            obj.insert("machine_id".to_string(), json!(rec.machine_id));
            obj.insert("imported".to_string(), json!(true));
            obj.insert("imported_at".to_string(), json!(Utc::now().to_rfc3339()));
            obj.insert("source_exported_at".to_string(), json!(rec.exported_at));
        }
        // Namespace the id so it can't collide with a local fact of the same
        // name (e.g., both machines use `kv:foo`).
        let import_id = format!("imported:{}:{}", rec.machine_id, rec.id);
        // Route to the segment named in the record; fall back to AgentMemory
        // for backward compatibility with exports that pre-date segmentation.
        let target_segment = rec
            .segment
            .as_deref()
            .and_then(Segment::from_name)
            .unwrap_or(Segment::AgentMemory);
        if let Err(e) = store
            .insert(target_segment, &import_id, &rec.embedding, payload)
            .await
        {
            tracing::warn!(error = %e, id = %import_id, "import: insert failed");
            continue;
        }
        count += 1;
    }

    // Update tracker.
    tracker.files.insert(
        key,
        ImportEntry {
            hash,
            imported_at: Utc::now().to_rfc3339(),
            count,
        },
    );
    write_tracker(&tracker_path, &tracker).await?;

    Ok(count)
}

async fn read_tracker(path: &Path) -> ImportTracker {
    match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => ImportTracker::default(),
    }
}

async fn write_tracker(path: &Path, tracker: &ImportTracker) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(tracker)?;
    tokio::fs::write(path, &bytes)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// List sessions reachable under `scope`. Print one row per session.
async fn list_sessions(project_root: &Path, scope: &str) -> Result<()> {
    let sessions_dir = project_root
        .join(".open-mpm")
        .join("state")
        .join("sessions");
    if !sessions_dir.exists() {
        println!("No sessions found at {}.", sessions_dir.display());
        return Ok(());
    }

    match scope {
        "session" => {
            let cur = current_session_id();
            println!("Current session: {cur}");
        }
        "all" => {
            // Tally tag-namespace counts across every session so the user can
            // see the taxonomy distribution at a glance.
            //
            // Why: With the hierarchical tag scheme (memories/, configuration/,
            // docs/) the raw session listing hid how memories are distributed
            // across the taxonomy. Grouping by top-level namespace surfaces
            // counts like "configuration/  21 entries (13 skills, 8 MCP)".
            // What: For each session dir, open its store, walk the AgentMemory
            // segment, and bin payloads by (top, full) tag. Then print the
            // per-session list followed by an aggregated namespace summary.
            let mut top_counts: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            let mut sub_counts: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            for entry in std::fs::read_dir(&sessions_dir)? {
                let entry = entry?;
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                println!("{name}");

                // Best-effort tally — a session whose dim doesn't match (or
                // that has no records) just contributes zero to the totals.
                let path = entry.path();
                if let Ok(store) = RedbUsearchStore::open(&path, ALL_MINI_LM_L6_V2_DIM)
                    && let Ok(recs) = store.list_segment(Segment::AgentMemory).await
                {
                    for (id, _vec, payload) in recs {
                        if is_auxiliary_id(&id) {
                            continue;
                        }
                        let tag = payload
                            .get("tag")
                            .and_then(|v| v.as_str())
                            .unwrap_or("(untagged)")
                            .to_string();
                        let top = tag.split('/').next().unwrap_or(&tag).to_string();
                        *top_counts.entry(top).or_insert(0) += 1;
                        *sub_counts.entry(tag).or_insert(0) += 1;
                    }
                }
            }
            if !top_counts.is_empty() {
                println!();
                println!("Tag namespace summary:");
                for (top, n) in &top_counts {
                    // Collect sub-tag breakdown for this top-level namespace.
                    let prefix = format!("{top}/");
                    let mut subs: Vec<(String, usize)> = sub_counts
                        .iter()
                        .filter(|(t, _)| t.starts_with(&prefix))
                        .map(|(t, c)| {
                            let sub = t.strip_prefix(&prefix).unwrap_or(t).to_string();
                            (sub, *c)
                        })
                        .collect();
                    subs.sort_by_key(|b| std::cmp::Reverse(b.1));
                    if subs.is_empty() {
                        println!("  {top}/    {n} entries");
                    } else {
                        let detail: Vec<String> =
                            subs.iter().map(|(s, c)| format!("{c} {s}")).collect();
                        println!("  {top}/    {n} entries  ({})", detail.join(", "));
                    }
                }
            }
        }
        "imported" => {
            let imported = sessions_dir.join("imported");
            if !imported.exists() {
                println!("No imported sessions yet.");
                return Ok(());
            }
            let store = RedbUsearchStore::open(&imported, ALL_MINI_LM_L6_V2_DIM)?;
            let recs = store.list_segment(Segment::AgentMemory).await?;
            // Group by source session_id for compactness.
            let mut by_session: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            for (_id, _vec, payload) in recs {
                let sid = payload
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(unknown)")
                    .to_string();
                *by_session.entry(sid).or_insert(0) += 1;
            }
            for (sid, n) in by_session {
                println!("{sid}\t{n} memories");
            }
        }
        _ => bail!("invalid scope: {scope}"),
    }
    Ok(())
}

/// Auto-import on startup: if `.open-mpm/shared-memories.jsonl` exists and
/// its hash differs from the tracker, import it. Best-effort — failures are
/// logged and never block startup.
pub async fn auto_import_if_changed(project_root: &Path) -> Result<usize> {
    let shared = project_root
        .join(".open-mpm")
        .join(SHARED_MEMORIES_FILENAME);
    if !shared.exists() {
        return Ok(0);
    }
    import_file(project_root, &shared).await
}

// Re-export the embedder type for callers that want to consume it without
// pulling in the whole memory module path. (Not currently used outside this
// file but kept for parity with `search_cmd`.)
#[allow(dead_code)]
fn _embedder_type_marker() -> Option<FastEmbedder> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parses_export_with_session_and_output() {
        let cmd =
            parse_args(&["export", "--session", "sess-1", "--output", "/tmp/x.jsonl"]).unwrap();
        assert_eq!(
            cmd,
            Command::Export {
                session: Some("sess-1".to_string()),
                output: Some(PathBuf::from("/tmp/x.jsonl")),
                segment: None,
            }
        );
    }

    #[test]
    fn parses_export_with_no_args() {
        let cmd = parse_args(&["export"]).unwrap();
        assert_eq!(
            cmd,
            Command::Export {
                session: None,
                output: None,
                segment: None,
            }
        );
    }

    #[test]
    fn parses_export_with_segment_filter() {
        let cmd = parse_args(&["export", "--segment", "brief"]).unwrap();
        assert_eq!(
            cmd,
            Command::Export {
                session: None,
                output: None,
                segment: Some("brief".to_string()),
            }
        );
    }

    #[test]
    fn rejects_invalid_export_segment() {
        // Unknown segment names must error before we open any stores.
        assert!(parse_args(&["export", "--segment", "bogus"]).is_err());
    }

    #[test]
    fn parses_import_from_committed() {
        let cmd = parse_args(&["import", "--from-committed"]).unwrap();
        assert_eq!(
            cmd,
            Command::Import {
                input: None,
                from_committed: true,
            }
        );
    }

    #[test]
    fn parses_list_with_scope() {
        let cmd = parse_args(&["list", "--scope", "imported"]).unwrap();
        assert_eq!(
            cmd,
            Command::List {
                scope: "imported".to_string()
            }
        );
    }

    #[test]
    fn parses_list_default_scope_is_session() {
        let cmd = parse_args(&["list"]).unwrap();
        assert_eq!(
            cmd,
            Command::List {
                scope: "session".to_string()
            }
        );
    }

    #[test]
    fn rejects_invalid_scope() {
        assert!(parse_args(&["list", "--scope", "bogus"]).is_err());
    }

    #[test]
    fn rejects_unknown_action() {
        assert!(parse_args(&["foo"]).is_err());
    }

    /// Helper: insert a memory into a session's store with the given session_id
    /// stamped into the payload, using a deterministic stub embedding.
    async fn insert_test_memory(project_root: &Path, session_id: &str, id: &str, content: &str) {
        let session_dir = project_root
            .join(".open-mpm")
            .join("state")
            .join("sessions")
            .join(session_id);
        std::fs::create_dir_all(&session_dir).unwrap();
        let store = RedbUsearchStore::open(&session_dir, 16).unwrap();
        let mut v = vec![0.0f32; 16];
        for (i, b) in content.bytes().take(16).enumerate() {
            v[i] = (b as f32) / 255.0;
        }
        let payload = json!({
            "content": content,
            "session_id": session_id,
            "tag": "memories/session",
            "created_at": Utc::now().to_rfc3339(),
        });
        store
            .insert(Segment::AgentMemory, id, &v, payload)
            .await
            .unwrap();
    }

    /// Same as insert_test_memory but uses ALL_MINI_LM_L6_V2_DIM to match the
    /// dimension export_session expects.
    async fn insert_test_memory_real_dim(
        project_root: &Path,
        session_id: &str,
        id: &str,
        content: &str,
    ) {
        let session_dir = project_root
            .join(".open-mpm")
            .join("state")
            .join("sessions")
            .join(session_id);
        std::fs::create_dir_all(&session_dir).unwrap();
        let store = RedbUsearchStore::open(&session_dir, ALL_MINI_LM_L6_V2_DIM).unwrap();
        let mut v = vec![0.0f32; ALL_MINI_LM_L6_V2_DIM];
        for (i, b) in content.bytes().take(ALL_MINI_LM_L6_V2_DIM).enumerate() {
            v[i] = (b as f32) / 255.0;
        }
        let payload = json!({
            "content": content,
            "session_id": session_id,
            "tag": "memories/session",
            "created_at": Utc::now().to_rfc3339(),
        });
        store
            .insert(Segment::AgentMemory, id, &v, payload)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn memory_export_produces_jsonl_with_machine_id() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        insert_test_memory_real_dim(&root, "sess-export", "fact-1", "Hello world.").await;

        let out = root.join("shared.jsonl");
        let n = export_session(&root, "sess-export", &out, None)
            .await
            .unwrap();
        assert_eq!(n, 1);
        assert!(out.exists());

        let body = std::fs::read_to_string(&out).unwrap();
        let line = body.lines().next().unwrap();
        let rec: ExportRecord = serde_json::from_str(line).unwrap();
        assert_eq!(rec.id, "fact-1");
        assert_eq!(rec.content, "Hello world.");
        assert_eq!(rec.session_id.as_deref(), Some("sess-export"));
        assert!(!rec.machine_id.is_empty(), "machine_id must be populated");
        assert_eq!(rec.embedding.len(), ALL_MINI_LM_L6_V2_DIM);
    }

    #[tokio::test]
    async fn memory_import_from_jsonl_marks_imported_true() {
        // Hand-craft a JSONL file with one record and import it; verify the
        // resulting payload in the imported store has imported=true.
        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join(".open-mpm")).unwrap();
        let in_path = root.join(".open-mpm").join(SHARED_MEMORIES_FILENAME);
        let rec = ExportRecord {
            id: "fact-x".to_string(),
            content: "Imported content.".to_string(),
            tag: Some("memories/session".to_string()),
            session_id: Some("remote-sess".to_string()),
            machine_id: "teammate-laptop".to_string(),
            exported_at: Utc::now().to_rfc3339(),
            embedding: vec![0.1f32; ALL_MINI_LM_L6_V2_DIM],
            payload: json!({"content": "Imported content.", "session_id": "remote-sess"}),
            segment: None,
        };
        std::fs::write(
            &in_path,
            format!("{}\n", serde_json::to_string(&rec).unwrap()),
        )
        .unwrap();

        let n = import_file(&root, &in_path).await.unwrap();
        assert_eq!(n, 1);

        // Inspect the imported store.
        let imported_dir = root
            .join(".open-mpm")
            .join("state")
            .join("sessions")
            .join("imported");
        let store = RedbUsearchStore::open(&imported_dir, ALL_MINI_LM_L6_V2_DIM).unwrap();
        let key = "imported:teammate-laptop:fact-x";
        let payload = store
            .get(Segment::AgentMemory, key)
            .await
            .unwrap()
            .expect("imported payload should exist");
        assert_eq!(
            payload.get("imported").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            payload.get("machine_id").and_then(|v| v.as_str()),
            Some("teammate-laptop")
        );
        assert_eq!(
            payload.get("session_id").and_then(|v| v.as_str()),
            Some("remote-sess")
        );
    }

    #[tokio::test]
    async fn auto_import_skips_when_hash_unchanged() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join(".open-mpm")).unwrap();
        let in_path = root.join(".open-mpm").join(SHARED_MEMORIES_FILENAME);

        let rec = ExportRecord {
            id: "fact-y".to_string(),
            content: "stable".to_string(),
            tag: None,
            session_id: Some("rs".to_string()),
            machine_id: "m1".to_string(),
            exported_at: Utc::now().to_rfc3339(),
            embedding: vec![0.0f32; ALL_MINI_LM_L6_V2_DIM],
            payload: json!({"content": "stable"}),
            segment: None,
        };
        std::fs::write(
            &in_path,
            format!("{}\n", serde_json::to_string(&rec).unwrap()),
        )
        .unwrap();

        let n1 = auto_import_if_changed(&root).await.unwrap();
        assert_eq!(n1, 1);
        // Second call with same file contents should be a no-op (returns 0).
        let n2 = auto_import_if_changed(&root).await.unwrap();
        assert_eq!(n2, 0, "unchanged file should not re-import");
    }

    #[tokio::test]
    async fn list_segment_round_trips_records() {
        // Sanity: list_segment returns what we inserted.
        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        insert_test_memory(&root, "sx", "a", "alpha").await;
        insert_test_memory(&root, "sx", "b", "beta").await;

        let session_dir = root
            .join(".open-mpm")
            .join("state")
            .join("sessions")
            .join("sx");
        let store = RedbUsearchStore::open(&session_dir, 16).unwrap();
        let recs = store.list_segment(Segment::AgentMemory).await.unwrap();
        let ids: Vec<&str> = recs.iter().map(|(id, _, _)| id.as_str()).collect();
        assert!(ids.contains(&"a"));
        assert!(ids.contains(&"b"));
    }

    /// Insert a record into a specific segment with full-dim embedding.
    async fn insert_into_segment(
        project_root: &Path,
        session_id: &str,
        segment: Segment,
        id: &str,
        content: &str,
    ) {
        let session_dir = project_root
            .join(".open-mpm")
            .join("state")
            .join("sessions")
            .join(session_id);
        std::fs::create_dir_all(&session_dir).unwrap();
        let store = RedbUsearchStore::open(&session_dir, ALL_MINI_LM_L6_V2_DIM).unwrap();
        let mut v = vec![0.0f32; ALL_MINI_LM_L6_V2_DIM];
        for (i, b) in content.bytes().take(ALL_MINI_LM_L6_V2_DIM).enumerate() {
            v[i] = (b as f32) / 255.0;
        }
        let payload = json!({
            "content": content,
            "session_id": session_id,
        });
        store.insert(segment, id, &v, payload).await.unwrap();
    }

    #[tokio::test]
    async fn memory_export_filters_by_segment() {
        // Insert records into two different segments; export with a filter
        // and verify only the matching tier shows up.
        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        insert_into_segment(&root, "sf", Segment::Brief, "b1", "active goal").await;
        insert_into_segment(&root, "sf", Segment::History, "h1", "decision X").await;

        let out = root.join("brief-only.jsonl");
        let n = export_session(&root, "sf", &out, Some(Segment::Brief))
            .await
            .unwrap();
        assert_eq!(n, 1, "only the Brief record should be exported");

        let body = std::fs::read_to_string(&out).unwrap();
        let line = body.lines().next().unwrap();
        let rec: ExportRecord = serde_json::from_str(line).unwrap();
        assert_eq!(rec.id, "b1");
        assert_eq!(rec.segment.as_deref(), Some("brief"));
    }

    #[tokio::test]
    async fn memory_import_routes_by_segment() {
        // A JSONL record stamped with segment=context must end up in the
        // imported store's Context segment, NOT AgentMemory.
        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join(".open-mpm")).unwrap();
        let in_path = root.join(".open-mpm").join(SHARED_MEMORIES_FILENAME);
        let rec = ExportRecord {
            id: "ctx-1".to_string(),
            content: "uses tokio runtime".to_string(),
            tag: None,
            session_id: Some("remote".to_string()),
            machine_id: "host-a".to_string(),
            exported_at: Utc::now().to_rfc3339(),
            embedding: vec![0.2f32; ALL_MINI_LM_L6_V2_DIM],
            payload: json!({"content": "uses tokio runtime"}),
            segment: Some("context".to_string()),
        };
        std::fs::write(
            &in_path,
            format!("{}\n", serde_json::to_string(&rec).unwrap()),
        )
        .unwrap();

        let n = import_file(&root, &in_path).await.unwrap();
        assert_eq!(n, 1);

        let imported_dir = root
            .join(".open-mpm")
            .join("state")
            .join("sessions")
            .join("imported");
        let store = RedbUsearchStore::open(&imported_dir, ALL_MINI_LM_L6_V2_DIM).unwrap();
        let key = "imported:host-a:ctx-1";
        // Should land in Context, not AgentMemory.
        assert!(store.get(Segment::Context, key).await.unwrap().is_some());
        assert!(
            store
                .get(Segment::AgentMemory, key)
                .await
                .unwrap()
                .is_none(),
            "context-tagged record must not leak into AgentMemory"
        );
    }

    #[tokio::test]
    async fn memory_import_without_segment_defaults_to_agent_memory() {
        // Backward-compat: legacy JSONL exports (no `segment` field) must
        // continue to import into AgentMemory.
        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join(".open-mpm")).unwrap();
        let in_path = root.join(".open-mpm").join(SHARED_MEMORIES_FILENAME);
        let rec = ExportRecord {
            id: "legacy-1".to_string(),
            content: "legacy fact".to_string(),
            tag: None,
            session_id: None,
            machine_id: "old-host".to_string(),
            exported_at: Utc::now().to_rfc3339(),
            embedding: vec![0.0f32; ALL_MINI_LM_L6_V2_DIM],
            payload: json!({"content": "legacy fact"}),
            segment: None,
        };
        std::fs::write(
            &in_path,
            format!("{}\n", serde_json::to_string(&rec).unwrap()),
        )
        .unwrap();

        let n = import_file(&root, &in_path).await.unwrap();
        assert_eq!(n, 1);

        let imported_dir = root
            .join(".open-mpm")
            .join("state")
            .join("sessions")
            .join("imported");
        let store = RedbUsearchStore::open(&imported_dir, ALL_MINI_LM_L6_V2_DIM).unwrap();
        let key = "imported:old-host:legacy-1";
        assert!(
            store
                .get(Segment::AgentMemory, key)
                .await
                .unwrap()
                .is_some(),
            "legacy record should default to AgentMemory"
        );
    }
}
