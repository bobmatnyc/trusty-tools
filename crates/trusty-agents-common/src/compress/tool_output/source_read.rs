//! Source-file read detection for tool-output compression.
//!
//! Why: #6986 — the dispatch classifies by substring against a tool name that
//! is the wrapped command's first two tokens, so `cat …/gh_cli/tests.rs`
//! matched the `test` branch and `filter_test_runner` deleted every byte
//! (`bytes_before=6037 bytes_after=0`), and `cat …/mod.rs` reached
//! `filter_file_read`, which strips `//`-prefixed lines and so removed the
//! doc comments the agent was reading. Those filters exist for gate output —
//! a test runner's log, a compiler's log — not for source an agent is reading
//! to edit it.
//! What: [`is_source_file_read`], the predicate [`super::classify_tool`]
//! consults first. It answers from the command alone: a read verb applied to
//! a path with a source or prose extension. That is the reliable signal here
//! because the tool name IS the command (`tm hook`'s rewrite passes
//! `effective_tool_name`, e.g. `cat crates/x/src/tests.rs`), whereas output
//! shape cannot separate a `.rs` file's contents from a compiler log that
//! quotes source.
//! Test: `is_source_file_read_*`, `classify_tool_passes_through_source_reads`,
//! and `cat_of_a_rust_test_file_survives_compression_byte_for_byte` in
//! `tool_output::tests`.

/// Commands whose whole job is to print a file's bytes back to a reader.
///
/// Why: A read verb names the caller's intent — "show me this file" — which
/// no substring of the path can override. `sed` and `tail` are listed for
/// completeness even though `effective_tool_name` usually drops their path
/// argument behind a flag (`sed -n`, `tail -f`); when a path does survive,
/// the same passthrough must apply.
/// What: Matched against the command's first token, with any directory
/// prefix stripped, case-insensitively.
/// Test: `is_source_file_read_requires_a_read_verb`.
const READ_VERBS: &[&str] = &["cat", "bat", "head", "tail", "nl", "less", "more", "sed"];

/// File extensions whose contents are source or prose read for editing.
///
/// Why: The extension is what separates "an agent is reading code" from "an
/// agent is reading a gate's captured output". `.txt`, `.log` and `.out` are
/// deliberately ABSENT: `<gate> > /tmp/gate.txt` is this project's documented
/// capture pattern (`assets/agents/BASE-AGENT.md`), so a later
/// `cat /tmp/gate.txt` is genuine gate output that must stay compressible.
/// What: Lowercase, without the dot, compared against the final `.`-segment
/// of a path token's basename.
/// Test: `is_source_file_read_ignores_gate_capture_extensions`.
const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "toml", "md", "py", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "java", "kt", "rb",
    "php", "c", "h", "cc", "cpp", "hpp", "cs", "swift", "sh", "bash", "zsh", "sql", "svelte",
    "vue", "css", "scss", "html", "yaml", "yml", "json", "tsv",
];

/// Whether `tool_name` is a read verb applied to a source or prose file.
///
/// Why: #6986 — such a command's output is the file itself, and every filter
/// in this module would damage it: the test-runner filter drops any line that
/// is not a failure, and the file-read filter drops every `//` and `#` line.
/// The compressor is for gate output, so a source read must bypass it
/// entirely rather than be compressed more gently.
/// What: `true` when the first whitespace token (directory prefix stripped,
/// lowercased) is in [`READ_VERBS`] and any later token's basename ends in a
/// [`SOURCE_EXTENSIONS`] entry. A read verb with no source-file argument
/// (`cat /tmp/gate.txt`, a bare `cat`) returns `false` and keeps its existing
/// classification.
/// Test: `is_source_file_read_matches_the_reported_shapes`,
/// `is_source_file_read_requires_a_read_verb`,
/// `is_source_file_read_ignores_gate_capture_extensions`.
pub fn is_source_file_read(tool_name: &str) -> bool {
    let mut tokens = tool_name.split_whitespace();
    let Some(verb) = tokens.next() else {
        return false;
    };
    let verb = verb.rsplit('/').next().unwrap_or(verb).to_ascii_lowercase();
    if !READ_VERBS.contains(&verb.as_str()) {
        return false;
    }
    tokens.any(has_source_extension)
}

/// Whether a command token names a file with a [`SOURCE_EXTENSIONS`] suffix.
fn has_source_extension(token: &str) -> bool {
    let basename = token.rsplit('/').next().unwrap_or(token);
    let Some((stem, ext)) = basename.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() {
        // A dotfile (`.zshrc`) has no extension, only a leading dot.
        return false;
    }
    SOURCE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
}
