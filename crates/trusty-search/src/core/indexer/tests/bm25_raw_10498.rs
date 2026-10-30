//! #10498: the raw BM25 score reaches each BM25-lane hit and the debug log, and
//! ranking is untouched (the D49 closure condition).

use super::*;
use crate::core::classifier::{QueryClassifier, QueryIntent};

/// Collects every log line at DEBUG and above written on this thread.
#[derive(Clone, Default)]
struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture() -> (Capture, tracing::subscriber::DefaultGuard) {
    let cap = Capture::default();
    let writer = cap.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .without_time()
        .finish();
    (cap, tracing::subscriber::set_default(subscriber))
}

async fn corpus() -> CodeIndexer {
    let idx = make_indexer();
    for (id, file, body) in [
        (
            "src/auth.rs:1:5",
            "src/auth.rs",
            "fn authenticate(user: &str, password: &str) -> bool { true }",
        ),
        (
            "src/session.rs:1:4",
            "src/session.rs",
            "fn authenticate_session(token: &str) -> bool { check(token) }",
        ),
        (
            "src/render.rs:1:3",
            "src/render.rs",
            "fn render_ui_components() { /* svelte */ }",
        ),
        (
            "src/db.rs:1:4",
            "src/db.rs",
            "struct Database { conn: String }",
        ),
        (
            "src/pay.rs:1:6",
            "src/pay.rs",
            "fn payment_processor_retry(attempts: u32) { /* TODO retry five times */ }",
        ),
        (
            "src/user.rs:1:6",
            "src/user.rs",
            "struct UserAccount { password_hash: String, salt: String }",
        ),
    ] {
        idx.add_chunk(raw(id, file, body)).await.expect("add chunk");
    }
    idx
}

fn query(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.to_string(),
        top_k: 10,
        expand_graph: false,
        ..Default::default()
    }
}

/// A BM25-lane hit carries the BM25 engine's own score for that doc.
///
/// Test: this test.
#[tokio::test]
async fn a_bm25_hit_carries_the_engines_raw_score() {
    let idx = corpus().await;
    let engine = idx
        .bm25_search("authenticate", 40, None)
        .await
        .expect("bm25");
    assert!(!engine.is_empty(), "fixture: BM25 matches the query");
    let hits = idx.search(&query("authenticate")).await.expect("search");
    for (id, raw_score) in &engine {
        let hit = hits.iter().find(|h| &h.id == id).expect("bm25 hit on page");
        assert_eq!(
            hit.bm25_raw,
            Some(*raw_score),
            "{id}: bm25_raw must equal the engine score, not the fused score {}",
            hit.score
        );
    }
}

/// A vector-only hit has no `bm25_raw`, and the key is absent from its JSON.
///
/// Test: this test.
#[tokio::test]
async fn a_vector_only_hit_has_no_bm25_raw_and_no_json_key() {
    let idx = corpus().await;
    let hits = idx.search(&query("authenticate")).await.expect("search");
    let vector_only = hits
        .iter()
        .find(|h| h.match_reason == "vector")
        .expect("fixture: the mock embedder returns docs BM25 does not match");
    assert_eq!(vector_only.bm25_raw, None);
    let json = serde_json::to_value(vector_only).expect("serialize");
    assert!(
        json.get("bm25_raw").is_none(),
        "bm25_raw must be absent, got {json}"
    );
    let bm25_hit = hits
        .iter()
        .find(|h| h.bm25_raw.is_some())
        .expect("a bm25 hit");
    assert!(serde_json::to_value(bm25_hit).unwrap()["bm25_raw"].is_number());
}

