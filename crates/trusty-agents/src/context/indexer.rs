//! Background turn indexer (#70).
//!
//! Why: Every agent turn (prompt+response) is potentially useful to a later
//! run — but embedding+persisting inline would slow the hot path. A
//! fire-and-forget mpsc channel lets the caller drop a `TurnRecord` without
//! awaiting I/O; a single background task embeds via OpenRouter's embeddings
//! endpoint and appends to `entries.jsonl`.
//! What: `HistoryIndexer::spawn` launches the background task and returns a
//! handle whose `record()` method is non-blocking. Failures (no API key,
//! network errors, invalid responses) are logged at warn level; the process
//! continues.
//! Test: See unit tests — they exercise the pure helpers (`tokenize`,
//! serialization round-trip) without hitting the network.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// A single agent turn captured for later recall.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecord {
    pub session_id: String,
    pub agent: String,
    pub turn_number: u32,
    pub timestamp: DateTime<Utc>,
    pub prompt_text: String,
    pub response_text: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

/// A persisted entry: the turn plus its embedding and BM25 tokenization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedEntry {
    pub id: String,
    pub turn: TurnRecord,
    pub embedding: Vec<f32>,
    pub bm25_terms: Vec<String>,
}

/// Fire-and-forget indexer handle.
#[derive(Clone)]
pub struct HistoryIndexer {
    tx: mpsc::Sender<TurnRecord>,
}

impl HistoryIndexer {
    /// Spawn the background indexer task and return a clonable handle.
    ///
    /// Why: Callers want to share the handle across the workflow engine and
    /// any sub-agents; `Clone` on `mpsc::Sender` makes this cheap.
    /// What: Creates a bounded channel (256), spawns `run_indexer`.
    /// Test: `spawn_then_drop_handle_shuts_down_cleanly`.
    pub fn spawn(store_dir: PathBuf, openrouter_api_key: String) -> Self {
        let (tx, rx) = mpsc::channel::<TurnRecord>(256);
        tokio::spawn(async move {
            run_indexer(rx, store_dir, openrouter_api_key).await;
        });
        Self { tx }
    }

    /// Enqueue a turn for background embedding + persistence. Never blocks.
    ///
    /// Why: The caller is on a latency-sensitive path (workflow phase); a
    /// full channel should drop the record silently rather than stall.
    /// What: `try_send` is non-blocking; on full channel, the record is
    /// dropped (indexer is intentionally best-effort).
    /// Test: Indirect (workflow engine integration).
    pub fn record(&self, turn: TurnRecord) {
        if let Err(e) = self.tx.try_send(turn) {
            tracing::debug!(error = %e, "history indexer: dropping turn (channel full)");
        }
    }
}

async fn run_indexer(mut rx: mpsc::Receiver<TurnRecord>, store_dir: PathBuf, api_key: String) {
    if let Err(e) = tokio::fs::create_dir_all(&store_dir).await {
        tracing::warn!(error = %e, dir = %store_dir.display(), "history indexer: failed to create store dir");
    }
    let entries_path = store_dir.join("entries.jsonl");

    while let Some(turn) = rx.recv().await {
        if api_key.is_empty() {
            tracing::debug!("history indexer: no api key, skipping embedding");
            continue;
        }
        match embed_turn(&turn, &api_key).await {
            Ok(embedding) => {
                let entry = IndexedEntry {
                    id: uuid::Uuid::new_v4().to_string(),
                    bm25_terms: tokenize(&format!("{} {}", turn.prompt_text, turn.response_text)),
                    embedding,
                    turn,
                };
                if let Ok(line) = serde_json::to_string(&entry) {
                    use tokio::io::AsyncWriteExt;
                    match tokio::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&entries_path)
                        .await
                    {
                        Ok(mut f) => {
                            // Write the line first; return early on failure so we
                            // do not flush a partial write. Then flush to ensure
                            // the bytes are visible to the retriever's read-back
                            // in the same process (Tokio's File uses spawn_blocking
                            // internally — write_all resolving does NOT guarantee
                            // the OS buffer is committed).
                            if let Err(e) = f.write_all(format!("{line}\n").as_bytes()).await {
                                tracing::warn!(error = %e, "history indexer: write failed");
                            } else if let Err(e) = f.flush().await {
                                tracing::warn!(error = %e, "history indexer: flush failed");
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, path = %entries_path.display(), "history indexer: open failed");
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "history indexer: embed failed");
            }
        }
    }
}

async fn embed_turn(turn: &TurnRecord, api_key: &str) -> anyhow::Result<Vec<f32>> {
    let prompt_prefix: String = turn.prompt_text.chars().take(2000).collect();
    let response_prefix: String = turn.response_text.chars().take(2000).collect();
    let text = format!(
        "Agent: {}\nTask: {}\nResponse: {}",
        turn.agent, prompt_prefix, response_prefix
    );

    let client = reqwest::Client::new();
    let resp = client
        .post("https://openrouter.ai/api/v1/embeddings")
        .bearer_auth(api_key)
        .json(&serde_json::json!({
            "model": "openai/text-embedding-3-small",
            "input": text
        }))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let embedding: Vec<f32> = resp["data"][0]["embedding"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("no embedding in response: {resp}"))?
        .iter()
        .filter_map(|v| v.as_f64().map(|f| f as f32))
        .collect();

    Ok(embedding)
}

/// Lowercasing, camelCase-aware tokenizer used for BM25 indexing.
///
/// Why: BM25 wants a bag of normalized terms. Without camelCase splitting,
/// `getUserProfile` is indexed as the single opaque token "getuserprofile",
/// so a query for "get user" / "user profile" misses the document entirely
/// (closes #1542). Splitting at camelCase / PascalCase boundaries adds the
/// component terms without changing the contract for short-token dropping.
/// What: Splits text on non-alphanumeric boundaries, then further splits
/// each alphanumeric run at camelCase / PascalCase transitions. Lowercases
/// every emitted token and drops tokens ≤ 2 chars. Deduplicates before
/// returning so the BM25 term list is stable.
/// Test: `tokenize_lowers_and_drops_short`, `tokenize_splits_camel_case`.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        if raw.len() > 2 {
            tokens.push(raw.to_lowercase());
        }
        // Emit camelCase / PascalCase sub-parts (e.g. "getUserProfile" →
        // "get", "user", "profile"). Parts ≤ 2 chars are dropped to match the
        // outer filter so the existing short-token contract is preserved.
        let parts = split_camel_case_indexer(raw);
        if parts.len() > 1 {
            for part in parts {
                if part.len() > 2 {
                    tokens.push(part.to_lowercase());
                }
            }
        }
    }
    tokens.sort_unstable();
    tokens.dedup();
    tokens
}

