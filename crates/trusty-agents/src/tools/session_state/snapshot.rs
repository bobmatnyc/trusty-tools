//! `session_state_snapshot` — path-scoped read of a project's `.trusty-mpm/`
//! session artefacts (#4171, epic #4167).
//!
//! Why: the session STORE (see `store.rs`) records what a session is; the
//! per-project `.trusty-mpm/` directory records what it DID — the pane
//! scrollback, the last instructions it was handed, the session write-ups
//! under `.trusty-mpm/sessions/`. That is the "in-flight work tracking" half
//! of issue #4171, and reconciling a paused session against live git/PR/CI
//! needs it.
//!
//! SECURITY — WHY THIS IS NOT A FILE READER. A session-state tool that took a
//! model-supplied directory would be an arbitrary-filesystem read wearing a
//! session-state name: `project_dir = "/Users/x"` plus
//! `file = "../.ssh/id_ed25519"` and the black-box/L0 distinction stops
//! mattering. So the root is NOT a parameter. It is fixed at construction
//! from the dispatch path's own project root, and the ONLY model-supplied
//! input is a path RELATIVE to `<root>/.trusty-mpm/`, which
//! [`resolve_within_snapshot_dir`] confines with two independent checks:
//!
//! 1. **Syntactic.** Every component of the relative path must be
//!    `Component::Normal`. An absolute path, a drive prefix, a leading `/`,
//!    or any `..` anywhere is rejected before touching the filesystem.
//! 2. **Post-canonicalization containment.** The resolved target and the
//!    snapshot directory are BOTH canonicalized (which resolves symlinks) and
//!    the target must still be prefixed by the directory. This is what
//!    defeats a symlink planted inside `.trusty-mpm/` pointing at
//!    `~/.aws/credentials` — a check that only inspected the literal path
//!    would pass it.
//!
//! Reads are additionally refused for anything that is not a regular file and
//! are truncated to a byte ceiling, so a directory or a multi-gigabyte log
//! cannot be pulled into the turn.
//! Test: `snapshot_lists_directory_entries`, `snapshot_reads_a_named_file`,
//! `snapshot_rejects_parent_traversal`, `snapshot_rejects_absolute_path`,
//! `snapshot_rejects_symlink_escape`, `snapshot_rejects_a_directory_target`,
//! `snapshot_truncates_a_large_file`,
//! `snapshot_absent_directory_is_a_recoverable_error`,
//! `snapshot_takes_no_root_parameter`.

use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{ToolExecutor, ToolResult};

/// Name of the per-project session-artefact directory.
const SNAPSHOT_DIR: &str = ".trusty-mpm";

/// Bytes returned from a single snapshot file before truncation.
///
/// Why: `.trusty-mpm/scrollback.txt` is an unbounded pane dump and the session
/// write-ups can be long; the result goes straight into the next LLM request,
/// so the ceiling is a context-budget guard, not a filesystem one.
/// What: the default `max_bytes`, and its hard cap.
/// Test: `snapshot_truncates_a_large_file`.
const DEFAULT_MAX_BYTES: usize = 32 * 1024;

/// `session_state_snapshot` — list or read this project's `.trusty-mpm/`
/// session artefacts, read-only and path-scoped.
///
/// Why / What / security posture: see the module doc comment above — the root
/// is fixed at construction and the relative path is doubly confined.
/// L0-ONLY: constructed exclusively by `super::session_state_tools` for
/// `crate::agents::AgentTier::L0Orchestration`, and its name is stripped for
/// every other tier by `super::retain_tier_permitted`.
/// Test: see the module doc comment's Test line.
pub struct SessionStateSnapshotTool {
    /// The project root whose `.trusty-mpm/` subtree this tool may read. Set
    /// once at construction from the dispatch path's project root; never
    /// influenced by tool arguments.
    root: PathBuf,
}

impl SessionStateSnapshotTool {
    /// Bind the tool to one project root.
    ///
    /// Why: the root is a construction-time capability, not a parameter —
    /// that is the whole path-scoping story (see the module doc comment).
    /// What: stores `root`; the readable subtree is `root/.trusty-mpm`.
    /// Test: `snapshot_takes_no_root_parameter`.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The single directory this tool may read from.
    fn snapshot_dir(&self) -> PathBuf {
        self.root.join(SNAPSHOT_DIR)
    }
}

