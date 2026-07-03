//! Spike: quantify realistic token savings from `compress_tool_output_async`
//! if it ran inline inside an MCP-proxy tool response.
//!
//! Why: issue #1953 needs real numbers (not the rtk/ztk README claims) to
//! decide whether routing built-in tools (Bash/Read/Grep) through a
//! tm-provided MCP proxy is worth the provenance/permission cost documented
//! in `docs/specs/tool-output-interception-seam.md`. This binary exercises
//! the exact compression function
//! (`trusty_agents::compress::compress_tool_output_async`) that would sit
//! inside such a proxy tool's response path, against four realistic tool
//! output fixtures.
//! What: builds four fixtures inline (cargo test noise, a git diff, a
//! `grep -r` result, an `ls -la` listing), runs each through
//! `compress_tool_output_async`, and prints a before/after byte count,
//! `compress::estimate_tokens` count, and percent reduction per fixture plus
//! an aggregate. Reports which compression path ran (RTK subprocess vs. the
//! native fallback chain) since RTK is an optional external binary that may
//! not be installed in the environment running this spike.
//! Test: run with
//! `cargo run -p trusty-agents --example tool_output_compression_spike`; its
//! captured stdout is quoted verbatim in the design doc's Spike section.

use trusty_agents::compress::{compress_tool_output_async, estimate_tokens};

/// One tool-output fixture: the tool name (drives filter dispatch) and the
/// raw output text.
struct Fixture {
    label: &'static str,
    tool_name: &'static str,
    output: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let fixtures = vec![
        Fixture {
            label: "cargo test (mostly-passing suite)",
            tool_name: "cargo test",
            output: cargo_test_fixture(),
        },
        Fixture {
            label: "git diff (multi-file changeset)",
            tool_name: "git diff",
            output: git_diff_fixture(),
        },
        Fixture {
            label: "grep -r (many matches)",
            tool_name: "grep",
            output: grep_fixture(),
        },
        Fixture {
            label: "ls -la (large directory listing)",
            tool_name: "ls",
            output: ls_fixture(),
        },
    ];

    println!(
        "compression path: {}",
        if rtk_on_path() {
            "RTK subprocess (rtk found on PATH)"
        } else {
            "native fallback chain (rtk NOT on PATH)"
        }
    );
    println!();

    let mut totals = Totals::default();
    for fixture in &fixtures {
        let before_bytes = fixture.output.len();
        let before_tokens = estimate_tokens(&fixture.output);
        let compressed = compress_tool_output_async(fixture.tool_name, &fixture.output).await;
        let after_bytes = compressed.len();
        let after_tokens = estimate_tokens(&compressed);

        println!(
            "--- {} (tool_name=\"{}\") ---",
            fixture.label, fixture.tool_name
        );
        println!(
            "  bytes:  {before_bytes:>6} -> {after_bytes:>6}  ({:.1}% reduction)",
            pct_reduction(before_bytes, after_bytes)
        );
        println!(
            "  tokens: {before_tokens:>6} -> {after_tokens:>6}  ({:.1}% reduction)",
            pct_reduction(before_tokens, after_tokens)
        );
        println!();

        totals.add(before_bytes, after_bytes, before_tokens, after_tokens);
    }

    println!("=== Aggregate across {} fixtures ===", fixtures.len());
    println!(
        "  bytes:  {:>6} -> {:>6}  ({:.1}% reduction)",
        totals.before_bytes,
        totals.after_bytes,
        pct_reduction(totals.before_bytes, totals.after_bytes)
    );
    println!(
        "  tokens: {:>6} -> {:>6}  ({:.1}% reduction)",
        totals.before_tokens,
        totals.after_tokens,
        pct_reduction(totals.before_tokens, totals.after_tokens)
    );

    Ok(())
}

/// Running before/after totals across all fixtures.
#[derive(Default)]
struct Totals {
    before_bytes: usize,
    after_bytes: usize,
    before_tokens: usize,
    after_tokens: usize,
}

impl Totals {
    fn add(
        &mut self,
        before_bytes: usize,
        after_bytes: usize,
        before_tokens: usize,
        after_tokens: usize,
    ) {
        self.before_bytes += before_bytes;
        self.after_bytes += after_bytes;
        self.before_tokens += before_tokens;
        self.after_tokens += after_tokens;
    }
}

