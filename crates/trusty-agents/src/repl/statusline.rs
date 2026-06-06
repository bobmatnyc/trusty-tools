//! Claude Code-style statusline configuration and rendering.
//!
//! Why: A bottom statusline lets the user see at-a-glance state — provider,
//! model, working directory, git branch, token spend — without parsing the
//! input bar or scrollback. Mirrors Claude Code's UX.
//! What: `StatuslineConfig` declares the segment ordering separately for
//! User vs. Project scope, loadable from `.trusty-agents/config.toml`. The render
//! helper composes the segments using current `ReplApp` state.
//! Test: `statusline_config_default_*`, `render_statusline_*` unit tests.

use std::path::Path;

use serde::Deserialize;

use super::tui::{AgentScope, ReplApp};

/// Per-scope segment ordering for the statusline.
///
/// Why: User scope (ctrl, persona) doesn't need provider/model details; project
/// scope wants the full routing context. Letting users override either list
/// from `.trusty-agents/config.toml` keeps the UI tunable.
/// What: Two `Vec<String>` lists naming segments. Recognized names:
/// `scope`, `provider`, `model`, `workdir`, `git`, `tokens`, `elapsed`.
/// Test: `statusline_config_default_user_segments`,
/// `statusline_config_default_project_segments`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StatuslineConfig {
    pub user_segments: Vec<String>,
    pub project_segments: Vec<String>,
}

impl Default for StatuslineConfig {
    fn default() -> Self {
        Self {
            user_segments: vec!["scope".to_string()],
            project_segments: vec![
                "provider".to_string(),
                "model".to_string(),
                "workdir".to_string(),
                "git".to_string(),
            ],
        }
    }
}

/// Top-level wrapper for `.trusty-agents/config.toml` parsing.
#[derive(Debug, Clone, Default, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    statusline: Option<StatuslineConfig>,
}

impl StatuslineConfig {
    /// Load from `<project>/.trusty-agents/config.toml` if present, otherwise default.
    ///
    /// Why: Users tune the statusline by dropping a TOML at project root; the
    /// REPL must not crash if the file is missing or malformed.
    /// What: Reads file, parses `[statusline]` table, returns default on any
    /// I/O or parse error.
    /// Test: `statusline_config_load_missing_file_returns_default`.
    pub fn load(project_dir: &Path) -> Self {
        let path = project_dir.join(".trusty-agents").join("config.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str::<ConfigFile>(&content) {
            Ok(cf) => cf.statusline.unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }
}

/// Compose the statusline string for the current `ReplApp` state.
///
/// Why: Centralizing the segment-rendering logic keeps the render path in
/// `tui.rs` simple — it just receives a `String` to draw.
/// What: Picks user/project segment list based on `app.agent_scope`, expands
/// each segment name to its current value, drops empty values, joins with
/// ` | `.
/// Test: `render_statusline_user_scope_shows_user`,
/// `render_statusline_project_scope_shows_segments`.
pub fn render_statusline(app: &ReplApp) -> String {
    let cfg = &app.statusline_config;
    let segments: &[String] = match app.agent_scope {
        AgentScope::User => &cfg.user_segments,
        AgentScope::Project => &cfg.project_segments,
    };
    let parts: Vec<String> = segments
        .iter()
        .filter_map(|name| segment_value(name.as_str(), app))
        .filter(|s| !s.is_empty())
        .collect();
    parts.join(" | ")
}

/// Resolve a single named segment to its rendered string value.
///
/// Why: Keeps each segment's render logic in one match arm so the table is
/// easy to extend (add a new arm + a default-list entry).
/// What: Returns `None` for unknown segments (silently dropped). Returns
/// `Some("")` for known segments with no current value (e.g. `git` outside a
/// repo) — these are filtered out by the caller.
/// Test: covered by `render_statusline_*` integration tests.
fn segment_value(name: &str, app: &ReplApp) -> Option<String> {
    match name {
        "scope" => Some(match app.agent_scope {
            AgentScope::User => "User".to_string(),
            AgentScope::Project => "Project".to_string(),
        }),
        "provider" => Some(app.provider_name.clone()),
        "model" => Some(app.model_name.clone()),
        "workdir" => {
            // Last path component for compactness.
            let p = Path::new(&app.working_dir);
            let basename = p
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&app.working_dir);
            Some(basename.to_string())
        }
        "git" => match &app.git_branch {
            Some(b) if !b.is_empty() => Some(if app.git_dirty {
                format!("{b}*")
            } else {
                b.clone()
            }),
            _ => Some(String::new()),
        },
        "tokens" => {
            if app.tokens_in > 0 || app.tokens_out > 0 {
                Some(format!("↑{} ↓{}", app.tokens_in, app.tokens_out))
            } else {
                Some(String::new())
            }
        }
        "elapsed" => Some(format_elapsed(app.session_start.elapsed())),
        _ => None,
    }
}

