//! Diff loading, truncation, and salient-identifier extraction.
//!
//! Why: the pipeline needs the unified diff as text, capped to `MAX_DIFF_CHARS`
//! to avoid token budget overruns, and a small set of changed symbols to drive
//! the code-search context retrieval step.
//!
//! What: exposes `DiffSource` (GitHub PR or local file path), `load_diff` to
//! fetch / read the raw diff text, `truncate_diff` to apply the char cap, and
//! `extract_identifiers` to pull changed function/type names from `+`/`-` lines.
//!
//! Test: `truncate_does_not_exceed_limit`, `extract_identifiers_finds_symbols`,
//! `local_diff_reads_file`.

use tracing::{debug, warn};

use crate::{
    config::constants::MAX_DIFF_CHARS,
    integrations::github::{GithubClient, GithubError, fetch_pr_diff},
};

// ─── Diff source ──────────────────────────────────────────────────────────────

/// Where to obtain the unified diff.
///
/// Why: the CLI supports both fetching from GitHub and reading a local file;
/// the pipeline step must handle both without special-casing throughout.
/// What: two variants — `Github` (fetches via API) and `LocalFile` (reads from
/// disk; always dry-run and requires no GitHub credentials).
/// Test: `local_diff_reads_file`.
#[derive(Debug, Clone)]
pub enum DiffSource {
    /// Fetch the diff from a live GitHub pull request.
    Github {
        /// GitHub organisation or user.
        owner: String,
        /// Repository name.
        repo: String,
        /// Pull request number.
        pr: u64,
        /// Resolved GitHub access token.
        token: String,
    },
    /// Read a unified diff from a local file (dry-run only; no GitHub needed).
    LocalFile {
        /// Absolute path to the `.diff` or `.patch` file.
        path: std::path::PathBuf,
    },
}

// ─── Load ─────────────────────────────────────────────────────────────────────

/// Load the raw diff text according to the given `DiffSource`.
///
/// Why: centralises the fetch / read logic so the rest of the pipeline just
/// receives a `String` regardless of where the diff came from.
/// What: for `Github`, calls `fetch_pr_diff`; for `LocalFile`, reads the file
/// with `std::fs::read_to_string`.  Returns a `GithubError` if the GitHub API
/// fails; wraps I/O errors in `GithubError::Transport` for `LocalFile`.
/// Test: `local_diff_reads_file` writes a tmp file and calls this.
pub async fn load_diff(source: &DiffSource) -> Result<String, GithubError> {
    match source {
        DiffSource::Github {
            owner,
            repo,
            pr,
            token,
        } => {
            let client = GithubClient::new()?;
            debug!(owner, repo, pr, "fetching PR diff from GitHub");
            fetch_pr_diff(&client, owner, repo, *pr, token).await
        }
        DiffSource::LocalFile { path } => {
            debug!(path = %path.display(), "loading diff from local file");
            std::fs::read_to_string(path).map_err(|e| {
                GithubError::Transport(format!("read local diff {}: {e}", path.display()))
            })
        }
    }
}

// ─── Truncate ─────────────────────────────────────────────────────────────────

/// Truncate a diff to at most `MAX_DIFF_CHARS` characters.
///
/// Why: very large diffs cause LLM token budget overruns and degraded review
/// quality.  The cap is defined in `constants::MAX_DIFF_CHARS` (160 000 chars).
/// What: if the diff exceeds the cap, it is cut at the nearest hunk boundary
/// before the cap, or at the raw char limit if no hunk boundary is found.  A
/// `[DIFF TRUNCATED — ...]` marker is appended so the LLM can see the diff is
/// incomplete.
/// Test: `truncate_does_not_exceed_limit`, `truncate_preserves_short_diff`.
pub fn truncate_diff(diff: &str) -> String {
    if diff.len() <= MAX_DIFF_CHARS {
        return diff.to_string();
    }

    // Try to cut at the last hunk separator before the limit.
    let candidate = &diff[..MAX_DIFF_CHARS];
    let cut_pos = candidate
        .rfind("\n@@")
        .map(|p| p + 1) // keep the newline before @@
        .unwrap_or(MAX_DIFF_CHARS);

    let truncated = &diff[..cut_pos];
    warn!(
        original_chars = diff.len(),
        kept_chars = truncated.len(),
        "diff truncated to MAX_DIFF_CHARS"
    );
    format!(
        "{truncated}\n[DIFF TRUNCATED — {remaining} chars omitted; review may be incomplete]",
        remaining = diff.len() - truncated.len()
    )
}

// ─── Truncation detection ──────────────────────────────────────────────────────