/// Percent reduction from `before` to `after`, 0.0 when `before` is 0.
fn pct_reduction(before: usize, after: usize) -> f64 {
    if before == 0 {
        return 0.0;
    }
    (1.0 - (after as f64 / before as f64)) * 100.0
}

/// Minimal PATH probe mirroring `compress::tool_output::rtk`'s private
/// `which` helper — duplicated here (not imported) since that helper is
/// crate-private and this is throwaway spike code, not a permanent API
/// consumer.
fn rtk_on_path() -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join("rtk").is_file()))
        .unwrap_or(false)
}

/// Realistic `cargo test` output: many passing tests plus a couple of
/// failures and the summary line, matching the shape `filter_test_runner`
/// expects (`test <name> ... ok` / `FAILED` / `test result:`).
fn cargo_test_fixture() -> String {
    let mut out = String::from("running 42 tests\n");
    for i in 0..38 {
        out.push_str(&format!("test compress::tests::case_{i} ... ok\n"));
    }
    out.push_str("test compress::tests::rejects_malformed_input ... FAILED\n");
    out.push_str("test compress::tests::handles_empty_output ... FAILED\n");
    out.push_str("\nfailures:\n\n---- compress::tests::rejects_malformed_input stdout ----\n");
    out.push_str("thread panicked at 'assertion failed: `(left == right)`'\n");
    out.push_str("---- compress::tests::handles_empty_output stdout ----\n");
    out.push_str("thread panicked at 'called `Option::unwrap()` on a `None` value'\n");
    out.push_str("\nfailures:\n    compress::tests::rejects_malformed_input\n");
    out.push_str("    compress::tests::handles_empty_output\n\n");
    out.push_str(
        "test result: FAILED. 40 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out\n",
    );
    out
}

/// Realistic `git diff` snippet spanning two files with several hunks.
fn git_diff_fixture() -> String {
    let mut out = String::from("diff --git a/src/daemon/optimizer.rs b/src/daemon/optimizer.rs\n");
    out.push_str(
        "index 3a1f2c9..7bd44e1 100644\n--- a/src/daemon/optimizer.rs\n+++ b/src/daemon/optimizer.rs\n",
    );
    out.push_str(
        "@@ -12,7 +12,9 @@ pub fn optimize_tool_output(cfg: &Config, tool: &str, payload: &mut Value) {\n",
    );
    for i in 0..8 {
        out.push_str(&format!("     let line_{i} = payload.get(\"line_{i}\");\n"));
    }
    out.push_str(
        "-    // old behavior\n+    // new behavior: route through compress_tool_output_async\n",
    );
    out.push_str("+    // and record the compression ratio for telemetry\n");
    out.push_str("@@ -40,6 +42,10 @@ impl Optimizer {\n");
    for i in 0..12 {
        out.push_str(&format!("     context_line_{i}\n"));
    }
    out.push_str("+    fn new_helper(&self) -> bool {\n+        true\n+    }\n");
    out.push_str("diff --git a/tests/optimizer_tests.rs b/tests/optimizer_tests.rs\n");
    out.push_str(
        "index 9c88a11..1d2e3f4 100644\n--- a/tests/optimizer_tests.rs\n+++ b/tests/optimizer_tests.rs\n",
    );
    out.push_str(
        "@@ -1,5 +1,7 @@\n use super::*;\n\n+#[test]\n+fn compresses_before_ring_buffer() {}\n",
    );
    for i in 0..6 {
        out.push_str(&format!(
            "     assert_eq!(fixture_{i}(), expected_{i}());\n"
        ));
    }
    out
}

/// Realistic `grep -r` result: many `path:line:match` lines.
fn grep_fixture() -> String {
    let mut out = String::new();
    for i in 0..120 {
        out.push_str(&format!(
            "crates/trusty-agents/src/compress/tool_output/mod{i}.rs:{}:    let compressed = compress_tool_output(tool_name, output);\n",
            10 + i
        ));
    }
    out
}

/// Realistic `ls -la` output: many file-listing lines.
fn ls_fixture() -> String {
    let mut out = String::from("total 912\n");
    out.push_str("drwxr-xr-x  42 user  staff   1344 Jul  3 09:00 .\n");
    out.push_str("drwxr-xr-x  18 user  staff    576 Jul  3 08:00 ..\n");
    for i in 0..150 {
        out.push_str(&format!(
            "-rw-r--r--   1 user  staff  {:>5} Jul  3 09:{:02} module_{i}.rs\n",
            1200 + i * 7,
            i % 60
        ));
    }
    out
}