/// Confine a model-supplied relative path to `dir`.
///
/// Why: THE path-scoping enforcement point for this tool. See the module doc
/// comment for why both checks are needed and what each one defeats.
/// What: returns `Err` with a legible reason when `rel` is absolute, carries a
/// path prefix or root, contains any `..` or `.` component, or when the
/// canonicalized join escapes the canonicalized `dir` (the symlink case).
/// Returns the canonicalized target otherwise. `dir` itself must exist and
/// canonicalize; a missing snapshot directory is reported as such.
/// Test: `snapshot_rejects_parent_traversal`, `snapshot_rejects_absolute_path`,
/// `snapshot_rejects_symlink_escape`,
/// `snapshot_absent_directory_is_a_recoverable_error`,
/// `resolve_within_snapshot_dir_accepts_a_nested_normal_path`.
pub(super) fn resolve_within_snapshot_dir(dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel_path = Path::new(rel);
    for component in rel_path.components() {
        match component {
            Component::Normal(_) => {}
            other => {
                return Err(format!(
                    "refused: '{rel}' must be a plain relative path inside {SNAPSHOT_DIR}/ \
                     (rejected component {other:?})"
                ));
            }
        }
    }
    if rel_path.components().next().is_none() {
        return Err(format!("refused: empty path inside {SNAPSHOT_DIR}/"));
    }
    let base = dir.canonicalize().map_err(|e| {
        format!(
            "no session snapshots for this project: cannot open {} ({e})",
            dir.display()
        )
    })?;
    let target = base.join(rel_path).canonicalize().map_err(|e| {
        format!(
            "refused: cannot open '{rel}' inside {} ({e})",
            dir.display()
        )
    })?;
    if !target.starts_with(&base) {
        return Err(format!(
            "refused: '{rel}' resolves outside {SNAPSHOT_DIR}/ (symlink escape)"
        ));
    }
    Ok(target)
}

/// Render the snapshot directory's entries.
///
/// Why: the model needs to discover which artefacts exist before naming one,
/// and a listing is a cheaper first call than a blind read.
/// What: one `name  <size>  [dir|file]` line per entry, sorted by name,
/// non-recursive (a nested directory is named, not walked).
/// Test: `snapshot_lists_directory_entries`.
fn render_listing(dir: &Path) -> Result<String, String> {
    let mut rows: Vec<String> = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| {
        format!(
            "no session snapshots for this project: cannot list {} ({e})",
            dir.display()
        )
    })?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let (kind, size) = match entry.metadata() {
            Ok(m) if m.is_dir() => ("dir", 0),
            Ok(m) => ("file", m.len()),
            Err(_) => ("unknown", 0),
        };
        rows.push(format!("{name}  {size} bytes  [{kind}]"));
    }
    rows.sort();
    if rows.is_empty() {
        return Ok(format!("{} is empty\n", dir.display()));
    }
    Ok(format!(
        "{} entries in {}:\n{}\n",
        rows.len(),
        dir.display(),
        rows.join("\n")
    ))
}

#[async_trait]
impl ToolExecutor for SessionStateSnapshotTool {
    fn name(&self) -> &str {
        "session_state_snapshot"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "session_state_snapshot",
                "description": "List or read this project's own recorded session artefacts (pane scrollback, last instructions, session write-ups). Read-only, and confined to this project's session-artefact directory: there is no parameter for choosing another directory, and a relative path that tries to escape is refused. Call with no arguments to see what is available.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file": {
                            "type": "string",
                            "description": "Relative path of one artefact to read, as shown by a no-argument call (e.g. 'scrollback.txt' or 'sessions/session-20260727-123342.md'). Omit to list the directory."
                        },
                        "max_bytes": {
                            "type": "integer",
                            "description": "Bytes to return before truncating. Defaults to 32768, which is also the maximum.",
                            "minimum": 1
                        }
                    },
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let dir = self.snapshot_dir();
        let Some(rel) = args
            .get("file")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return match render_listing(&dir) {
                Ok(text) => ToolResult::ok(text),
                Err(e) => ToolResult::err(e),
            };
        };
        let target = match resolve_within_snapshot_dir(&dir, rel) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e),
        };
        match std::fs::metadata(&target) {
            Ok(m) if !m.is_file() => {
                return ToolResult::err(format!(
                    "refused: '{rel}' is not a regular file; call with no arguments to list the \
                     directory"
                ));
            }
            Err(e) => return ToolResult::err(format!("cannot stat '{rel}': {e}")),
            Ok(_) => {}
        }
        let max_bytes = args
            .get("max_bytes")
            .and_then(Value::as_u64)
            .map(|n| (n as usize).clamp(1, DEFAULT_MAX_BYTES))
            .unwrap_or(DEFAULT_MAX_BYTES);
        let bytes = match std::fs::read(&target) {
            Ok(b) => b,
            Err(e) => return ToolResult::err(format!("cannot read '{rel}': {e}")),
        };
        let total = bytes.len();
        let truncated = total > max_bytes;
        let head = if truncated {
            &bytes[..max_bytes]
        } else {
            &bytes[..]
        };
        let text = String::from_utf8_lossy(head);
        let mut out = format!("{rel} ({total} bytes");
        if truncated {
            out.push_str(&format!(", showing first {max_bytes}"));
        }
        out.push_str("):\n");
        out.push_str(&text);
        ToolResult::ok(out)
    }
}
