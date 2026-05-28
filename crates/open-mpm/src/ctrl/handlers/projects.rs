//! Project-registry CTRL tools and stack-detection helpers.
//!
//! Why: Lets the user / LLM manage `~/.open-mpm/projects.json` from the REPL
//! without editing JSON by hand, and lets `SetActiveProjectTool` pin a default
//! for path-defaulting tools like `start_pm`.
//! What: `AddProjectTool`, `RemoveProjectTool`, `ListProjectsTool`,
//! `SetActiveProjectTool`, plus `detect_stack` / `is_empty_project` helpers.
//! Test: `add_project_tool_validates_path`, `set_active_project_*`,
//! `detect_stack_*`, `is_empty_project_*`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::registry::ProjectRegistry;
use crate::tools::traits::{ToolExecutor, ToolResult};

/// `list_projects()` — dump active entries from ~/.open-mpm/projects.json.
pub(crate) struct ListProjectsTool;

#[async_trait]
impl ToolExecutor for ListProjectsTool {
    fn name(&self) -> &str {
        "list_projects"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "list_projects",
                "description": "List projects CTRL has connected to, with last_connected and pm_count.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, _args: Value) -> ToolResult {
        match ProjectRegistry::new() {
            Ok(reg) => match reg.list_active().await {
                Ok(entries) => match serde_json::to_string(&entries) {
                    Ok(s) => ToolResult::ok(s),
                    Err(e) => ToolResult::err(format!("list_projects: serialize: {e}")),
                },
                Err(e) => ToolResult::err(format!("list_projects: {e:#}")),
            },
            Err(e) => ToolResult::err(format!("list_projects: registry unavailable: {e:#}")),
        }
    }
}

/// `add_project(path)` — register a project in `~/.open-mpm/projects.json`.
/// (#202)
///
/// Why: Lets the user (or LLM) bring a directory under CTRL management
/// without having to launch a PM first. Mirrors the implicit registration
/// performed by `Ctrl::connect`, but as a standalone, idempotent action so
/// `list_projects` can show it before any work begins.
/// What: Validates that `path` exists and is a directory, then calls
/// `ProjectRegistry::register_pm_start` (the same path used during
/// `connect`) so the entry's metadata stays consistent with normal use.
/// Test: `add_project_tool_validates_path`.
pub(crate) struct AddProjectTool;

#[async_trait]
impl ToolExecutor for AddProjectTool {
    fn name(&self) -> &str {
        "add_project"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "add_project",
                "description": "Register a project directory in ~/.open-mpm/projects.json so it appears in list_projects.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path to the project directory" }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(raw) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("add_project: missing 'path'");
        };
        let path = match PathBuf::from(raw).canonicalize() {
            Ok(p) => p,
            Err(e) => return ToolResult::err(format!("add_project: cannot resolve {raw}: {e}")),
        };
        if !path.is_dir() {
            return ToolResult::err(format!("add_project: not a directory: {}", path.display()));
        }
        let stack = detect_stack(&path);
        let status = if is_empty_project(&path) {
            "new"
        } else {
            "existing"
        };
        match ProjectRegistry::new() {
            Ok(reg) => match reg.register_pm_start(&path).await {
                Ok(()) => ToolResult::ok(format!(
                    "Project registered: {} (stack: {}, status: {})",
                    path.display(),
                    stack,
                    status
                )),
                Err(e) => ToolResult::err(format!("add_project: {e:#}")),
            },
            Err(e) => ToolResult::err(format!("add_project: registry unavailable: {e:#}")),
        }
    }
}

/// `remove_project(path)` — drop an entry from `~/.open-mpm/projects.json`.
/// (#202)
///
/// Why: Lets the user clean up the registry without editing JSON by hand.
/// Does NOT touch any running PM session — that's the job of `stop_task`.
/// What: Calls `ProjectRegistry::remove`, which removes the canonical-path
/// keyed entry and saves atomically.
/// Test: covered indirectly via `add_project_tool_validates_path` plus
/// `ProjectRegistry::remove` round-trip in registry tests.
pub(crate) struct RemoveProjectTool;

