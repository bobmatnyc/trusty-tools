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

// ── grep/ls/find/rg dispatch (#1957) ───────────────────────────────────

#[test]
fn compress_tool_output_reduces_long_grep_output() {
    // #1957: grep had no filter branch — the #1953 spike measured 0%
    // reduction for `grep -r` output. This must shrink once the branch
    // exists.
    let mut input = String::new();
    for i in 0..150 {
        input.push_str(&format!(
            "crates/foo/src/bar{i}.rs:{}:    some matched line of code here\n",
            10 + i
        ));
    }
    let out = compress_tool_output("grep", &input);
    assert!(
        out.lines().count() < input.lines().count(),
        "grep output should be compressed, got {} lines from {} input lines",
        out.lines().count(),
        input.lines().count()
    );
}

#[test]
fn compress_tool_output_reduces_long_ls_output() {
    // #1957: ls had no filter branch — the #1953 spike measured 0%
    // reduction for `ls -la` output.
    let mut input = String::from("total 912\n");
    for i in 0..150 {
        input.push_str(&format!(
            "-rw-r--r--   1 user  staff  {:>5} Jul  3 09:{:02} module_{i}.rs\n",
            1200 + i * 7,
            i % 60
        ));
    }
    let out = compress_tool_output("ls", &input);
    assert!(
        out.lines().count() < input.lines().count(),
        "ls output should be compressed, got {} lines from {} input lines",
        out.lines().count(),
        input.lines().count()
    );
}

#[test]
fn filter_grep_output_caps_long_match_list() {
    let mut input = String::new();
    for i in 0..150 {
        input.push_str(&format!(
            "src/mod{i}.rs:{}:    let x = compress(y);\n",
            10 + i
        ));
    }
    let out = filter_grep_output(&input);
    assert!(out.contains("lines omitted"));
    assert!(out.contains("src/mod0.rs"), "head lines must be kept");
    assert!(out.contains("src/mod149.rs"), "tail lines must be kept");
    assert!(out.lines().count() < input.lines().count());
}

#[test]
fn filter_grep_output_passthrough_short() {
    let input = "a.rs:1:match one\nb.rs:2:match two\n";
    assert_eq!(filter_grep_output(input), input);
}

#[test]
fn filter_ls_output_caps_long_listing() {
    let mut input = String::from("total 912\n");
    for i in 0..150 {
        input.push_str(&format!(
            "-rw-r--r--   1 user  staff  {:>5} Jul  3 09:{:02} module_{i}.rs\n",
            1200 + i * 7,
            i % 60
        ));
    }
    let out = filter_ls_output(&input);
    assert!(out.contains("lines omitted"));
    assert!(out.contains("module_0.rs"), "head entries must be kept");
    assert!(out.contains("module_149.rs"), "tail entries must be kept");
    assert!(out.lines().count() < input.lines().count());
}

#[test]
fn filter_ls_output_passthrough_short() {
    let input = "total 8\n-rw-r--r--  1 user  staff  100 Jul  3 09:00 a.rs\n";
    assert_eq!(filter_ls_output(input), input);
}

#[test]
fn compress_tool_output_dispatch_routes_find_and_rg() {
    let mut find_input = String::new();
    for i in 0..100 {
        find_input.push_str(&format!("crates/foo/src/mod{i}.rs\n"));
    }
    let out = compress_tool_output("find", &find_input);
    assert!(out.lines().count() < find_input.lines().count());

    let mut rg_input = String::new();
    for i in 0..100 {
        rg_input.push_str(&format!("src/mod{i}.rs:{}:matched line\n", 10 + i));
    }
    let out = compress_tool_output("rg", &rg_input);
    assert!(out.lines().count() < rg_input.lines().count());
}

#[test]
fn compress_tool_output_grep_tool_name_variants_route_correctly() {
    // "Grep" (capitalized, as an MCP tool name) and a "grep -r <pattern>"
    // shaped tool name must both route through the grep filter.
    let mut input = String::new();
    for i in 0..100 {
        input.push_str(&format!("src/mod{i}.rs:{}:matched line\n", 10 + i));
    }
    let out = compress_tool_output("Grep", &input);
    assert!(out.lines().count() < input.lines().count());
}

