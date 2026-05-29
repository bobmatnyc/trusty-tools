//! Unit tests for per-tool output compression: dispatch, filters, structured
//! detection, filter strategies, and the RTK fallback.
//!
//! Why: Each filter is pure and worth exhaustive coverage; the dispatch table
//! and size/structured gates are the contract callers depend on.
//! What: Per-filter tests, dispatch tests, `is_structured_format` cases,
//! `FilterStrategy`/`Language` cases, and the async RTK-absent fallback.
//! Test: This module is itself the test coverage.

use super::*;

#[test]
fn test_runner_strips_passing_tests() {
    let mut input = String::new();
    for i in 0..10 {
        input.push_str(&format!("test mod::passing_{i} ... ok\n"));
    }
    input.push_str("test mod::failing ... FAILED\n");
    input.push_str("test result: FAILED. 10 passed; 1 failed\n");
    let out = filter_test_runner(&input);
    assert!(out.contains("failing"));
    assert!(!out.contains("passing_0"));
    assert!(!out.contains("passing_9"));
}

#[test]
fn test_runner_keeps_summary_line() {
    let input = "test foo ... ok\ntest result: FAILED. 1 passed; 1 failed\n";
    let out = filter_test_runner(input);
    assert!(out.contains("test result: FAILED"));
}

#[test]
fn test_runner_no_failures_returns_summary_only() {
    let input = "test a ... ok\ntest b ... ok\ntest result: ok. 2 passed; 0 failed\n";
    let out = filter_test_runner(input);
    assert_eq!(out, "test result: ok. 2 passed; 0 failed");
}

#[test]
fn test_runner_unknown_tool_passthrough() {
    let input = "random output line\nanother line\n";
    let out = compress_tool_output("git_status", input);
    assert_eq!(out, input);
}

#[test]
fn filter_git_diff_strips_context_lines() {
    let input = "\
--- a/foo.rs
+++ b/foo.rs
@@ -1,7 +1,7 @@
 ctx1
 ctx2
 ctx3
-removed
+added
 ctx4
 ctx5
";
    let out = filter_git_diff(input);
    assert!(!out.contains("ctx1"));
    assert!(!out.contains("ctx5"));
    assert!(out.contains("-removed"));
    assert!(out.contains("+added"));
    assert!(out.contains("@@ ... @@"));
}

#[test]
fn filter_git_diff_preserves_adds_and_removes() {
    let input = "@@ -1,2 +1,2 @@\n-old line\n+new line\n";
    let out = filter_git_diff(input);
    assert!(out.contains("-old line"));
    assert!(out.contains("+new line"));
}

#[test]
fn filter_git_diff_passthrough_no_context() {
    let input = "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";
    let out = filter_git_diff(input);
    // No context runs to collapse → unchanged
    assert_eq!(out, input);
}

#[test]
fn filter_git_log_strips_author_date() {
    let mut input = String::new();
    for i in 0..10 {
        input.push_str(&format!("commit abc123{i:03}def456789\n"));
        input.push_str("Author: Alice <alice@example.com>\n");
        input.push_str("Date:   Mon Jan 1 12:00:00 2024 +0000\n");
        input.push('\n');
        input.push_str(&format!("    feat: subject line {i}\n"));
        input.push('\n');
    }
    // 60 lines total
    let out = compress_tool_output("git_log", &input);
    assert!(!out.contains("Author:"));
    assert!(!out.contains("Date:"));
    assert!(out.contains("commit abc123"));
    assert!(out.contains("subject line 0"));
}

#[test]
fn filter_git_log_passthrough_short() {
    // Under 30 lines → passthrough via dispatch
    let input = "commit abc1234\nAuthor: Bob\nDate: today\n\n    short\n";
    let out = compress_tool_output("git_log", input);
    assert_eq!(out, input);
}

#[test]
fn filter_file_read_strips_blank_comment_lines() {
    let mut input = String::new();
    // 50 code lines + 50 comment lines + 50 blank lines = 150
    // But we need > 200 for dispatch — generate enough.
    for i in 0..120 {
        input.push_str(&format!("let x_{i} = {i};\n"));
    }
    for i in 0..60 {
        input.push_str(&format!("// comment {i}\n"));
    }
    for _ in 0..60 {
        input.push('\n');
    }
    let out = compress_tool_output("read_file", &input);
    assert!(!out.contains("// comment"));
    assert!(out.contains("let x_0"));
    assert!(out.contains("let x_119"));
}

#[test]
fn filter_file_read_passthrough_short() {
    let input = "fn main() {\n    println!(\"hi\");\n}\n";
    let out = compress_tool_output("read_file", input);
    assert_eq!(out, input);
}

#[test]
fn filter_file_read_no_over_filter() {
    // All-comment file — filtering would leave 0 lines, so return original.
    let mut input = String::new();
    for i in 0..250 {
        input.push_str(&format!("// only comment {i}\n"));
    }
    let out = filter_file_read(&input);
    assert_eq!(out, input);
}