/// Split an identifier at camelCase / PascalCase boundaries.
///
/// Why: Kept private; only `tokenize` above should call it. Extracted so the
/// boundary logic is testable in isolation without bloating `tokenize`.
/// What: Inserts split points at lowercase/digit → uppercase transitions
/// (`codeIndexer` → `["code", "Indexer"]`) and at acronym-to-word transitions
/// (`HTTPSClient` → `["HTTPS", "Client"]`). Returns the whole input verbatim
/// when it has fewer than two characters.
/// Test: Covered transitively by `tokenize_splits_camel_case`.
fn split_camel_case_indexer(s: &str) -> Vec<&str> {
    let bytes_len = s.len();
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    if chars.len() < 2 {
        return vec![s];
    }
    let mut bounds: Vec<usize> = vec![0];
    for i in 1..chars.len() {
        let (idx, c) = chars[i];
        let (_, prev) = chars[i - 1];
        // lowercase / digit → uppercase: new word begins.
        let lower_to_upper = (prev.is_lowercase() || prev.is_ascii_digit()) && c.is_uppercase();
        // Acronym run followed by a new word: "HTTPSClient" splits before 'C'.
        let acronym_to_word = prev.is_uppercase()
            && c.is_uppercase()
            && i + 1 < chars.len()
            && chars[i + 1].1.is_lowercase();
        if lower_to_upper || acronym_to_word {
            bounds.push(idx);
        }
    }
    bounds.push(bytes_len);
    bounds
        .windows(2)
        .map(|w| &s[w[0]..w[1]])
        .filter(|p| !p.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_lowers_and_drops_short() {
        let toks = tokenize("Hello, World!  A rust test.");
        assert!(toks.iter().any(|t| t == "hello"));
        assert!(toks.iter().any(|t| t == "world"));
        assert!(toks.iter().any(|t| t == "rust"));
        // Short words (<=2 chars) and punctuation should not appear.
        assert!(!toks.iter().any(|t| t == "a"));
        assert!(!toks.iter().any(|t| t.is_empty()));
    }

    #[test]
    fn tokenize_splits_camel_case() {
        // Why: closes #1542 — BM25 recall gap when queries use word fragments
        // of camelCase identifiers (e.g. "get user" should hit "getUserProfile").
        // What: asserts that camelCase sub-parts are emitted alongside the raw
        // lowercased token so both exact-identifier and word-fragment queries hit.
        // Test: directly exercises `tokenize("getUserProfile")`.
        let toks = tokenize("getUserProfile");
        // Raw lowercased token must be present (exact-identifier match still wins).
        assert!(
            toks.iter().any(|t| t == "getuserprofile"),
            "raw lowercased token missing: {toks:?}"
        );
        // camelCase parts (>2 chars) must also be present.
        assert!(
            toks.iter().any(|t| t == "get"),
            "camelCase part 'get' missing: {toks:?}"
        );
        assert!(
            toks.iter().any(|t| t == "user"),
            "camelCase part 'user' missing: {toks:?}"
        );
        assert!(
            toks.iter().any(|t| t == "profile"),
            "camelCase part 'profile' missing: {toks:?}"
        );
        // Short-token filter must still apply: no empty or ≤2-char tokens.
        assert!(
            !toks.iter().any(|t| t.len() <= 2),
            "short token leaked: {toks:?}"
        );
        // PascalCase: "GetUserProfile" should also split correctly.
        let toks2 = tokenize("GetUserProfile");
        assert!(
            toks2.iter().any(|t| t == "get"),
            "PascalCase split failed: {toks2:?}"
        );
        assert!(
            toks2.iter().any(|t| t == "user"),
            "PascalCase split failed: {toks2:?}"
        );
        assert!(
            toks2.iter().any(|t| t == "profile"),
            "PascalCase split failed: {toks2:?}"
        );
    }

    #[test]
    fn indexed_entry_round_trips_json() {
        let e = IndexedEntry {
            id: "id-1".into(),
            turn: TurnRecord {
                session_id: "s".into(),
                agent: "a".into(),
                turn_number: 1,
                timestamp: Utc::now(),
                prompt_text: "p".into(),
                response_text: "r".into(),
                prompt_tokens: 1,
                completion_tokens: 2,
            },
            embedding: vec![0.1, 0.2],
            bm25_terms: vec!["foo".into()],
        };
        let s = serde_json::to_string(&e).unwrap();
        let back: IndexedEntry = serde_json::from_str(&s).unwrap();
        assert_eq!(back.id, "id-1");
        assert_eq!(back.embedding, vec![0.1, 0.2]);
    }
}
