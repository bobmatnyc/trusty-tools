//! Unit tests for `memory`/`code` search arg-parsing and result formatting.
//!
//! Why: Parsing must stay backward-compatible across the clap migration, and
//! the formatters' human/JSON output is a stable contract for tooling.
//! What: `parse_args` cases for each subcommand form + formatter assertions.
//! Test: This module is itself the test coverage.

use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use serde_json::json;

use super::format::{
    format_code_results, format_memory_results, format_session_list, format_sessions, preview_text,
};
use super::{Command, parse_args};
use crate::memory::{AgentSession, MemoryResult, SessionMeta};
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
fn format_memory_results_human() {
    let results = vec![MemoryResult {
        id: "sess-1".to_string(),
        score: 0.87,
        segment: "mem".to_string(),
        payload: json!({
            "agent_name": "python-engineer",
            "phase": "code",
            "timestamp": "2026-04-22T10:30:00Z",
            "prompt": "write a hello world",
            "response": "print('hello')"
        }),
    }];
    let out = format_memory_results(&results, false).unwrap();
    assert!(out.contains("Agent"));
    assert!(out.contains("Phase"));
    assert!(out.contains("Score"));
    assert!(out.contains("python-engineer"));
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
fn format_sessions_human_includes_timestamp_and_preview() {
    let sessions = vec![AgentSession {
        id: "s1".to_string(),
        agent_name: "pm".to_string(),
        workflow_run_id: "run-1".to_string(),
        phase: "plan".to_string(),
        prompt: "plan the work".to_string(),
        response: "ok".to_string(),
        timestamp: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        parent_id: None,
        segment: None,
    }];
    let out = format_sessions(&sessions, false).unwrap();
    assert!(out.contains("pm"));
    assert!(out.contains("(plan)"));
    assert!(out.contains("plan the work"));
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
fn format_session_list_human_lists_runs() {
    let s = vec![SessionMeta {
        run_id: "abcdef1234567890".to_string(),
        started_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        task_preview: "hello world task".to_string(),
    }];
    let out = format_session_list(&s, false).unwrap();
    assert!(out.contains("Run"));
    assert!(out.contains("abcdef12"));
    assert!(out.contains("hello world task"));
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
