//! `seed_mcp_connections` — index `.mcp.json` server definitions into memory.
//!
//! Why: Agents should be able to recall "what MCP servers are available?" via
//! `memory_recall` without bespoke tooling.
//! What: Extends `ProjectInitializer` with `seed_mcp_connections`.
//! Test: `seed_mcp_connections_indexes_servers` in `init::tests`.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use chrono::Utc;

use super::{file_mtime_secs, render_mcp_description};
use crate::init::{MCP_SEED_SESSION_ID, ProjectInitializer};
use crate::memory::{Embedder, MemoryStore, Segment};

impl ProjectInitializer {
    /// Seed agent memory with MCP server connection definitions.
    ///
    /// Why: Agents using `memory_recall` should be able to ask "what MCP
    /// servers are available?" or "is there a server that can search vector
    /// databases?" and get the right hit. Indexing the MCP config gives them
    /// that visibility without bespoke tooling.
    /// What: Reads `.mcp.json` from the project root (and `~/.claude/.mcp.json`
    /// if present), parses the `mcpServers` map, and for each server builds a
    /// human-readable description embedding command/args/env. Stored in
    /// `Segment::AgentMemory` with a stable id (`mcp:<server_name>`) and
    /// payload `{ "content": ..., "tag": "configuration/mcp",
    /// "session_id": "seed/configuration/mcp", "server_name": ..., "command": ...,
    /// "path": ... }`. Tracked in `.trusty-agents/state/mcp_seeded.json`.
    /// Test: `seed_mcp_connections_indexes_servers` in `init::tests`.
    pub async fn seed_mcp_connections(
        &self,
        store: &dyn MemoryStore,
        embedder: &dyn Embedder,
    ) -> Result<usize> {
        let mut sources: Vec<PathBuf> = Vec::new();
        let project_mcp = self.project_dir.join(".mcp.json");
        if project_mcp.exists() {
            sources.push(project_mcp);
        }
        if let Some(home) = std::env::var_os("HOME") {
            let user_mcp = PathBuf::from(home).join(".claude").join(".mcp.json");
            if user_mcp.exists() {
                sources.push(user_mcp);
            }
        }
        if sources.is_empty() {
            tracing::debug!("seed_mcp_connections: no .mcp.json present, skipping");
            return Ok(0);
        }

        let seeded_path = self.agent_dir.join("mcp_seeded.json");
        let mut seeded: HashMap<String, u64> = match tokio::fs::read(&seeded_path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => HashMap::new(),
        };

        let mut seeded_count: usize = 0;
        for source in &sources {
            let mtime_secs = file_mtime_secs(source).await;

            let raw = match tokio::fs::read_to_string(source).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %source.display(),
                        "seed_mcp_connections: read failed (skipping)"
                    );
                    continue;
                }
            };
            let parsed: serde_json::Value = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %source.display(),
                        "seed_mcp_connections: parse failed (skipping)"
                    );
                    continue;
                }
            };

            let servers = match parsed.get("mcpServers").and_then(|v| v.as_object()) {
                Some(s) => s,
                None => {
                    tracing::debug!(
                        path = %source.display(),
                        "seed_mcp_connections: no mcpServers key, skipping"
                    );
                    continue;
                }
            };

            let rel = source
                .strip_prefix(&self.project_dir)
                .unwrap_or(source)
                .to_string_lossy()
                .to_string();

            for (server_name, def) in servers {
                let track_key = format!("{rel}#{server_name}");
                if let Some(prev) = seeded.get(&track_key)
                    && *prev >= mtime_secs
                    && mtime_secs > 0
                {
                    continue;
                }

                let command = def
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let args: Vec<String> = def
                    .get("args")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let description = def
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let env_keys: Vec<String> = def
                    .get("env")
                    .and_then(|v| v.as_object())
                    .map(|o| o.keys().cloned().collect())
                    .unwrap_or_default();

                let content =
                    render_mcp_description(server_name, &command, &args, &description, &env_keys);

                let vec = match embedder.embed_single(&content) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            server = %server_name,
                            "seed_mcp_connections: embed failed (skipping)"
                        );
                        continue;
                    }
                };
                let id = format!("mcp:{server_name}");
                let payload = serde_json::json!({
                    "content": content,
                    "tag": "configuration/mcp",
                    "session_id": MCP_SEED_SESSION_ID,
                    "server_name": server_name,
                    "command": command,
                    "args": args,
                    "env_keys": env_keys,
                    "path": rel.clone(),
                    "created_at": Utc::now().to_rfc3339(),
                });
                if let Err(e) = store.insert(Segment::AgentMemory, &id, &vec, payload).await {
                    tracing::warn!(error = %e, id = %id, "seed_mcp_connections: insert failed");
                    continue;
                }

                seeded.insert(track_key, mtime_secs);
                seeded_count += 1;
            }
        }

        if let Ok(bytes) = serde_json::to_vec_pretty(&seeded)
            && let Err(e) = tokio::fs::write(&seeded_path, &bytes).await
        {
            tracing::warn!(
                error = %e,
                path = %seeded_path.display(),
                "seed_mcp_connections: write tracker failed (continuing)"
            );
        }

        Ok(seeded_count)
    }
}
