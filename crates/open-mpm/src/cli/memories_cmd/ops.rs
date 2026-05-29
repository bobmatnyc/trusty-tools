//! Export / import / list / auto-import implementations for `memories`.
//!
//! Why: The store-backed I/O is the bulk of the command; isolating it from the
//! CLI parsing keeps both files focused and under the 500-line cap.
//! What: `export_session`, `import_file`, `list_sessions`,
//! `auto_import_if_changed`, plus the machine-id / segment-name / hashing /
//! tracker helpers.
//! Test: Round-trips and segment routing covered in `memories_cmd::tests`.

use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use super::{
    ExportRecord, IMPORT_TRACKER_FILENAME, ImportEntry, ImportTracker, SHARED_MEMORIES_FILENAME,
    current_session_id,
};
use crate::memory::store::{MemoryStore, Segment};
use crate::memory::{RedbUsearchStore, embed::ALL_MINI_LM_L6_V2_DIM};

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
pub(super) async fn list_sessions(project_root: &Path, scope: &str) -> Result<()> {
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
