//! Tests for the #11625 search verbs: clap parsing, the params each verb
//! sends, and the daemon-down error.

use super::*;
use crate::commands::mock_socket::mock_daemon;
use crate::commands::search_verbs_render::lines;
use clap::Parser;

/// A stand-in for `Cli` carrying the same global `-i` and `-v` flags, so a
/// short-flag clash with them fails here as it would in `main.rs`.
#[derive(Parser)]
struct Wrap {
    #[arg(short = 'i', long, global = true)]
    index: Option<String>,
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    verb: SearchVerb,
}

fn parse(args: &[&str]) -> SearchVerb {
    let mut argv = vec!["trusty-search"];
    argv.extend_from_slice(args);
    Wrap::try_parse_from(argv).expect("parses").verb
}

#[test]
fn the_wrapper_command_is_well_formed() {
    use clap::CommandFactory;
    Wrap::command().debug_assert();
}

#[test]
fn similar_parses() {
    let v = parse(&["similar", "src/a.rs", "--function", "go", "-k", "3"]);
    assert_eq!(
        v,
        SearchVerb::Similar {
            file: "src/a.rs".into(),
            function: Some("go".into()),
            top_k: Some(3)
        }
    );
}

#[test]
fn call_chain_parses() {
    let v = parse(&[
        "call-chain",
        "main",
        "--direction",
        "callers",
        "--max-depth",
        "2",
        "--no-source",
    ]);
    assert_eq!(
        v,
        SearchVerb::CallChain {
            entry_point: "main".into(),
            direction: Some(Direction::Callers),
            max_depth: Some(2),
            no_source: true
        }
    );
}

#[test]
fn grep_parses_with_ripgrep_short_flags() {
    let v = parse(&[
        "grep",
        "fn",
        "--all",
        "-F",
        "-w",
        "-l",
        "-C",
        "2",
        "--glob",
        "*.rs",
        "--max-count",
        "7",
        "--ignore-case",
        "--invert-match",
    ]);
    let SearchVerb::Grep(g) = v else {
        panic!("expected grep")
    };
    assert!(g.all && g.fixed_strings && g.word_regexp && g.files_with_matches);
    assert!(g.ignore_case && g.invert_match && !g.multiline);
    assert_eq!((g.context, g.max_results), (Some(2), Some(7)));
    assert_eq!(g.glob.as_deref(), Some("*.rs"));
}

#[test]
fn typeahead_parses() {
    let v = parse(&["typeahead", "rrf", "--limit", "4", "--mode", "blended"]);
    assert_eq!(
        v,
        SearchVerb::Typeahead {
            query: "rrf".into(),
            limit: Some(4),
            mode: Some(TypeaheadMode::Blended)
        }
    );
}

/// Each verb, sent to a mock daemon, reaches the method and params the
/// matching MCP tool sends (`mcp/tools/{search,misc}.rs`).
#[tokio::test]
async fn each_verb_sends_its_method_and_params() {
    let cases = [
        (
            parse(&["similar", "src/a.rs", "--function", "go", "-k", "3"]),
            METHOD_SIMILAR,
            json!({"index_id": "demo", "body": {"file": "src/a.rs", "function": "go", "top_k": 3}}),
        ),
        (
            parse(&["similar", "src/a.rs"]),
            METHOD_SIMILAR,
            json!({"index_id": "demo", "body": {"file": "src/a.rs"}}),
        ),
        (
            parse(&["call-chain", "main", "--max-depth", "2", "--no-source"]),
            METHOD_CALL_CHAIN,
            json!({"index_id": "demo", "entry_point": "main", "max_depth": 2, "include_source": false}),
        ),
        (
            parse(&["grep", "fn"]),
            METHOD_GREP,
            json!({"index_id": "demo", "body": {"pattern": "fn"}}),
        ),
        (
            parse(&["grep", "fn", "--all"]),
            METHOD_GREP_ALL,
            json!({"pattern": "fn"}),
        ),
        (
            parse(&["typeahead", "ma", "--limit", "3", "--mode", "blended"]),
            METHOD_TYPEAHEAD,
            json!({"index_id": "demo", "q": "ma", "limit": 3, "mode": "blended"}),
        ),
    ];
    for (verb, method, params) in cases {
        let (want_m, want_p) = (method.to_string(), params.clone());
        let daemon = mock_daemon(move |m, p| {
            assert_eq!(m, want_m);
            assert_eq!(p, want_p);
            Ok(json!({"ok": true}))
        })
        .await;
        let body = run(&daemon.client, &verb, "demo").await.expect("answered");
        assert_eq!(body, json!({"ok": true}), "{method}");
    }
}

