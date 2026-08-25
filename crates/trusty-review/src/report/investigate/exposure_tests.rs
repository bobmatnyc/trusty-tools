//! Tests for the bind/exposure collector (#6191).

use super::*;

fn file(path: &str, content: &str) -> SelectedFile {
    SelectedFile {
        path: path.to_string(),
        content: content.to_string(),
        truncated: false,
        dimensions: Vec::new(),
        selected_by: None,
        hotspot: None,
        declared_for: None,
    }
}

/// The base case the whole issue rests on: the bind address is IN the source,
/// and the collector reads it there rather than waiting for a finding to spell
/// `localhost`.
#[test]
fn a_loopback_bind_is_collected_with_its_line() {
    let facts = collect(&[file(
        "src/daemon.rs",
        "fn main() {\n    let l = TcpListener::bind(\"127.0.0.1:7878\").unwrap();\n}\n",
    )]);
    assert_eq!(facts.len(), 1, "{facts:?}");
    assert_eq!(facts[0].kind, ExposureKind::LoopbackBind);
    assert_eq!(facts[0].file, "src/daemon.rs");
    assert!(facts[0].evidence.contains("127.0.0.1:7878"), "{facts:?}");
    assert!(!facts[0].kind.is_beyond_host());
}

/// One file, two binds. The public one is the fact about the surface — a
/// loopback bind elsewhere in the same file does not make it host-local.
#[test]
fn a_public_bind_beats_a_loopback_bind_on_one_file() {
    let facts = collect(&[file(
        "src/serve.rs",
        "let dev = TcpListener::bind(\"127.0.0.1:0\")?;\nlet prod = TcpListener::bind(\"0.0.0.0:8080\")?;\n",
    )]);
    assert_eq!(facts[0].kind, ExposureKind::PublicBind);
    assert!(facts[0].kind.is_beyond_host());
}

/// The Telegram-gateway shape from the issue: no bind at all, an outbound call
/// to a third-party API. It used to classify as unestablished, which withheld
/// every true reach claim about it.
#[test]
fn a_collected_outbound_call_is_reach_evidence() {
    let facts = collect(&[file(
        "src/telegram.rs",
        "const API: &str = \"https://api.telegram.org/bot\";\n",
    )]);
    assert_eq!(facts.len(), 1, "{facts:?}");
    assert_eq!(facts[0].kind, ExposureKind::NetworkClient);
    assert!(facts[0].kind.is_beyond_host());
}

/// A file that both binds loopback and calls out is a loopback-LISTENING
/// surface. Its outbound traffic does not make it reachable.
#[test]
fn an_outbound_url_is_weaker_than_a_bind() {
    let facts = collect(&[file(
        "src/gateway.rs",
        "let l = TcpListener::bind(\"127.0.0.1:9000\")?;\nlet api = \"https://api.example.com/v1\";\n",
    )]);
    assert_eq!(facts[0].kind, ExposureKind::LoopbackBind);
}

/// A loopback URL is not an outbound call.
#[test]
fn a_loopback_url_is_not_an_outbound_call() {
    let facts = collect(&[file(
        "src/client.rs",
        "let base = \"http://127.0.0.1:7879\";\nlet other = \"http://localhost:7878/health\";\n",
    )]);
    assert!(facts.is_empty(), "{facts:?}");
}

/// No evidence is a real answer. A file stating nothing produces no fact, which
/// is what leaves the guard's text-marker path in place for it.
#[test]
fn a_file_stating_nothing_yields_no_fact() {
    let facts = collect(&[file(
        "src/util.rs",
        "pub fn add(a: u8, b: u8) -> u8 { a + b }\n",
    )]);
    assert!(facts.is_empty(), "{facts:?}");
}

/// An address in prose is not a bind. Without a bind marker on the line the
/// collector says nothing rather than guessing — guessing is what #6191 exists
/// to stop.
#[test]
fn a_commented_address_without_a_bind_marker_is_not_evidence() {
    let facts = collect(&[file(
        "src/notes.rs",
        "// the daemon answers on 0.0.0.0 in production\npub const PORT: u16 = 8080;\n",
    )]);
    assert!(facts.is_empty(), "{facts:?}");
}

/// A bind site whose address came from config states nothing about where it
/// binds, so it is not evidence either way.
#[test]
fn a_bind_with_no_address_literal_is_not_evidence() {
    let facts = collect(&[file(
        "src/serve.rs",
        "let l = TcpListener::bind(cfg.addr).await?;\n",
    )]);
    assert!(facts.is_empty(), "{facts:?}");
}

/// The guard normalises a component to a lower-cased path before it looks up,
/// so the index must be keyed the same way or a fact never matches its finding.
#[test]
fn the_index_is_keyed_by_lowercased_path() {
    let facts = vec![ExposureFact {
        file: "Crates/App/Src/Serve.rs".to_string(),
        kind: ExposureKind::PublicBind,
        evidence: "bind(\"0.0.0.0:80\")".to_string(),
    }];
    let index = ExposureIndex::from_facts(&facts);
    assert_eq!(
        index.kind("crates/app/src/serve.rs"),
        Some(ExposureKind::PublicBind)
    );
    assert_eq!(index.kind("crates/app/src/other.rs"), None);
    assert!(!index.is_empty());
    assert!(ExposureIndex::default().is_empty());
}

/// Two facts about one file collapse to the strongest.
#[test]
fn the_index_keeps_the_strongest_fact_per_file() {
    let facts = vec![
        ExposureFact {
            file: "a.rs".to_string(),
            kind: ExposureKind::LoopbackBind,
            evidence: "x".to_string(),
        },
        ExposureFact {
            file: "a.rs".to_string(),
            kind: ExposureKind::PublicBind,
            evidence: "y".to_string(),
        },
    ];
    let index = ExposureIndex::from_facts(&facts);
    assert_eq!(index.kind("a.rs"), Some(ExposureKind::PublicBind));
}

/// A long bind line is capped, and the cap lands on a char boundary.
#[test]
fn a_long_evidence_line_is_capped() {
    let padding = "é".repeat(400);
    let facts = collect(&[file(
        "src/serve.rs",
        &format!("let l = TcpListener::bind(\"127.0.0.1:1\"); // {padding}\n"),
    )]);
    assert!(facts[0].evidence.len() <= MAX_EVIDENCE + 4, "{facts:?}");
    assert!(facts[0].evidence.ends_with('…'), "{facts:?}");
}