#[async_trait]
impl ToolExecutor for RemoveProjectTool {
    fn name(&self) -> &str {
        "remove_project"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "remove_project",
                "description": "Remove a project entry from ~/.open-mpm/projects.json. Does not stop running PM sessions.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path of the project to remove" }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(raw) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("remove_project: missing 'path'");
        };
        // Try to canonicalize, but fall back to the literal path so users
        // can remove entries whose directories have already been deleted.
        let path = PathBuf::from(raw)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(raw));
        match ProjectRegistry::new() {
            Ok(reg) => match reg.remove(&path).await {
                Ok(true) => ToolResult::ok(format!("Project removed: {}", path.display())),
                Ok(false) => ToolResult::ok(format!("Project not found: {}", path.display())),
                Err(e) => ToolResult::err(format!("remove_project: {e:#}")),
            },
            Err(e) => ToolResult::err(format!("remove_project: registry unavailable: {e:#}")),
        }
    }
}

/// `set_active_project(path)` — change CTRL's default project for `start_pm`.
/// (#202)
///
/// Why: Lets a user pin a project once and then invoke `start_pm` (or other
/// path-defaulting tools) without repeating the path on every turn.
/// What: Validates that `path` exists, then writes it to the shared
/// `active_project` slot held by `Ctrl`. `StartPmTool` reads the same slot.
/// Test: `set_active_project_updates_slot`.
pub(crate) struct SetActiveProjectTool {
    pub(crate) active_project: Arc<Mutex<Option<PathBuf>>>,
}

#[async_trait]
impl ToolExecutor for SetActiveProjectTool {
    fn name(&self) -> &str {
        "set_active_project"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "set_active_project",
                "description": "Set the active project path used as a default for start_pm and similar tools.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path to set as the active project" }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(raw) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("set_active_project: missing 'path'");
        };
        let path = match PathBuf::from(raw).canonicalize() {
            Ok(p) => p,
            Err(e) => {
                return ToolResult::err(format!("set_active_project: cannot resolve {raw}: {e}"));
            }
        };
        if !path.is_dir() {
            return ToolResult::err(format!(
                "set_active_project: not a directory: {}",
                path.display()
            ));
        }
        match self.active_project.lock() {
            Ok(mut slot) => {
                *slot = Some(path.clone());
                ToolResult::ok(format!("Active project set to: {}", path.display()))
            }
            Err(e) => ToolResult::err(format!("set_active_project: lock poisoned: {e}")),
        }
    }
}

/// Detect the primary tech stack of a project by checking indicator files.
///
/// Why: When CTRL learns about a project, surfacing stack identity (Rust /
/// Python / Node / Go / etc.) lets it pick the right engineer agent without
/// asking the user. Returning `"unknown"` keeps the caller's flow simple.
/// What: Walks a list of `(filename, stack-label)` pairs; supports `*.ext`
/// glob entries via `read_dir`. First match wins.
/// Test: `detect_stack_finds_rust`, `detect_stack_returns_unknown`.
pub(crate) fn detect_stack(project_path: &Path) -> String {
    let indicators: &[(&str, &str)] = &[
        ("Cargo.toml", "Rust"),
        ("go.mod", "Go"),
        ("pom.xml", "Java (Maven)"),
        ("build.gradle", "Java/Kotlin (Gradle)"),
        ("build.gradle.kts", "Java/Kotlin (Gradle)"),
        ("pyproject.toml", "Python"),
        ("setup.py", "Python"),
        ("package.json", "Node.js/TypeScript"),
        ("Gemfile", "Ruby"),
        ("mix.exs", "Elixir"),
        ("composer.json", "PHP"),
        ("*.csproj", "C#/.NET"),
    ];
    for (file, stack) in indicators {
        if file.contains('*') {
            if let Ok(entries) = std::fs::read_dir(project_path) {
                let ext = file.trim_start_matches("*.");
                if entries
                    .flatten()
                    .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some(ext))
                {
                    return (*stack).to_string();
                }
            }
        } else if project_path.join(file).exists() {
            return (*stack).to_string();
        }
    }
    "unknown".to_string()
}

/// Heuristic: is the project directory effectively empty (a "new" project)?
///
/// Why: When a user adds a project, CTRL should distinguish "scaffold a new
/// project here" from "index this existing codebase". A directory with no
/// non-hidden entries is treated as new.
/// What: Returns true when `read_dir` yields no entries whose name does not
/// start with '.'. On read failure, returns false (treat as existing).
/// Test: covered by `add_project_tool_validates_path` indirectly.
pub(crate) fn is_empty_project(project_path: &Path) -> bool {
    match std::fs::read_dir(project_path) {
        Ok(entries) => !entries.flatten().any(|e| {
            e.file_name()
                .to_str()
                .map(|n| !n.starts_with('.'))
                .unwrap_or(false)
        }),
        Err(_) => false,
    }
}
