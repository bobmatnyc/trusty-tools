//! Unit tests for `memory`/`code` search arg-parsing and result formatting.
//!
//! Why: Parsing must stay backward-compatible across the clap migration, and
//! the formatters' human/JSON output is a stable contract for tooling.
//! What: `parse_args` cases for each subcommand form + formatter assertions.
//! Test: This module is itself the test coverage.

use std::path::PathBuf;

use super::format::{format_code_results, preview_text};
use super::{Command, parse_args};
use crate::search::CodeChunk;

#[test]
fn parse_memory_search_args() {
    let cmd = parse_args(&["memory", "search", "hello", "--top-k", "3"]).unwrap();
    assert_eq!(
        cmd,
        Command::MemorySearch {
            query: "hello".to_string(),
            top_k: 3,
            json: false,
        }
    );
}

#[test]
fn parse_code_search_with_lang_filter() {
    let cmd = parse_args(&["code", "search", "fn main", "--lang", "rust", "--json"]).unwrap();
    assert_eq!(
        cmd,
        Command::CodeSearch {
            query: "fn main".to_string(),
            top_k: 5,
            lang: Some("rust".to_string()),
            json: true,
        }
    );
}

#[test]
fn parse_memory_run() {
    let cmd = parse_args(&["memory", "run", "run-abc-123"]).unwrap();
    assert_eq!(
        cmd,
        Command::MemoryRun {
            run_id: "run-abc-123".to_string(),
            json: false,
        }
    );
}

#[test]
fn format_code_results_json() {
    let chunks = vec![CodeChunk {
        file: PathBuf::from("/tmp/foo.rs"),
        function_name: Some("main".to_string()),
        start_line: 1,
        end_line: 3,
        language: "rust".to_string(),
        score: 0.9,
        text: "fn main() {}".to_string(),
        match_reason: "hybrid".to_string(),
    }];
    let out = format_code_results(&chunks, true).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(parsed.is_array());
    assert_eq!(parsed[0]["function_name"], "main");
}

#[test]
fn preview_text_handles_newlines_and_truncation() {
    assert_eq!(preview_text("hi\nthere", 80), "hi there");
    let long = "x".repeat(200);
    assert_eq!(preview_text(&long, 10).chars().count(), 10);
}

#[test]
fn parse_rejects_unknown_command() {
    assert!(parse_args(&["foo", "bar", "baz"]).is_err());
}

#[test]
fn parse_rejects_missing_positional() {
    assert!(parse_args(&["memory", "search"]).is_err());
}

#[test]
fn parse_memory_sessions() {
    let cmd = parse_args(&["memory", "sessions"]).unwrap();
    assert_eq!(cmd, Command::MemorySessions { json: false });
    let cmd = parse_args(&["memory", "sessions", "--json"]).unwrap();
    assert_eq!(cmd, Command::MemorySessions { json: true });
}

#[test]
fn parse_memory_search_all() {
    let cmd = parse_args(&["memory", "search-all", "hello", "--top-k", "7"]).unwrap();
    assert_eq!(
        cmd,
        Command::MemorySearchAll {
            query: "hello".to_string(),
            top_k: 7,
            json: false,
        }
    );
}

#[test]
fn parse_handles_json_flag_before_query() {
    let cmd = parse_args(&["memory", "search", "--json", "hello"]).unwrap();
    assert_eq!(
        cmd,
        Command::MemorySearch {
            query: "hello".to_string(),
            top_k: 5,
            json: true,
        }
    );
}

#[tokio::test]
async fn legacy_memory_commands_fail_before_store_access() {
    for args in [
        vec!["memory", "search", "synthetic"],
        vec!["memory", "run", "missing"],
        vec!["memory", "sessions"],
        vec!["memory", "search-all", "synthetic"],
    ] {
        let error =
            super::run_search_command(&args.into_iter().map(String::from).collect::<Vec<_>>())
                .await
                .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Local memory commands are retired")
        );
        assert!(
            error
                .to_string()
                .contains("Existing local files are preserved")
        );
    }
}