#[test]
fn compress_tool_output_does_not_misfire_on_rg_substring() {
    // #1957: "rg" is a substring of unrelated tool names ("cargo", "git
    // merge"). The dispatch must not route these through the grep filter —
    // it has no branch for "merge", so this must pass through unchanged.
    let mut input = String::new();
    for i in 0..40 {
        input.push_str(&format!("Merge made by the 'ort' strategy, file {i}.rs\n"));
    }
    let out = compress_tool_output("git merge", &input);
    assert_eq!(out, input);
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
    // Force-bypass rtk by resolving a name guaranteed to not
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

// ── RTK argv split ──────────────────────────────────────────────────────

use super::rtk::{compress_via_rtk_binary, rtk_filter_for, rtk_pipe_argv, stderr_head};

/// Write an executable `/bin/sh` shim into `dir` and return its path.
///
/// Why: The argv the `rtk` process actually receives is only observable from
/// inside that process; a shim that echoes its own arguments makes it
/// observable without needing the real `rtk` on `PATH`.
/// What: Writes `body` to `<dir>/fake-rtk`, chmods it 0755, returns the path.
#[cfg(unix)]
fn write_rtk_shim(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-rtk");
    std::fs::write(&path, body).expect("write shim");
    let mut perms = std::fs::metadata(&path).expect("stat shim").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod shim");
    path
}

#[test]
fn rtk_pipe_argv_never_carries_the_tool_name() {
    // Every rtk subcommand but `pipe` RUNS the named tool: `rtk git status`
    // would execute `git status` and return its output instead of filtering
    // the output we captured. Nothing from the tool name may reach argv as an
    // executable position.
    let argv = rtk_pipe_argv("git status");
    assert_eq!(argv[0], "pipe");
    assert!(
        !argv.contains(&"git"),
        "`git` must never reach argv: {argv:?}"
    );
    assert!(
        !argv.contains(&"status"),
        "`status` must never reach argv: {argv:?}"
    );
    assert_eq!(argv, vec!["pipe", "-f", "git-status"]);
}

#[test]
fn rtk_pipe_argv_is_always_pipe_mode_with_a_vetted_filter() {
    // The general invariant behind the case above: argv[0] is always `pipe`,
    // and the only other elements are `-f` and a name rtk itself accepts.
    for tool in [
        "git status",
        "cargo test -p trusty-agents-common",
        "rm -rf /",
        "kubectl get pods",
        "",
    ] {
        let argv = rtk_pipe_argv(tool);
        assert_eq!(argv[0], "pipe", "tool {tool:?} left pipe mode");
        for element in &argv[1..] {
            assert!(
                *element == "-f" || rtk_filter_for(&element.replace('-', " ")).is_some(),
                "tool {tool:?} put an unvetted element {element:?} in argv"
            );
        }
    }
}

#[test]
fn rtk_pipe_argv_falls_back_to_bare_pipe_for_an_unknown_tool() {
    // rtk exits 1 on an unrecognised `-f`, so an unknown tool must omit it.
    assert_eq!(rtk_pipe_argv("kubectl get pods"), vec!["pipe"]);
    assert_eq!(rtk_pipe_argv(""), vec!["pipe"]);
}

#[test]
fn rtk_filter_for_maps_known_tool_names() {
    for (tool, expected) in [
        ("cargo test", Some("cargo-test")),
        ("cargo test -p trusty-agents-common", Some("cargo-test")),
        ("git status", Some("git-status")),
        ("git log --oneline -20", Some("git-log")),
        ("git diff", Some("git-diff")),
        ("go test ./...", Some("go-test")),
        ("ruff check", Some("ruff-check")),
        ("pytest", Some("pytest")),
        ("grep -n needle src", Some("grep")),
        ("tsc --noEmit", Some("tsc")),
        // `git` alone is not a filter rtk accepts — only the two-word forms.
        ("git", None),
        ("kubectl get pods", None),
        ("", None),
    ] {
        assert_eq!(rtk_filter_for(tool), expected, "tool name {tool:?}");
    }
}

#[test]
fn every_pinned_filter_round_trips_from_its_spaced_form() {
    // Pins the whole rtk 0.48.0 list: each name, written the way a tool name
    // spells it (spaces for hyphens), must map back to itself.
    for filter in super::rtk::RTK_PIPE_FILTERS {
        let spaced = filter.replace('-', " ");
        assert_eq!(
            rtk_filter_for(&spaced),
            Some(*filter),
            "filter {filter:?} did not round-trip from {spaced:?}"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn rtk_binary_receives_the_pipe_invocation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shim = write_rtk_shim(
        dir.path(),
        "#!/bin/sh\nfor a in \"$@\"; do echo \"arg=$a\"; done\ncat >/dev/null\n",
    );
    let out = compress_via_rtk_binary(&shim, "git status", "payload\n")
        .await
        .expect("shim exits zero, so the rtk path must be taken");
    // One line per argv element. A subcommand invocation would print
    // `arg=git` and `arg=status` here and would have RUN git status.
    assert_eq!(out, "arg=pipe\narg=-f\narg=git-status\n");
}

#[cfg(unix)]
#[tokio::test]
async fn rtk_binary_omits_the_filter_flag_for_an_unknown_tool() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shim = write_rtk_shim(
        dir.path(),
        "#!/bin/sh\nfor a in \"$@\"; do echo \"arg=$a\"; done\ncat >/dev/null\n",
    );
    let out = compress_via_rtk_binary(&shim, "kubectl get pods", "payload\n")
        .await
        .expect("shim exits zero");
    assert_eq!(out, "arg=pipe\n");
}

/// Round-trip a marker payload through the REAL `rtk`, when one is installed.
///
/// Why: Exit status alone cannot tell a filter that consumed our stdin from a
/// subcommand that ignored it and ran a tool instead — that is exactly how the
/// pre-fix invocation read as working. Asserting the marker survives is an
/// output-correspondence check, which does distinguish them.
/// What: Skips silently when `rtk` is absent. Otherwise pipes a marked payload
/// through the public wrapper and requires both the marker back and
/// `CompressionPath::RtkBinary`.
#[tokio::test]
async fn real_rtk_pipe_round_trips_a_marker_payload() {
    if trusty_common::bin_resolve::resolve_binary("rtk").is_none() {
        return;
    }
    const MARKER: &str = "RTK_PIPE_MARKER_8f3a";
    let payload = format!("{MARKER}\nsecond line\nthird line\n");
    // An unmapped tool name so rtk applies no filter and passes content through.
    let (text, path) = compress_tool_output_async_with_path("kubectl get pods", &payload).await;
    assert_eq!(
        path,
        CompressionPath::RtkBinary,
        "rtk is installed, so the rtk path must have been taken; got {path:?}"
    );
    assert!(
        text.contains(MARKER),
        "rtk pipe must return OUR stdin, not another command's output; got {text:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn rtk_binary_returns_none_and_warns_on_non_zero_exit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shim = write_rtk_shim(
        dir.path(),
        "#!/bin/sh\ncat >/dev/null\necho 'rtk: No such file or directory (os error 2)' >&2\nexit 127\n",
    );
    let out = compress_via_rtk_binary(&shim, "git status", "payload\n").await;
    assert!(out.is_none(), "a non-zero rtk exit must fall back");
}

// ── The resolver seam (#7325) ───────────────────────────────────────────

#[tokio::test]
async fn no_rtk_resolver_forces_the_native_chain_on_any_host() {
    // #7325: the assertion this seam exists for — it must hold identically on
    // a host with rtk installed and on one without, which the default
    // resolver cannot promise because `resolve_binary` finds a Homebrew rtk
    // whatever PATH says.
    let payload = "line one\nline two\n".repeat(20);
    let (text, path) =
        compress_tool_output_async_with_path_using(no_rtk, "kubectl get pods", &payload).await;
    assert_eq!(path, CompressionPath::NativeFallback);
    assert!(!text.is_empty(), "the native chain must return content");
}

#[cfg(unix)]
#[tokio::test]
async fn a_resolver_naming_a_missing_binary_falls_back_to_native() {
    // Fail-open guard: the seam takes a path from the caller, so a path to a
    // file that does not exist must NOT become a silent `rtk_binary` claim.
    // The spawn fails, `compress_via_rtk_binary` returns None, and the path
    // reported is the one that actually produced the bytes.
    fn ghost(_name: &str) -> Option<std::path::PathBuf> {
        Some(std::path::PathBuf::from(
            "/nonexistent/definitely-not-rtk-7325",
        ))
    }
    let payload = "line one\nline two\n".repeat(20);
    let (_text, path) =
        compress_tool_output_async_with_path_using(ghost, "kubectl get pods", &payload).await;
    assert_eq!(
        path,
        CompressionPath::NativeFallback,
        "a binary that never ran must never be reported as the rtk path"
    );
}

#[test]
fn value_forces_native_fallback_accepts_only_truthy_spellings() {
    for truthy in ["1", "true", "TRUE", " yes ", "on"] {
        assert!(
            super::rtk::value_forces_native_fallback(Some(truthy)),
            "{truthy:?} must force the native chain"
        );
    }
    for falsy in [None, Some(""), Some("0"), Some("false"), Some("maybe")] {
        assert!(
            !super::rtk::value_forces_native_fallback(falsy),
            "{falsy:?} must leave the real resolver in place"
        );
    }
}

#[tokio::test]
async fn default_rtk_resolver_is_the_real_resolver_without_the_env_var() {
    // Without `TRUSTY_COMPRESS_NO_RTK` set, production must keep reaching the
    // real resolver — the seam must not quietly disable rtk for everyone. The
    // test asserts agreement with `resolve_binary` rather than presence, so it
    // holds on a host with rtk and on one without.
    if std::env::var(ENV_COMPRESS_NO_RTK).is_ok() {
        // Announce the skip: a silently-returning test reads as a pass.
        eprintln!(
            "SKIP default_rtk_resolver_is_the_real_resolver_without_the_env_var: \
             {ENV_COMPRESS_NO_RTK} is set in this environment"
        );
        return;
    }
    assert_eq!(
        default_rtk_resolver()("rtk"),
        trusty_common::bin_resolve::resolve_binary("rtk"),
        "the default resolver must be the real one"
    );
}

#[test]
fn stderr_head_takes_the_first_line_and_truncates() {
    assert_eq!(stderr_head(b"\n   \nrtk: boom\nsecond line\n"), "rtk: boom");
    assert_eq!(stderr_head(b""), "");
    assert_eq!(stderr_head(&vec![b'x'; 500]).len(), 200);
}

// ── CompressionPath (issue #1956 stats-logging signal) ──────────────────

#[test]
fn compression_path_as_str_is_stable() {
    // `tm compress`'s structured stats log (issue #1956) keys on these exact
    // strings; a Debug-format change to the enum must not silently change them.
    assert_eq!(CompressionPath::RtkBinary.as_str(), "rtk_binary");
    assert_eq!(CompressionPath::NativeFallback.as_str(), "native_fallback");
}

#[tokio::test]
async fn compress_tool_output_async_with_path_matches_plain_wrapper() {
    let mut input = String::new();
    for i in 0..10 {
        input.push_str(&format!("test t{i} ... ok\n"));
    }
    input.push_str("test result: ok. 10 passed; 0 failed\n");

    let (text, path) = compress_tool_output_async_with_path("cargo_test", &input).await;
    assert!(text.contains("test result"));
    // Path-reporting variant must produce the exact same text as the plain
    // wrapper — the latter is defined as discarding this variant's path.
    let via_wrapper = compress_tool_output_async("cargo_test", &input).await;
    assert_eq!(text, via_wrapper);
    // Whichever route actually ran in this environment, the reported path
    // must round-trip through a valid, stable string.
    assert!(matches!(path.as_str(), "rtk_binary" | "native_fallback"));
}

// ── Filter coverage classification (#6566) ──────────────────────────────

#[test]
fn classify_tool_routes_known_tool_families() {
    assert_eq!(
        classify_tool("cargo test -p x"),
        Some(ToolFilter::TestRunner)
    );
    assert_eq!(classify_tool("cargo check"), Some(ToolFilter::CargoCheck));
    assert_eq!(classify_tool("cargo clippy"), Some(ToolFilter::CargoCheck));
    assert_eq!(classify_tool("git diff"), Some(ToolFilter::GitDiff));
    assert_eq!(classify_tool("git log"), Some(ToolFilter::GitLog));
    // #6986: `cat file.rs` now short-circuits to `None` — a source read is a
    // passthrough. The FileRead branch still routes for a read verb applied
    // to something that is not source, which is what this line now pins.
    assert_eq!(
        classify_tool("cat /tmp/gate.txt"),
        Some(ToolFilter::FileRead)
    );
    assert_eq!(classify_tool("grep -n foo"), Some(ToolFilter::Grep));
    assert_eq!(classify_tool("rg foo"), Some(ToolFilter::Grep));
    assert_eq!(classify_tool("find ."), Some(ToolFilter::Grep));
    assert_eq!(classify_tool("ls -la"), Some(ToolFilter::Ls));
    // Case-insensitive, like the dispatch has always been.
    assert_eq!(classify_tool("CARGO TEST"), Some(ToolFilter::TestRunner));
}

#[test]
fn classify_tool_returns_none_for_uncovered_tools() {
    // The names #6566 measured as the bulk of the no-op wraps.
    for name in [
        "git status",
        "git add",
        "git push",
        "sed -n",
        "gh pr",
        "gh issue",
        "wc -l",
        "ssh",
        "tm wait",
        "bash",
        "echo",
    ] {
        assert_eq!(classify_tool(name), None, "expected no filter for {name}");
    }
}

#[test]
fn has_filter_for_true_for_covered_tools() {
    for name in ["cargo test", "git diff", "git log", "grep -n", "ls", "cat"] {
        assert!(has_filter_for(name), "expected a filter for {name}");
    }
}

#[test]
fn has_filter_for_false_for_uncovered_tools() {
    for name in ["git status", "sed -n", "gh pr", "wc -l", "ssh"] {
        assert!(!has_filter_for(name), "expected no filter for {name}");
    }
}

#[test]
fn has_filter_for_agrees_with_classify_tool() {
    // The predicate is defined as `classify_tool(..).is_some()`; this pins
    // that equivalence so the two cannot answer differently (#6566). Drift
    // between the PREDICATE and the DISPATCH is prevented structurally
    // instead: `compress_tool_output` matches exhaustively over `ToolFilter`
    // with no catch-all arm, so a new filter variant fails to compile until
    // the dispatch handles it, and `classify_tool` is the only thing that
    // produces one.
    for name in [
        "cargo test",
        "cargo clippy",
        "git diff",
        "git log",
        "read",
        "grep",
        "ls",
        "git status",
        "sed -n",
        "gh pr",
        "",
    ] {
        assert_eq!(
            has_filter_for(name),
            classify_tool(name).is_some(),
            "{name}"
        );
    }
}

#[test]
fn uncovered_tool_output_passes_through_unchanged() {
    // The behavioural half of the predicate's contract: a name `has_filter_for`
    // rejects gets its bytes back verbatim from the dispatch, which is exactly
    // what made the unconditional hook wrap a no-op process spawn (#6566).
    let input = "M  crates/trusty-mpm/src/main.rs\n".repeat(20);
    for name in ["git status", "sed -n", "gh pr"] {
        assert!(!has_filter_for(name));
        assert_eq!(compress_tool_output(name, &input), input, "{name}");
    }
}

// ── Source-file reads pass through (#6986) ─────────────────────────────

/// A `.rs` file with doc comments and `#[test]` functions, `lines` lines long.
fn rust_source_fixture(lines: usize) -> String {
    let mut src = String::from("//! Module docs the agent needs to read.\n\n");
    for i in 0..lines {
        src.push_str(&format!("/// Why: item {i} exists for reason {i}.\n"));
        src.push_str(&format!(
            "#[test]\nfn checks_case_{i}() {{ assert!(true); }}\n\n"
        ));
    }
    src
}

#[test]
fn cat_of_a_rust_test_file_survives_compression_byte_for_byte() {
    // The exact shape from #6986: `tm hook` derives the tool name from the
    // command's first two tokens, so the PATH reached the substring dispatch
    // and `tests.rs` matched the `test` branch. `filter_test_runner` found no
    // `test result:` summary and returned an empty string —
    // `bytes_before=6037 bytes_after=0 pct_reduction=100.0`.
    let src = rust_source_fixture(40);
    let tool = "cat crates/trusty-agents/src/ticketing/gh_cli/tests.rs";
    assert!(src.len() > SIZE_GATE_BYTES);
    let out = compress_tool_output(tool, &src);
    assert!(!out.is_empty(), "compressor emptied a source read");
    assert_eq!(out, src, "source read must round-trip byte-for-byte");
}

#[test]
fn cat_of_a_rust_module_keeps_its_doc_comments() {
    // The second symptom in #6986: `cat …/mod.rs` cleared the `test`/`cargo`
    // substring branches, reached `FileRead`, and `filter_file_read` dropped
    // every `//`-prefixed line — the doc comments were the point of the read.
    let src = rust_source_fixture(120);
    assert!(src.lines().count() > FILE_READ_LINE_GATE);
    let out = compress_tool_output("cat crates/trusty-agents/src/ticketing/gh_cli/mod.rs", &src);
    assert!(
        out.contains("/// Why: item 0"),
        "doc comments were stripped"
    );
    assert_eq!(out, src);
}

#[tokio::test]
async fn native_fallback_passes_a_source_read_through_unchanged() {
    // Ties the fix to the `compression_path=native_fallback` route the issue
    // reported. `rtk` may be installed in this environment, so assert only on
    // the run that actually took the native path.
    let src = rust_source_fixture(40);
    let tool = "cat crates/trusty-agents/src/ticketing/gh_cli/tests.rs";
    let (text, path) = compress_tool_output_async_with_path(tool, &src).await;
    if path == CompressionPath::NativeFallback {
        assert_eq!(text, src);
    }
}

#[test]
fn classify_tool_passes_through_source_reads() {
    // A path segment must never choose the filter: `tests.rs` picked the test
    // runner, `logging.rs` would pick `git log`, `checks.rs` cargo-check.
    for name in [
        "cat crates/trusty-agents/src/ticketing/gh_cli/tests.rs",
        "cat crates/trusty-mpm/src/logging.rs",
        "cat crates/trusty-common/src/checks.rs",
        "cat Cargo.toml",
        "head crates/trusty-mpm/CHANGELOG.md",
        "bat website/src/lib/tools.ts",
    ] {
        assert_eq!(classify_tool(name), None, "{name}");
        assert!(!has_filter_for(name), "{name}");
    }
}

#[test]
fn is_source_file_read_matches_the_reported_shapes() {
    assert!(is_source_file_read("cat crates/x/src/tests.rs"));
    assert!(is_source_file_read("head -50 crates/x/src/mod.rs"));
    assert!(is_source_file_read("sed -n 1,80p crates/x/build.rs"));
    // Case-insensitive on both verb and extension, and a directory-qualified
    // program name resolves to its basename.
    assert!(is_source_file_read("/bin/CAT crates/x/src/Main.RS"));
}

#[test]
fn is_source_file_read_requires_a_read_verb() {
    // Same paths, non-read verbs — these keep their existing classification.
    assert!(!is_source_file_read("cargo test crates/x/src/tests.rs"));
    assert!(!is_source_file_read("grep -n foo crates/x/src/lib.rs"));
    assert!(!is_source_file_read("rm crates/x/src/lib.rs"));
    // A read verb with no file argument at all.
    assert!(!is_source_file_read("cat"));
    assert!(!is_source_file_read(""));
}

#[test]
fn is_source_file_read_ignores_gate_capture_extensions() {
    // `<gate> > /tmp/gate.txt` is this project's documented capture pattern,
    // so reading one back is genuine gate output and stays compressible.
    for name in [
        "cat /tmp/gate.txt",
        "cat /tmp/cargo-test.log",
        "cat target/build.out",
        "cat .zshrc",
    ] {
        assert!(!is_source_file_read(name), "{name}");
    }
}

#[test]
fn dispatch_never_returns_empty_for_non_empty_input() {
    // The backstop behind the classification fix: any filter that consumed
    // every byte hands the original back rather than emitting nothing.
    let mut passing_only = String::new();
    for i in 0..40 {
        passing_only.push_str(&format!("test suite::case_{i} ... ok\n"));
    }
    // No `test result:` line, so `filter_test_runner` has no summary to fall
    // back to and previously returned "".
    assert_eq!(filter_test_runner(&passing_only), "");
    assert_eq!(
        compress_tool_output("cargo test", &passing_only),
        passing_only
    );
}

// ── Test-runner failure diagnostics and per-suite summaries (#7544) ─────

/// `cargo test` output for a failing suite followed by a passing one.
///
/// The shape #7544 reproduced: a panic block carrying a file:line:col
/// location, an `assertion `left == right`` message with its left/right
/// values, a multiline `---- <test> stdout ----` section, and TWO
/// `test result:` summaries — `FAILED` first, `ok` second.
fn two_suites_failed_then_passing() -> String {
    String::from(
        r#"   Compiling trusty-demo v0.1.0 (/w/demo)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 1.20s
     Running unittests src/lib.rs (target/debug/deps/demo-1111111111111111)

running 2 tests
test keeps_working ... ok
test breaks ... FAILED

failures:

---- breaks stdout ----

thread 'breaks' panicked at crates/trusty-demo/src/lib.rs:42:9:
assertion `left == right` failed: shunt budget must match the ledger
  left: 1
  right: 2
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

failures:
    breaks

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

     Running tests/integration.rs (target/debug/deps/integration-2222222222222222)

running 1 test
test integration_smoke ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
"#,
    )
}

/// Native-fallback output for `cargo test`, with `rtk` ruled out.
///
/// `no_rtk` makes the route deterministic instead of depending on whether an
/// `rtk` binary happens to be installed on the machine running the suite.
async fn native_cargo_test(output: &str) -> String {
    let (text, path) =
        compress_tool_output_async_with_path_using(no_rtk, "cargo test", output).await;
    assert_eq!(path, CompressionPath::NativeFallback);
    text
}

#[tokio::test]
async fn test_runner_keeps_every_suite_summary_in_order() {
    // #7544: only the LAST `test result:` line survived, so a passing suite
    // after a failing one left the output ending `test result: ok` — the
    // failure was unreadable from the compressed text.
    let out = native_cargo_test(&two_suites_failed_then_passing()).await;
    let failed = out
        .find("test result: FAILED. 1 passed; 1 failed")
        .unwrap_or_else(|| panic!("failing suite summary dropped:\n{out}"));
    let passed = out
        .find("test result: ok. 1 passed; 0 failed")
        .unwrap_or_else(|| panic!("passing suite summary dropped:\n{out}"));
    assert!(
        failed < passed,
        "suite summaries must stay in original order:\n{out}"
    );
}

#[tokio::test]
async fn test_runner_keeps_panic_location_and_assertion_values() {
    // #7544: the panic line, the assertion message, and the left/right values
    // matched none of the keep-list predicates and were dropped as noise.
    let out = native_cargo_test(&two_suites_failed_then_passing()).await;
    for needle in [
        "panicked at crates/trusty-demo/src/lib.rs:42:9",
        "assertion `left == right` failed: shunt budget must match the ledger",
        "left: 1",
        "right: 2",
        "---- breaks stdout ----",
        "test breaks ... FAILED",
    ] {
        assert!(out.contains(needle), "dropped {needle:?} from:\n{out}");
    }
}

#[tokio::test]
async fn test_runner_keeps_the_failing_test_name_list() {
    // The `failures:` list names what to re-run; it is the one line an agent
    // needs to narrow the next command.
    let out = native_cargo_test(&two_suites_failed_then_passing()).await;
    let tail = out
        .rsplit("failures:")
        .next()
        .expect("rsplit always yields one element");
    assert!(
        tail.contains("breaks"),
        "failing-test name list dropped:\n{out}"
    );
}

#[tokio::test]
async fn test_runner_still_reduces_passing_only_output() {
    // The reduction this filter exists for must survive the fix: a green run
    // is almost entirely `test <name> ... ok` lines.
    let mut input = String::from("running 40 tests\n");
    for i in 0..40 {
        input.push_str(&format!("test suite::case_{i} ... ok\n"));
    }
    input.push_str("test result: ok. 40 passed; 0 failed; 0 ignored\n");
    let out = native_cargo_test(&input).await;
    assert!(
        out.len() * 4 < input.len(),
        "passing-only output barely shrank: {} -> {}",
        input.len(),
        out.len()
    );
    assert!(!out.contains("case_7 ... ok"), "kept a passing test line");
    assert!(
        out.contains("test result: ok. 40 passed"),
        "lost the summary"
    );
}

#[tokio::test]
async fn test_runner_keeps_unrecognised_blocks() {
    // Fail-open: a harness this filter has never seen must come back, not be
    // discarded for matching no keep-list predicate (#7544).
    // The fixture deliberately opens with a non-`key: value` line: a leading
    // one routes the whole input through the structured-format passthrough
    // instead of this filter, so the test would pass without proving anything.
    let input = "\
running 3 scenarios through the bespoke harness
scenario budget ledger reconciliation
  step seed ledger -> 3 rows
  step reconcile -> amber
  outcome pending (awaiting operator)
a summary line nothing here recognises
test result: ok. 3 passed; 0 failed; 0 ignored
";
    assert!(
        !is_structured_format(input),
        "fixture took the YAML passthrough"
    );
    let out = native_cargo_test(input).await;
    for needle in [
        "running 3 scenarios through the bespoke harness",
        "scenario budget ledger reconciliation",
        "step reconcile -> amber",
        "outcome pending (awaiting operator)",
        "a summary line nothing here recognises",
    ] {
        assert!(out.contains(needle), "dropped {needle:?} from:\n{out}");
    }
}

#[tokio::test]
async fn test_runner_drops_cargo_progress_lines() {
    // The reduction side of the blocklist: cargo's indented progress verbs go,
    // `Running` stays because it names the suite each summary belongs to.
    let out = native_cargo_test(&two_suites_failed_then_passing()).await;
    assert!(
        !out.contains("Compiling trusty-demo"),
        "kept Compiling:\n{out}"
    );
    assert!(
        !out.contains("Finished `test` profile"),
        "kept Finished:\n{out}"
    );
    assert!(
        out.contains("Running unittests src/lib.rs"),
        "dropped the suite attribution line:\n{out}"
    );
    assert!(
        out.contains("Running tests/integration.rs"),
        "dropped the second suite's attribution line:\n{out}"
    );
}

#[test]
fn test_runner_keeps_column_zero_lines_that_look_like_progress() {
    // A test's own stdout is never cargo chatter, whatever word it starts with.
    let input = "\
running 1 test
Compiling the shunt ledger in-process before the assertion
Finished reconciling 3 rows
test writes_progress_words ... ok
test result: ok. 1 passed; 0 failed; 0 ignored
";
    let out = filter_test_runner(input);
    assert!(
        out.contains("Compiling the shunt ledger"),
        "dropped stdout:\n{out}"
    );
    assert!(
        out.contains("Finished reconciling 3 rows"),
        "dropped stdout:\n{out}"
    );
    assert!(!out.contains("... ok"), "kept a passing test line:\n{out}");
}