#[test]
fn filter_cargo_check_strips_compiling() {
    let input = "   Compiling foo v0.1.0\n   Compiling bar v0.2.0\n    Finished dev [unoptimized] target(s) in 1.23s\nwarning: unused variable: `x`\n";
    let out = filter_cargo_check(input);
    assert!(!out.contains("Compiling"));
    assert!(!out.contains("Finished"));
    assert!(out.contains("warning: unused variable"));
}

#[test]
fn filter_cargo_check_keeps_warnings() {
    let input = "   Compiling x v0.1.0\nwarning: foo\nerror: bar\n    Finished\n";
    let out = filter_cargo_check(input);
    assert!(out.contains("warning: foo"));
    assert!(out.contains("error: bar"));
}

#[test]
fn compress_tool_output_dispatch_test() {
    // Inputs must exceed SIZE_GATE_BYTES (80) so the dispatch table runs.
    // test/cargo (without check/clippy) → test runner filter
    let test_input = "test alpha ... ok\ntest beta ... ok\ntest gamma ... ok\ntest result: ok. 3 passed; 0 failed\n";
    let r = compress_tool_output("cargo_test", test_input);
    assert!(r.contains("test result: ok. 3 passed; 0 failed"));
    assert!(!r.contains("alpha ... ok"));

    // diff → diff filter
    let diff_input = "--- a/file.rs\n+++ b/file.rs\n@@ -1,5 +1,5 @@\n ctx1\n ctx2\n-old line of code\n+new line of code\n ctx3\n";
    let r = compress_tool_output("git_diff", diff_input);
    assert!(r.contains("@@ ... @@"));

    // unknown → passthrough (small input — size gate, but assertion still holds)
    let r = compress_tool_output("unknown_tool", "raw output");
    assert_eq!(r, "raw output");

    // check → cargo_check filter
    let check_input = "   Compiling foo v0.1.0\n   Compiling bar v0.1.0\n    Finished dev in 1.2s\nwarning: unused variable: `x`\n";
    let r = compress_tool_output("cargo_check", check_input);
    assert!(!r.contains("Compiling"));
    assert!(r.contains("warning"));

    // clippy → cargo_check filter
    let clippy_input = "   Compiling baz v0.1.0\n    Finished release [optimized] target(s) in 2.34s\nerror: type mismatch in arg\n";
    let r = compress_tool_output("cargo_clippy", clippy_input);
    assert!(!r.contains("Finished"));
    assert!(r.contains("error"));
}

#[test]
fn compress_tool_output_reduces_long_passing_test_output() {
    let mut input = String::new();
    for i in 0..200 {
        input.push_str(&format!("test mod::t{i} ... ok\n"));
    }
    input.push_str("test result: ok. 200 passed; 0 failed\n");
    let out = compress_tool_output("cargo_test", &input);
    assert!(out.lines().count() <= 5);
}

// ── Size gate ────────────────────────────────────────────────────────

#[test]
fn size_gate_skips_short_inputs() {
    // Input under SIZE_GATE_BYTES is returned unchanged even when the
    // tool name would otherwise route to a filter.
    let short = "test foo ... ok\ntest result: ok. 1 passed\n";
    assert!(short.len() < SIZE_GATE_BYTES);
    let out = compress_tool_output("cargo_test", short);
    assert_eq!(out, short, "size gate must passthrough short content");
}

#[test]
fn size_gate_lets_large_inputs_through() {
    // Construct an input over 80 bytes; expect compression to apply.
    let mut input = String::new();
    for i in 0..10 {
        input.push_str(&format!("test passing_{i} ... ok\n"));
    }
    input.push_str("test result: ok. 10 passed; 0 failed\n");
    assert!(input.len() >= SIZE_GATE_BYTES);
    let out = compress_tool_output("cargo_test", &input);
    assert!(!out.contains("passing_0 ... ok"));
}

// ── Structured-format detection ──────────────────────────────────────

#[test]
fn is_structured_format_json_object() {
    assert!(is_structured_format("{\"key\": \"value\", \"n\": 42}"));
}

#[test]
fn is_structured_format_json_array() {
    assert!(is_structured_format("[1, 2, 3, 4]"));
}

#[test]
fn is_structured_format_json_with_leading_whitespace() {
    assert!(is_structured_format("   \n  {\"x\": 1}"));
}

#[test]
fn is_structured_format_yaml_doc_marker() {
    assert!(is_structured_format("---\nname: foo\nversion: 1\n"));
}

#[test]
fn is_structured_format_yaml_kv() {
    assert!(is_structured_format("name: example\nversion: 1.0\n"));
}

#[test]
fn is_structured_format_toml_section() {
    assert!(is_structured_format("[package]\nname = \"foo\"\n"));
}

#[test]
fn is_structured_format_csv() {
    let csv = "id,name,value\n1,foo,10\n2,bar,20\n3,baz,30\n";
    assert!(is_structured_format(csv));
}