/// Format a `Duration` as `MM:SS` for the elapsed segment.
fn format_elapsed(d: std::time::Duration) -> String {
    let total = d.as_secs();
    let m = total / 60;
    let s = total % 60;
    format!("{:02}:{:02}", m, s)
}

/// Probe git for current branch and dirty state at startup.
///
/// Why: The statusline shows live git context; calling `git` once at REPL
/// boot keeps the render path off the critical I/O path.
/// What: Returns `(branch, dirty)`. Branch is `None` outside a repo; dirty is
/// true when `git status --porcelain` produces any output.
/// Test: Covered indirectly via REPL startup; pure subprocess I/O.
pub fn probe_git(project_dir: &Path) -> (Option<String>, bool) {
    let branch = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(project_dir)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty() && s != "HEAD")
            } else {
                None
            }
        });
    let dirty = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(project_dir)
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false);
    (branch, dirty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn fresh_app() -> ReplApp {
        ReplApp::new("ctrl".to_string(), "tester".to_string())
    }

    #[test]
    fn statusline_config_default_user_segments() {
        let c = StatuslineConfig::default();
        assert_eq!(c.user_segments, vec!["scope".to_string()]);
    }

    #[test]
    fn statusline_config_default_project_segments() {
        let c = StatuslineConfig::default();
        assert_eq!(
            c.project_segments,
            vec![
                "provider".to_string(),
                "model".to_string(),
                "workdir".to_string(),
                "git".to_string(),
            ]
        );
    }

    #[test]
    fn statusline_config_load_missing_file_returns_default() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = StatuslineConfig::load(tmp.path());
        assert_eq!(cfg.user_segments, StatuslineConfig::default().user_segments);
    }

    #[test]
    fn statusline_config_load_parses_user_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".trusty-agents")).unwrap();
        let toml = r#"
[statusline]
user_segments = ["scope", "elapsed"]
project_segments = ["model", "git"]
"#;
        std::fs::write(tmp.path().join(".trusty-agents/config.toml"), toml).unwrap();
        let cfg = StatuslineConfig::load(tmp.path());
        assert_eq!(cfg.user_segments, vec!["scope", "elapsed"]);
        assert_eq!(cfg.project_segments, vec!["model", "git"]);
    }

    #[test]
    fn render_statusline_user_scope_shows_user() {
        let app = fresh_app();
        let s = render_statusline(&app);
        assert_eq!(s, "User");
    }

    #[test]
    fn render_statusline_project_scope_shows_segments() {
        let mut app = fresh_app();
        app.agent_scope = AgentScope::Project;
        app.provider_name = "openrouter".to_string();
        app.model_name = "claude-sonnet-4-6".to_string();
        app.working_dir = "/Users/x/projects/trusty-agents".to_string();
        app.git_branch = Some("main".to_string());
        app.git_dirty = true;
        let s = render_statusline(&app);
        assert_eq!(s, "openrouter | claude-sonnet-4-6 | trusty-agents | main*");
    }

    #[test]
    fn render_statusline_drops_empty_git_segment() {
        let mut app = fresh_app();
        app.agent_scope = AgentScope::Project;
        app.provider_name = "openrouter".to_string();
        app.model_name = "m".to_string();
        app.working_dir = "/p".to_string();
        app.git_branch = None;
        let s = render_statusline(&app);
        // No "git" segment value → dropped.
        assert_eq!(s, "openrouter | m | p");
    }

    #[test]
    fn render_statusline_tokens_segment() {
        let mut app = fresh_app();
        app.statusline_config.user_segments = vec!["tokens".to_string()];
        app.tokens_in = 10;
        app.tokens_out = 5;
        let s = render_statusline(&app);
        assert_eq!(s, "↑10 ↓5");
    }

    #[test]
    fn format_elapsed_mm_ss() {
        assert_eq!(format_elapsed(std::time::Duration::from_secs(0)), "00:00");
        assert_eq!(format_elapsed(std::time::Duration::from_secs(65)), "01:05");
        assert_eq!(
            format_elapsed(std::time::Duration::from_secs(3661)),
            "61:01"
        );
    }

    #[test]
    fn probe_git_outside_repo_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let (branch, _dirty) = probe_git(tmp.path());
        assert!(
            branch.is_none(),
            "expected None outside a repo, got {branch:?}"
        );
    }

    #[test]
    fn session_start_used_for_elapsed_segment() {
        let mut app = fresh_app();
        app.statusline_config.user_segments = vec!["elapsed".to_string()];
        // Force a known session_start so output is deterministic.
        app.session_start = Instant::now() - std::time::Duration::from_secs(125);
        let s = render_statusline(&app);
        // Should be 02:05 ± a tick.
        assert!(s.starts_with("02:0"), "unexpected elapsed: {s}");
    }
}