/// Marker prefix `truncate_diff` appends when it cuts an over-cap diff.
///
/// Why: the runner's fail-closed guard (#1638) must detect that the diff handed
/// to the reviewer had content removed; matching the exact marker string keeps
/// the producer (`truncate_diff`) and the consumer (`diff_was_truncated`) in sync
/// from one definition rather than two copies of a magic string.
/// What: the literal prefix emitted by `truncate_diff` (see the `format!` above).
/// Test: `diff_was_truncated_detects_diff_marker`.
pub const DIFF_TRUNCATED_MARKER: &str = "[DIFF TRUNCATED";

/// Marker prefix `FilteredDiff::render_for_prompt` appends when it cuts at the
/// char budget (whole files or trailing hunks dropped).
///
/// Why: `render_for_prompt` drops whole files at the END of a large diff (a
/// changed signature in a late file is silently lost); the runner must detect
/// this and fail closed rather than review the visible portion only (#1638).
/// What: the literal prefix emitted by `render_for_prompt` (see `models.rs`).
/// Test: `diff_was_truncated_detects_render_marker`.
pub const RENDER_TRUNCATED_MARKER: &str = "[RENDER TRUNCATED";

/// Returns `true` when the rendered diff handed to the reviewer had content
/// removed by either truncation path.
///
/// Why: a reviewer working from a partial diff produces wrong/incomplete reviews
/// — the live symptom was a changed `build(…, null)` signature cut off so the
/// reviewer literally could not see it (#1638).  The runner uses this predicate
/// to fail CLOSED to UNKNOWN (consistent with the #1241 / #590 philosophy) rather
/// than silently reviewing the visible portion.
/// What: a pure check for the self-announcing markers that `truncate_diff` and
/// `FilteredDiff::render_for_prompt` append when (and only when) they drop content.
/// Detecting the markers — rather than re-deriving length math — keeps this in
/// lock-step with the producers and is robust to either path firing.
/// Test: `diff_was_truncated_detects_diff_marker`,
/// `diff_was_truncated_detects_render_marker`,
/// `diff_was_truncated_false_on_complete_diff`.
pub fn diff_was_truncated(rendered: &str) -> bool {
    rendered.contains(DIFF_TRUNCATED_MARKER) || rendered.contains(RENDER_TRUNCATED_MARKER)
}

// ─── Identifier extraction ────────────────────────────────────────────────────

/// Extract salient identifier names from changed (`+`/`-`) diff lines.
///
/// Why: the context-retrieval step (search client) needs a short list of
/// changed symbols to build focused search queries.  Scanning the diff for
/// function/type names is cheaper and more precise than using the full diff as
/// a single blob query.
/// What: scans lines that start with `+` or `-` (excluding `+++`/`---` header
/// lines) for tokens that look like Rust/Python/JS identifiers: `fn foo`, `def
/// foo`, `class Foo`, `struct Foo`, `type Foo`, `impl Foo`, `interface Foo`,
/// `const FOO`, `enum Foo`.  Also captures bare `snake_case` and `CamelCase`
/// tokens from changed lines.  Returns up to `max_identifiers` unique names.
/// Test: `extract_identifiers_finds_symbols`, `extract_identifiers_deduplicates`.
pub fn extract_identifiers(diff: &str, max_identifiers: usize) -> Vec<String> {
    use std::collections::LinkedList;

    // Keyword patterns: keyword followed by identifier.
    let kw_prefixes: &[&str] = &[
        "fn ",
        "def ",
        "class ",
        "struct ",
        "type ",
        "impl ",
        "interface ",
        "const ",
        "enum ",
        "trait ",
        "async fn ",
        "pub fn ",
        "pub struct ",
        "pub enum ",
        "pub trait ",
        "pub type ",
        "pub const ",
        "private ",
        "protected ",
    ];

    let mut seen = std::collections::HashSet::new();
    let mut results: LinkedList<String> = LinkedList::new();

    for line in diff.lines() {
        // Only look at added/removed lines; skip file header lines.
        let content = if line.starts_with("+++") || line.starts_with("---") {
            continue;
        } else if let Some(rest) = line.strip_prefix('+') {
            rest.trim()
        } else if let Some(rest) = line.strip_prefix('-') {
            rest.trim()
        } else {
            continue;
        };

        // Keyword-prefixed extraction.
        for kw in kw_prefixes {
            if let Some(after_kw) = content.find(kw).map(|i| &content[i + kw.len()..]) {
                let name = after_kw
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .next()
                    .unwrap_or("")
                    .trim();
                if is_valid_identifier(name) && seen.insert(name.to_string()) {
                    results.push_back(name.to_string());
                    if results.len() >= max_identifiers {
                        return results.into_iter().collect();
                    }
                }
            }
        }

        // Also capture CamelCase tokens (likely type names).
        for token in content.split(|c: char| !c.is_alphanumeric() && c != '_') {
            if is_camel_case(token) && seen.insert(token.to_string()) {
                results.push_back(token.to_string());
                if results.len() >= max_identifiers {
                    return results.into_iter().collect();
                }
            }
        }
    }

    results.into_iter().collect()
}

