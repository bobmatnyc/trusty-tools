//! Ollama probe helpers for the REPL.
//!
//! Why: `/provider local` and `/local` slash commands need to verify a
//! running ollama instance and list its pulled models before flipping the
//! provider override. Extracting these helpers keeps `mod.rs` lean and
//! makes the probe path independently testable.
//! What: `probe_ollama` GETs `<host>/api/tags` and returns the names;
//! `ollama_host` resolves the host from `OLLAMA_HOST` (defaulting to
//! `http://localhost:11434`).
//! Test: Exercised indirectly via `/provider local` and `/local test`
//! REPL paths; manual smoke test against a live ollama.

use std::time::Duration;

use anyhow::{Context, Result};

/// Probe a local ollama server and return the list of model names.
///
/// Why: `/provider local` lets the user route LLM calls to a locally-running
/// ollama instance. Before flipping the override we must verify ollama is
/// running and surface its model list so the user can pick one with `/model`.
/// What: GETs `<host>/api/tags`, parses `{"models":[{"name":"..."},...]}`,
/// returns the names. 2-second timeout keeps the REPL responsive when ollama
/// is offline.
/// Test: Unit-tested indirectly via `/provider local` path (manual REPL).
pub(crate) async fn probe_ollama(host: &str) -> Result<Vec<String>> {
    let url = format!("{}/api/tags", host.trim_end_matches('/'));
    let resp = crate::llm::http_client()
        .get(&url)
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .with_context(|| format!("connecting to {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("ollama responded with status {status}");
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .context("parsing ollama /api/tags response")?;
    let names = body
        .get("models")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(names)
}

/// Resolve the configured ollama host (env override or default).
pub(crate) fn ollama_host() -> String {
    std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string())
}