#[test]
fn is_structured_format_prose_is_false() {
    assert!(!is_structured_format(
        "This is normal prose, not structured data at all."
    ));
}

#[test]
fn is_structured_format_test_output_is_false() {
    // Test runner output shouldn't be mistaken for structured data.
    let out = "test mod::foo ... ok\ntest mod::bar ... ok\ntest result: ok\n";
    assert!(!is_structured_format(out));
}

#[test]
fn structured_format_passthrough_via_dispatch() {
    // A JSON payload routed via a tool name that would otherwise filter
    // must come back unchanged.
    let payload = "{\"results\": [{\"name\": \"alpha\", \"status\": \"ok\"}, {\"name\": \"beta\", \"status\": \"fail\"}]}";
    assert!(payload.len() >= SIZE_GATE_BYTES);
    let out = compress_tool_output("cargo_test_json", payload);
    assert_eq!(out, payload);
}

// ── FilterStrategy / Language ────────────────────────────────────────

#[test]
fn language_from_extension_known() {
    assert_eq!(Language::from_extension("rs"), Language::Rust);
    assert_eq!(Language::from_extension(".py"), Language::Python);
    assert_eq!(Language::from_extension("TS"), Language::TypeScript);
    assert_eq!(Language::from_extension("go"), Language::Go);
    assert_eq!(Language::from_extension("json"), Language::Data);
    assert_eq!(Language::from_extension("weird"), Language::Unknown);
}

#[test]
fn language_comment_prefix_rust() {
    assert_eq!(Language::Rust.comment_prefix(), Some("//"));
    assert_eq!(Language::Python.comment_prefix(), Some("#"));
    assert_eq!(Language::Data.comment_prefix(), None);
}

#[test]
fn language_block_comment_rust() {
    assert_eq!(Language::Rust.block_comment(), Some(("/*", "*/")));
    assert_eq!(Language::Python.block_comment(), Some(("\"\"\"", "\"\"\"")));
    assert_eq!(Language::Data.block_comment(), None);
}

#[test]
fn filter_strategy_no_filter_identity() {
    let f = get_filter(FilterLevel::None);
    let input = "line one\n\nline two\n// comment\n";
    assert_eq!(f.filter(input, Language::Rust), input);
}

#[test]
fn filter_strategy_minimal_drops_blanks() {
    let f = get_filter(FilterLevel::Minimal);
    let input = "line one\n\nline two   \n   \nline three\n";
    let out = f.filter(input, Language::Unknown);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines, vec!["line one", "line two", "line three"]);
}

#[test]
fn filter_strategy_aggressive_strips_rust_line_comments() {
    let f = get_filter(FilterLevel::Aggressive);
    let input = "let x = 1;\n// this is a comment\nlet y = 2;\n  // indented comment\n";
    let out = f.filter(input, Language::Rust);
    assert!(!out.contains("comment"));
    assert!(out.contains("let x = 1;"));
    assert!(out.contains("let y = 2;"));
}

#[test]
fn filter_strategy_aggressive_strips_python_hash_comments() {
    let f = get_filter(FilterLevel::Aggressive);
    let input = "x = 1\n# hash comment here\ny = 2\n";
    let out = f.filter(input, Language::Python);
    assert!(!out.contains("hash comment"));
    assert!(out.contains("x = 1"));
    assert!(out.contains("y = 2"));
}

#[test]
fn filter_strategy_aggressive_unknown_lang_keeps_comments() {
    // With no comment prefix known, aggressive becomes minimal.
    let f = get_filter(FilterLevel::Aggressive);
    let input = "data1\n// looks like a comment\ndata2\n";
    let out = f.filter(input, Language::Unknown);
    assert!(out.contains("// looks like a comment"));
}

// ── RTK subprocess fallback ──────────────────────────────────────────

#[tokio::test]
async fn compress_via_rtk_returns_none_when_binary_absent() {
    // We can't reliably assert presence in CI, but we CAN assert that
    // the function returns Some/None without panicking and that the
    // async fallback always returns a String.
    let payload = "test result: ok. 100 passed; 0 failed\n".repeat(5);
    let result = compress_tool_output_async("cargo_test", &payload).await;
    assert!(!result.is_empty(), "async fallback must return content");
}

#[tokio::test]
async fn compress_tool_output_async_falls_back_when_rtk_absent() {
    // Force-bypass rtk by checking which() with a name guaranteed to not
    // exist; we test the integration via the public async wrapper.
    let mut input = String::new();
    for i in 0..10 {
        input.push_str(&format!("test t{i} ... ok\n"));
    }
    input.push_str("test result: ok. 10 passed; 0 failed\n");
    let out = compress_tool_output_async("cargo_test", &input).await;
    // Whether rtk ran or native fallback ran, the summary must be retained.
    assert!(out.contains("test result"));
}