/// Returns `true` if `s` looks like a valid non-trivial identifier.
///
/// Why: filters out empty strings, single-char tokens, and reserved words.
/// What: checks length ≥ 2, starts with alpha/underscore, and is not a
/// common short reserved word (`if`, `as`, `fn`, `in`, `is`, `do`).
/// Test: covered transitively by `extract_identifiers_finds_symbols`.
fn is_valid_identifier(s: &str) -> bool {
    if s.len() < 2 {
        return false;
    }
    let first = s.chars().next().unwrap_or(' ');
    if !first.is_alphabetic() && first != '_' {
        return false;
    }
    // Filter short reserved words.
    !matches!(
        s,
        "if" | "as" | "fn" | "in" | "is" | "do" | "be" | "by" | "or"
    )
}

/// Returns `true` if `s` looks like a CamelCase type name.
///
/// Why: type names in diffs are often used bare (not after a `struct` keyword)
/// in function signatures; capturing them improves search recall.
/// What: checks the token starts with an uppercase letter, is at least 3 chars,
/// and contains at least one lowercase letter (filtering all-caps constants).
/// Test: `camel_case_detection`.
fn is_camel_case(s: &str) -> bool {
    if s.len() < 3 {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap_or(' ');
    first.is_uppercase()
        && s.chars().any(|c| c.is_lowercase())
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

// ─── Diff-path parsing ────────────────────────────────────────────────────────

/// Extract changed file paths from a unified diff.
///
/// Why: the search and analyze clients need the list of changed files to scope
/// their queries.
/// What: scans `--- a/` and `+++ b/` header lines; strips the `a/`/`b/`
/// prefix; deduplicates and returns a sorted list.
/// Test: `extract_changed_files_from_diff`.
pub fn extract_changed_files(diff: &str) -> Vec<String> {
    let mut files = std::collections::BTreeSet::new();
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            let path = rest.trim();
            if !path.is_empty() && path != "/dev/null" {
                files.insert(path.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("--- a/") {
            let path = rest.trim();
            if !path.is_empty() && path != "/dev/null" {
                files.insert(path.to_string());
            }
        }
    }
    files.into_iter().collect()
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_DIFF: &str = r#"diff --git a/src/auth.rs b/src/auth.rs
index abc..def 100644
--- a/src/auth.rs
+++ b/src/auth.rs
@@ -10,7 +10,7 @@ use crate::config::Config;
-pub fn authenticate(user: &str) -> Result<Token, AuthError> {
+pub async fn authenticate(user: &str, config: &Config) -> Result<Token, AuthError> {
     // Implementation
+    let token = Token::new(user, config.secret());
-    let token = Token::new(user);
     Ok(token)
 }
+
+struct TokenCache {
+    inner: HashMap<String, Token>,
+}
"#;

    #[test]
    fn truncate_preserves_short_diff() {
        let short = "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n";
        let result = truncate_diff(short);
        assert_eq!(result, short, "short diff should pass through unchanged");
    }

    #[test]
    fn truncate_does_not_exceed_limit() {
        // Build a diff longer than MAX_DIFF_CHARS.
        let long = "a".repeat(MAX_DIFF_CHARS + 1000);
        let result = truncate_diff(&long);
        assert!(
            result.len() <= MAX_DIFF_CHARS + 200,
            "truncated diff must not greatly exceed MAX_DIFF_CHARS: len={}",
            result.len()
        );
        assert!(
            result.contains("[DIFF TRUNCATED"),
            "truncated diff must contain the truncation marker"
        );
    }

    #[test]
    fn truncate_prefers_hunk_boundary() {
        // Build a diff with a hunk boundary within the limit.
        let hunk_header = "\n@@ -1,3 +1,3 @@ fn foo";
        let mut diff = "a".repeat(MAX_DIFF_CHARS / 2);
        diff.push_str(hunk_header);
        diff.push_str(&"b".repeat(MAX_DIFF_CHARS)); // Push over limit.
        let result = truncate_diff(&diff);
        // The truncation marker should appear.
        assert!(result.contains("[DIFF TRUNCATED"));
    }

    #[test]
    fn diff_was_truncated_detects_diff_marker() {
        // A diff long enough that `truncate_diff` actually cuts it must be flagged.
        let long = "a".repeat(MAX_DIFF_CHARS + 1000);
        let rendered = truncate_diff(&long);
        assert!(
            rendered.contains(DIFF_TRUNCATED_MARKER),
            "precondition: truncate_diff must append its marker"
        );
        assert!(
            diff_was_truncated(&rendered),
            "diff_was_truncated must detect the truncate_diff marker"
        );
    }

    #[test]
    fn diff_was_truncated_detects_render_marker() {
        // Simulate the marker render_for_prompt appends when it drops whole files.
        let rendered = "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n+fn a() {}\n\
             \n[RENDER TRUNCATED — char budget (160000) reached; ~3 file(s) omitted; \
             review covers only the visible portion above]\n";
        assert!(
            diff_was_truncated(rendered),
            "diff_was_truncated must detect the render_for_prompt marker"
        );
    }

    #[test]
    fn diff_was_truncated_false_on_complete_diff() {
        assert!(
            !diff_was_truncated(SAMPLE_DIFF),
            "a complete diff with no truncation marker must not be flagged"
        );
        assert!(
            !diff_was_truncated(""),
            "an empty diff must not be flagged as truncated"
        );
    }

    #[test]
    fn extract_identifiers_finds_symbols() {
        let ids = extract_identifiers(SAMPLE_DIFF, 20);
        // `authenticate` should be found (after `pub fn` / `pub async fn`).
        assert!(
            ids.contains(&"authenticate".to_string()),
            "expected 'authenticate' in identifiers: {ids:?}"
        );
        // `TokenCache` should be found (CamelCase).
        assert!(
            ids.contains(&"TokenCache".to_string()),
            "expected 'TokenCache' in identifiers: {ids:?}"
        );
    }

    #[test]
    fn extract_identifiers_deduplicates() {
        // A diff that mentions the same name on multiple changed lines.
        let diff = "+fn foo() {}\n-fn foo() {}\n+fn foo() -> bool { true }\n";
        let ids = extract_identifiers(diff, 10);
        let count = ids.iter().filter(|s| s.as_str() == "foo").count();
        assert_eq!(count, 1, "duplicate identifiers must be deduplicated");
    }

    #[test]
    fn extract_identifiers_respects_limit() {
        let diff = (0..50)
            .map(|i| format!("+fn func{i}() {{}}\n"))
            .collect::<Vec<_>>()
            .join("");
        let ids = extract_identifiers(&diff, 5);
        assert_eq!(ids.len(), 5, "must respect max_identifiers cap");
    }

    #[test]
    fn extract_changed_files_from_diff() {
        let files = extract_changed_files(SAMPLE_DIFF);
        assert_eq!(files, vec!["src/auth.rs".to_string()]);
    }

    #[test]
    fn extract_changed_files_deduplicates() {
        let diff = "+++ b/src/a.rs\n+++ b/src/a.rs\n+++ b/src/b.rs\n";
        let files = extract_changed_files(diff);
        assert_eq!(files.len(), 2);
        assert!(files.contains(&"src/a.rs".to_string()));
        assert!(files.contains(&"src/b.rs".to_string()));
    }

    #[test]
    fn camel_case_detection() {
        assert!(is_camel_case("TokenCache"));
        assert!(is_camel_case("AuthError"));
        assert!(!is_camel_case("foo"));
        assert!(!is_camel_case("ALL_CAPS_CONST")); // no lowercase → false
        assert!(!is_camel_case("ab")); // too short
    }

    #[tokio::test]
    async fn local_diff_reads_file() {
        use std::io::Write as _;
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let content = "--- a/foo.rs\n+++ b/foo.rs\n+fn bar() {}\n";
        tmp.write_all(content.as_bytes()).expect("write");

        let source = DiffSource::LocalFile {
            path: tmp.path().to_path_buf(),
        };
        let result = load_diff(&source).await.expect("should read local file");
        assert_eq!(result, content);
    }

    /// Ensure load_diff returns an error when the local file does not exist.
    #[tokio::test]
    async fn local_diff_missing_file_returns_error() {
        use std::path::Path;
        let source = DiffSource::LocalFile {
            path: Path::new("/nonexistent/path/review.diff").to_path_buf(),
        };
        let result = load_diff(&source).await;
        assert!(
            result.is_err(),
            "missing local diff file must return an error"
        );
    }
}