/// Ranking identity: the ordered ids and exact fused scores for one query per
/// intent match a snapshot taken before the field existed. Scores are compared
/// as raw bits, so any change to RRF, the intent weights, a floor or the grep
/// merge fails here.
///
/// Test: this test.
#[tokio::test]
async fn ranking_is_byte_identical_to_the_pre_change_snapshot() {
    let idx = corpus().await;
    let queries = [
        ("struct Database", QueryIntent::Definition),
        ("callers of authenticate", QueryIntent::Usage),
        ("how does the password check work", QueryIntent::Conceptual),
        ("TODO retry", QueryIntent::BugDebt),
        ("authenticate", QueryIntent::Keyword),
        (
            "the payment_processor retries failed attempts five times",
            QueryIntent::Unknown,
        ),
    ];
    let mut snapshot = String::new();
    for (text, want_intent) in queries {
        assert_eq!(
            QueryClassifier::classify_with_domain(text, &[]),
            want_intent,
            "fixture: {text:?} must keep its intent"
        );
        snapshot.push_str(&format!("# {want_intent:?}\n"));
        for hit in idx.search(&query(text)).await.expect("search") {
            snapshot.push_str(&format!("{} {:08x}\n", hit.id, hit.score.to_bits()));
        }
    }
    if std::env::var("PRINT_SNAPSHOT_10498").is_ok() {
        println!("SNAPSHOT_BEGIN\n{snapshot}SNAPSHOT_END");
    }
    assert_eq!(snapshot, SNAPSHOT, "ranking changed:\n{snapshot}");
}

/// The per-query debug line carries each top hit's raw BM25 score and ranks,
/// and never the query text.
///
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn the_debug_log_names_raw_bm25_scores_without_the_query_text() {
    let idx = corpus().await;
    let (cap, _guard) = capture();
    let text = "authenticate";
    idx.search(&query(text)).await.expect("search");
    let log = cap.text();
    let line = log
        .lines()
        .find(|l| l.contains("bm25 raw scores"))
        .unwrap_or_else(|| panic!("no bm25 raw score line in:\n{log}"));
    for field in [
        "DEBUG",
        "index_id=test",
        "query_id=",
        "src/auth.rs:1:5",
        "bm25_rank=",
        "fused_rank=",
    ] {
        assert!(line.contains(field), "{field} missing from: {line}");
    }
    assert!(!line.contains(text), "query text leaked into: {line}");
}

impl Capture {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("capture lock")).into_owned()
    }
}

/// Captured from the pre-change ranking (stub commit, ranking code untouched).
const SNAPSHOT: &str = concat!(
    "# Definition\n",
    "src/db.rs:1:4 3fc20e00\n",
    "src/user.rs:1:6 3c837ff6\n",
    "src/render.rs:1:3 3b9e8e0a\n",
    "src/auth.rs:1:5 3b99999a\n",
    "src/session.rs:1:4 3b973ca7\n",
    "src/pay.rs:1:6 3b94f20a\n",
    "# Usage\n",
    "src/auth.rs:1:5 3c864b8a\n",
    "src/session.rs:1:4 3c831494\n",
    "src/pay.rs:1:6 3c042108\n",
    "src/render.rs:1:3 3c000000\n",
    "src/user.rs:1:6 3bfc0fc1\n",
    "src/db.rs:1:4 3bf83e10\n",
    "# Conceptual\n",
    "src/session.rs:1:4 3c864b8a\n",
    "src/user.rs:1:6 3c83b5a7\n",
    "src/auth.rs:1:5 3c80d368\n",
    "src/pay.rs:1:6 3c500d02\n",
    "src/render.rs:1:3 3c49a634\n",
    "src/db.rs:1:4 3c46980d\n",
    "# BugDebt\n",
    "src/pay.rs:1:6 3c8577f9\n",
    "src/render.rs:1:3 3ad6df43\n",
    "src/db.rs:1:4 3ad3680d\n",
    "src/auth.rs:1:5 3ad00d02\n",
    "src/user.rs:1:6 3acccccd\n",
    "src/session.rs:1:4 3ac6980d\n",
    "# Keyword\n",
    "src/auth.rs:1:5 3c82849e\n",
    "src/session.rs:1:4 3c7ea610\n",
    "src/user.rs:1:6 3c212773\n",
    "src/render.rs:1:3 3c1e8e0a\n",
    "src/pay.rs:1:6 3c1c09c2\n",
    "src/db.rs:1:4 3c173ca7\n",
    "# Unknown\n",
    "src/pay.rs:1:6 3c83bcb2\n",
    "src/render.rs:1:3 3c212773\n",
    "src/user.rs:1:6 3c1e8e0a\n",
    "src/auth.rs:1:5 3c19999a\n",
    "src/db.rs:1:4 3c173ca7\n",
    "src/session.rs:1:4 3c14f20a\n",
);