#[test]
fn grep_sends_every_switch_the_mcp_tool_sends() {
    let v = parse(&[
        "grep",
        "x",
        "--ignore-case",
        "-F",
        "-w",
        "--invert-match",
        "-l",
        "--multiline",
        "-C",
        "1",
        "-B",
        "2",
        "-A",
        "3",
        "--glob",
        "*.rs",
        "--max-results",
        "9",
    ]);
    let (_, p) = v.request("demo");
    assert_eq!(
        p["body"],
        json!({
            "pattern": "x", "case_insensitive": true, "fixed_strings": true,
            "word_regexp": true, "invert_match": true, "files_with_matches": true,
            "multiline": true, "context": 1, "context_before": 2, "context_after": 3,
            "glob": "*.rs", "max_results": 9
        })
    );
}

/// No daemon on the socket: a clear error naming the socket. `main()` turns
/// the `Err` into exit code 1.
#[tokio::test]
async fn a_verb_names_the_socket_when_no_daemon_answers() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let socket = dir.path().join("absent.sock");
    let client = DaemonClient::at(&socket);
    let err = run(&client, &parse(&["typeahead", "ma"]), "demo")
        .await
        .expect_err("no daemon answers")
        .to_string();
    assert!(err.starts_with("could not reach daemon"), "{err}");
    assert!(err.contains(&socket.display().to_string()), "{err}");
}

#[test]
fn render_lines_cover_each_verb_shape() {
    let grep =
        json!({"matches": [{"file": "a.rs", "line": 3, "text": "fn x"}], "truncated": false});
    assert_eq!(
        lines(&parse(&["grep", "x"]), &grep),
        Some(vec!["a.rs:3:fn x".into()])
    );
    // #11625: `-A`/`-B`/`-C` context prints ripgrep-style `file-N-text` lines.
    let ctx = json!({"matches": [{"file": "a.rs", "line": 3, "text": "fn x",
        "context_before": ["// a", "// b"], "context_after": ["}"]}], "truncated": false});
    assert_eq!(
        lines(&parse(&["grep", "x", "-C", "2"]), &ctx),
        Some(vec![
            "a.rs-1-// a".into(),
            "a.rs-2-// b".into(),
            "a.rs:3:fn x".into(),
            "a.rs-4-}".into(),
        ])
    );
    let hits = json!({"hits": [{"label": "go", "path": "a.rs", "start_line": 1, "source": "lexical", "score": 1.0}]});
    assert_eq!(
        lines(&parse(&["typeahead", "g"]), &hits),
        Some(vec!["go  a.rs:1  (lexical, 1.000)".into()])
    );
    let sim = json!({"results": [{"file": "a.rs", "start_line": 1, "end_line": 4, "score": 0.5}]});
    assert_eq!(
        lines(&parse(&["similar", "a.rs"]), &sim),
        Some(vec!["a.rs:1-4  (score: 0.500)".into()])
    );
    assert_eq!(
        lines(&parse(&["call-chain", "m"]), &json!("tree")),
        Some(vec!["tree".into()])
    );
    assert_eq!(lines(&parse(&["grep", "x"]), &json!({})), None);
}
