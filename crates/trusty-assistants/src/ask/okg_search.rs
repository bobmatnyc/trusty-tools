//! `okg_search`: a read-only text search rooted at one instance's OKG (#11650).
//!
//! Why: the ask engine's registry holds MCP tools plus a search over the
//! instance's own OKG tree, and nothing else. trusty-code's `search_code`
//! picks an ancestor index when the OKG has none, so it is not rooted.
//! What: [`OkgSearchTool`] walks `<home>/okg` without following symlinks and
//! returns `path:line: text` for each line containing the query,
//! case-insensitively. It reads at most [`MAX_FILES_SCANNED`] files and says
//! when it stopped early; a path it could not walk or read is reported, and
//! with no hits that is an error, not "no matches" (#11650). It reads only;
//! it never writes, spawns or connects.
//! Test: `super::tests::tools_tests::okg_search_finds_lines_under_the_okg_only`,
//! `super::tests::tools_tests::okg_search_rejects_an_empty_query`,
//! `super::tests::tools_tests::okg_search_reports_unreadable_paths`,
//! `super::tests::tools_tests::okg_search_stops_at_the_scan_budget`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};
use trusty_code::tools::{ToolExecutor, ToolResult};

/// The tool's registered name.
pub const OKG_SEARCH_TOOL_NAME: &str = "okg_search";

/// Matches returned when the model does not ask for a number.
const DEFAULT_MAX_RESULTS: usize = 20;

/// Hard cap on returned matches.
const MAX_RESULTS: usize = 50;

/// Files over this size are skipped.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Characters kept from one matching line.
const MAX_LINE_CHARS: usize = 300;

/// Most files one search reads (#11650). The walk stops here and says so.
/// Test: `super::tests::tools_tests::okg_search_stops_at_the_scan_budget`.
pub const MAX_FILES_SCANNED: usize = 2_000;

/// Unreadable paths named in a result; the rest are only counted.
const MAX_ERRORS_SHOWN: usize = 5;

/// File extensions searched: the OKG tree is markdown.
const EXTENSIONS: &[&str] = &["md", "markdown", "txt"];

/// Search over one instance's OKG directory.
/// Test: `super::tests::tools_tests::okg_search_finds_lines_under_the_okg_only`.
pub struct OkgSearchTool {
    root: PathBuf,
}

impl OkgSearchTool {
    /// A tool rooted at `okg_dir`. Touches no filesystem.
    pub fn new(okg_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: okg_dir.into(),
        }
    }
}

#[async_trait]
impl ToolExecutor for OkgSearchTool {
    fn name(&self) -> &str {
        OKG_SEARCH_TOOL_NAME
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": OKG_SEARCH_TOOL_NAME,
                "description": "Search this assistant's own knowledge graph (OKG) for lines \
                                containing a phrase, case-insensitively. Read-only.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Text to look for." },
                        "max_results": {
                            "type": "integer",
                            "description": "Most matches to return (default 20, at most 50)."
                        }
                    },
                    "required": ["query"]
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_lowercase();
        if query.is_empty() {
            return ToolResult::err("okg_search needs a non-empty `query`");
        }
        let limit = args
            .get("max_results")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_MAX_RESULTS, |n| (n as usize).clamp(1, MAX_RESULTS));
        if !self.root.is_dir() {
            return ToolResult::err(format!(
                "this assistant's OKG directory `{}` does not exist",
                self.root.display()
            ));
        }
        let root = self.root.clone();
        match tokio::task::spawn_blocking(move || search(&root, &query, limit, MAX_FILES_SCANNED))
            .await
        {
            Ok(scan) => scan.into_result(),
            Err(e) => ToolResult::err(format!("okg_search failed: {e}")),
        }
    }
}

/// What one search found, and what it could not look at.
/// Test: `super::tests::tools_tests::okg_search_reports_unreadable_paths`,
/// `super::tests::tools_tests::okg_search_stops_at_the_scan_budget`.
#[derive(Debug, Default)]
struct Scan {
    /// `relpath:line: text` matches.
    hits: Vec<String>,
    /// Files read.
    scanned: usize,
    /// The file budget ran out with files left unread.
    budget_hit: bool,
    /// Files skipped for being over [`MAX_FILE_BYTES`].
    oversize: usize,
    /// How many paths could not be walked, statted or read as UTF-8.
    unreadable: usize,
    /// The first few of those, as `path: error`.
    unreadable_shown: Vec<String>,
}

impl Scan {
    fn unreadable(&mut self, what: String) {
        self.unreadable += 1;
        if self.unreadable_shown.len() < MAX_ERRORS_SHOWN {
            self.unreadable_shown.push(what);
        }
    }

    /// The tool result. No hits plus an unreadable path is an error, never
    /// "no matches" (#11650); every other gap is stated in a note.
    fn into_result(self) -> ToolResult {
        let mut notes = Vec::new();
        if self.budget_hit {
            notes.push(format!(
                "note: the search stopped after {} files (the scan budget); \
                 files after that were not searched",
                self.scanned
            ));
        }
        if self.oversize > 0 {
            notes.push(format!(
                "note: {} files over {} bytes were not searched",
                self.oversize, MAX_FILE_BYTES
            ));
        }
        let unreadable = (self.unreadable > 0).then(|| {
            format!(
                "{} paths in this assistant's OKG could not be read: {}",
                self.unreadable,
                self.unreadable_shown.join("; ")
            )
        });
        if self.hits.is_empty() {
            if let Some(unreadable) = unreadable {
                notes.insert(0, format!("okg_search found no matches, but {unreadable}"));
                return ToolResult::err(notes.join("\n"));
            }
            notes.insert(0, "no matches in this assistant's OKG".to_string());
            return ToolResult::ok(notes.join("\n"));
        }
        if let Some(unreadable) = unreadable {
            notes.push(format!("note: {unreadable}"));
        }
        let mut out = self.hits;
        out.extend(notes);
        ToolResult::ok(out.join("\n"))
    }
}

/// Up to `limit` hits for `query` (already lowercased), reading at most
/// `budget` files.
fn search(root: &Path, query: &str, limit: usize, budget: usize) -> Scan {
    let mut scan = Scan::default();
    let rel = |path: &Path| {
        path.strip_prefix(root)
            .unwrap_or(path)
            .display()
            .to_string()
    };
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
    {
        // #11650: a walk error is reported, not read as "no matches".
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                let path = e.path().map_or_else(|| "?".to_string(), rel);
                scan.unreadable(format!("{path}: {e}"));
                continue;
            }
        };
        let searched = entry.file_type().is_file()
            && entry
                .path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| EXTENSIONS.contains(&x));
        if !searched {
            continue;
        }
        let path = entry.path();
        match entry.metadata() {
            Ok(m) if m.len() > MAX_FILE_BYTES => {
                scan.oversize += 1;
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                scan.unreadable(format!("{}: {e}", rel(path)));
                continue;
            }
        }
        // #11650: bound the files read, not only their size and the hits.
        if scan.scanned == budget {
            scan.budget_hit = true;
            break;
        }
        scan.scanned += 1;
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                scan.unreadable(format!("{}: {e}", rel(path)));
                continue;
            }
        };
        for (n, line) in text.lines().enumerate() {
            if line.to_lowercase().contains(query) {
                let shown: String = line.trim().chars().take(MAX_LINE_CHARS).collect();
                scan.hits.push(format!("{}:{}: {shown}", rel(path), n + 1));
                if scan.hits.len() >= limit {
                    return scan;
                }
            }
        }
    }
    scan
}
