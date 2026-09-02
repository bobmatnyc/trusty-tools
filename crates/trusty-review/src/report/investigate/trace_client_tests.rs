//! Tests for the trusty-search reads the trace pass makes (#6166).
//!
//! Why: the entry-node parse is the only thing standing between a `text/plain`
//! report and a structured anchor, and the 404 taxonomy is what keeps "the
//! index is not registered" from being reported as "the symbol does not exist".
//! What: the parse (with and without an entry header, and against the report
//! shape the live daemon emits), the 404 split, and the byte bound.
//! Test: included as `#[cfg(test)] mod tests` from `trace_client.rs`.

use super::*;

/// Verbatim from `GET /indexes/trusty-tools-2c24d89f/call_chain?entry_point=
/// SHRINK_GUARD_RATIO_DIVISOR` on 2026-08-22 — the report the RED finding at
/// `usearch_store.rs:169` anchors on.
const LIVE_REPORT: &str = "# Call chain: SHRINK_GUARD_RATIO_DIVISOR\n\
# Index: trusty-tools-2c24d89f  Direction: outgoing  Depth: 1\n\
\n\
═══════════════════════════════════════\n\
\n\
## `SHRINK_GUARD_RATIO_DIVISOR` [ENTRY]  crates/trusty-search/src/core/store/usearch_store.rs:181\n\
Signature: pub(super) const SHRINK_GUARD_RATIO_DIVISOR: usize = 2;\n\
Why: (no doc)\n\
What: (no doc)\n\
\n\
Calls →\n\
  (none discovered)\n\
\n\
───────────────────────────────────────\n";

#[test]
fn the_entry_node_parses_out_of_a_call_chain_report() {
    let got = parse_entry_node(LIVE_REPORT).expect("entry node");
    assert_eq!(got.symbol, "SHRINK_GUARD_RATIO_DIVISOR");
    assert_eq!(
        got.file,
        "crates/trusty-search/src/core/store/usearch_store.rs"
    );
    assert_eq!(got.line, 181);
    assert_eq!(
        got.signature,
        "pub(super) const SHRINK_GUARD_RATIO_DIVISOR: usize = 2;"
    );
}

/// Every edge below the entry node is dropped — #6167 is why. A parse that
/// started collecting them would be the leg-2 change, not a refactor.
#[test]
fn callee_lines_are_not_read_as_part_of_the_entry_node() {
    let report =
        format!("{LIVE_REPORT}  · write  crates/trusty-agents/src/x.rs::Function::write:\n");
    let got = parse_entry_node(&report).expect("entry node");
    assert_eq!(
        got.file,
        "crates/trusty-search/src/core/store/usearch_store.rs"
    );
}

#[test]
fn a_report_with_no_entry_header_parses_to_nothing() {
    assert!(parse_entry_node("# Call chain: x\n\nnothing here\n").is_none());
}

/// The daemon answers 404 for an unregistered index AND for an unresolvable
/// entry point; only the body separates them.
#[test]
fn the_two_404_bodies_are_told_apart() {
    assert_eq!(
        classify(
            404,
            r#"{"error":"unknown index: no-such"}"#,
            "no-such",
            "Foo"
        ),
        TraceError::IndexAbsent("no-such".to_string())
    );
    assert_eq!(
        classify(
            404,
            r#"{"error":"entry point not found: MAX_ALLOC_PROBES"}"#,
            "idx",
            "MAX_ALLOC_PROBES"
        ),
        TraceError::SymbolAbsent("MAX_ALLOC_PROBES".to_string())
    );
    assert_eq!(
        classify(503, r#"{"error":"kg_unavailable"}"#, "idx", "Foo"),
        TraceError::Api {
            status: 503,
            body: r#"{"error":"kg_unavailable"}"#.to_string(),
        }
    );
}

#[test]
fn a_long_body_is_bounded_on_a_char_boundary() {
    let long = "é".repeat(500);
    let got = bound(&long, MAX_ERROR_BODY);
    assert!(
        got.len() <= MAX_ERROR_BODY + '…'.len_utf8(),
        "{}",
        got.len()
    );
    assert!(got.ends_with('…'));
    assert_eq!(bound("short", MAX_ERROR_BODY), "short");
}

#[test]
fn an_explicit_base_url_loses_its_trailing_slash() {
    let source = HttpTraceSource::at("http://127.0.0.1:9999/").expect("client builds");
    assert_eq!(source.base_url(), "http://127.0.0.1:9999");
}

/// #6677 review: the registry read is THIS source's, not the machine's.
///
/// Why: the sibling read on `HttpAnalyzeMetricsSource` resolved the advertised
/// daemon whatever socket the source held, which made a stubbed suite issue a
/// live GET to the developer's own trusty-search. `HttpTraceSource` reads
/// `self.base_url` instead, and this pins that: a source pointed at a dead port
/// reads an empty registry, where a machine-wide resolve would return whatever
/// the host daemon holds.
/// What: binds a port, drops it, and asserts the read from that address is
/// empty. On a machine with no daemon the assertion is vacuous; on one with a
/// daemon it is the leak check.
#[tokio::test]
async fn the_registry_read_uses_this_sources_base_url() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);

    let source = HttpTraceSource::at(format!("http://127.0.0.1:{port}")).expect("client builds");
    assert!(
        source.registered_indexes().await.is_empty(),
        "the read must go to this source's base_url, never the machine's daemon"
    );
}

/// The layout the source resolves through, pinned so a later edit cannot
/// quietly swap it for a literal `127.0.0.1:7878` — which would miss an
/// auto-ported daemon and every `TRUSTY_DATA_DIR`-isolated one. Nothing here
/// calls `resolve_base_url`: it probes the network and can refresh a file under
/// `$HOME`, neither of which belongs in a unit test.
#[test]
fn the_shared_search_layout_is_the_one_being_resolved() {
    assert_eq!(DaemonAddrLayout::TRUSTY_SEARCH.addr_file_name, "http_addr");
    assert_eq!(DaemonAddrLayout::TRUSTY_SEARCH.default_port, 7878);
}
