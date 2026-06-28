//! `tm statusline` — emit one status-bar line for Claude Code's `statusLine` hook.
//!
//! Why: Claude Code's `statusLine` hook fires on every render cycle and calls this
//! command; this handler parses the hook JSON from stdin and emits a compact
//! ` │ `-separated segment string on stdout. All fields degrade gracefully.
//! What: reads a JSON object from stdin, renders repo+branch/model/daemon/cost
//! segments, and exits 0. Missing or invalid fields produce empty/omitted segments.
//! Test: `render_statusline_minimal_input`, `render_statusline_full_payload` in tests.

pub(crate) mod compaction;

use std::io::Read as _;

use crate::formatters::info_box::DaemonInfo;
use compaction::{ContextWindow, compaction_segment};

/// Claude Code `statusLine` hook input (all fields optional via `#[serde(default)]`).
///
/// Why: Claude Code may add fields in future versions; `deny_unknown_fields` is
/// intentionally absent so new keys do not cause parse failures.
/// What: cwd, model metadata, cost summary, context-window data, and the
/// context-window overflow flag.
/// Test: `render_statusline_minimal_input` (empty input), `render_statusline_full_payload`.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct StatusInput {
    #[serde(default)]
    pub(crate) session_id: String,
    #[serde(default)]
    pub(crate) cwd: String,
    #[serde(default)]
    pub(crate) model: ModelInfo,
    #[serde(default)]
    pub(crate) cost: CostInfo,
    #[serde(default)]
    pub(crate) exceeds_200k_tokens: bool,
    #[serde(default)]
    pub(crate) context_window: Option<ContextWindow>,
}

/// Model metadata from the Claude Code hook input.
///
/// Why: `display_name` carries the human-readable label ("Opus") operators want
/// in the status bar; `id` is the fallback when `display_name` is absent.
/// What: both fields are `String` with `#[serde(default)]` so absent keys are "".
/// Test: `render_statusline_full_payload`.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct ModelInfo {
    #[serde(default)]
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) display_name: String,
}

/// Cost accumulator from the Claude Code hook input.
///
/// Why: operators want running cost in the status bar to monitor spend.
/// What: `total_cost_usd` is the session's accumulated cost; 0.0 when absent.
/// Test: `render_statusline_full_payload`.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct CostInfo {
    #[serde(default)]
    pub(crate) total_cost_usd: f64,
}

/// Read Claude Code's `statusLine` JSON from stdin and print one compact line.
///
/// Why: Claude Code's `statusLine` hook protocol is "command reads JSON from
/// stdin, prints one line to stdout, exits 0"; this is the single entry point.
/// What: reads all of stdin, parses it as `StatusInput` (empty/invalid → default),
/// renders segments, and prints exactly one line to stdout.
/// Test: `render_statusline_minimal_input`, `render_statusline_full_payload`.
pub(crate) fn run_statusline() -> anyhow::Result<()> {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let input: StatusInput = serde_json::from_str(&raw).unwrap_or_default();
    println!("{}", render_statusline(&input));
    Ok(())
}

/// Render the compact statusline string from parsed hook input.
///
/// Why: keeping rendering pure (no mandatory I/O) makes it unit-testable
/// independently of the stdin protocol.
/// What: builds up to seven segments — project+branch, model, daemon, session
/// count, context%, compaction efficiency, cost — joined with ` │ `, emitting
/// only non-empty segments.
/// Test: `render_statusline_minimal_input`, `render_statusline_full_payload`.
pub(crate) fn render_statusline(input: &StatusInput) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(seg) = project_segment(&input.cwd) {
        parts.push(seg);
    }
    if let Some(seg) = model_segment(&input.model) {
        parts.push(seg);
    }

    // Probe daemon info: lock file (cheap) + optional session-count HTTP probe.
    let daemon = {
        let d = DaemonInfo::from_lock_file();
        if d.online {
            let url = daemon_base_url(&d);
            d.probe_session_count(&url)
        } else {
            d
        }
    };
    parts.push(daemon_segment(&daemon));
    if let Some(n) = daemon.session_count {
        parts.push(count_segment(n));
    }

    if input.exceeds_200k_tokens {
        parts.push("ctx>200k".to_string());
    }

    // Compaction efficiency / live context fill.
    if let Some(seg) = compaction_segment(&input.session_id, input.context_window.as_ref()) {
        parts.push(seg);
    }

    if input.cost.total_cost_usd > 0.0 {
        parts.push(cost_segment(input.cost.total_cost_usd));
    }

    parts.join(" \u{2502} ")
}

// ── Segment builders ──────────────────────────────────────────────────────────

/// Build the project-name + git-branch segment from `cwd`.
///
/// Why: shows the project name and current branch without requiring a running
/// daemon; a subprocess git call is cheap enough for a status-bar refresh.
/// What: extracts the dirname basename and appends ` ⎇ <branch>` when the
/// working directory is inside a git repo.
/// Test: `render_statusline_minimal_input` (empty cwd → None).
fn project_segment(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    let project = std::path::Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if project.is_empty() {
        return None;
    }
    let branch = git_branch(cwd);
    Some(match branch {
        Some(b) if !b.is_empty() && b != "HEAD" => {
            format!("{project}  \u{2387} {b}")
        }
        _ => project,
    })
}

/// Build the model-label segment.
///
/// Why: operators want to know which model Claude Code is using at a glance.
/// What: returns `display_name` when non-empty, `id` as fallback, `None` when
/// both are empty (segment omitted from the status bar).
/// Test: `render_statusline_full_payload`.
fn model_segment(model: &ModelInfo) -> Option<String> {
    if !model.display_name.is_empty() {
        Some(model.display_name.clone())
    } else if !model.id.is_empty() {
        Some(model.id.clone())
    } else {
        None
    }
}

/// Build the daemon online/offline segment (`tm ●:<port>` or `tm ○`).
///
/// Why: makes the daemon state immediately visible without opening a terminal.
/// What: extracts the port from `daemon.addr` and returns `"tm ●:<port>"` or
/// `"tm ○"`.
/// Test: `render_statusline_minimal_input` (offline daemon → `tm ○`).
fn daemon_segment(daemon: &DaemonInfo) -> String {
    if daemon.online {
        let port = daemon
            .addr
            .rfind(':')
            .map(|i| &daemon.addr[i..])
            .unwrap_or(daemon.addr.as_str());
        format!("tm \u{25cf}{port}")
    } else {
        "tm \u{25cb}".to_string()
    }
}

/// Build the session-count segment (`⛁N`).
///
/// Why: shows at a glance how many trusty-mpm sessions are active.
/// What: returns `"⛁{n}"`.
/// Test: `render_statusline_full_payload`.
fn count_segment(n: usize) -> String {
    format!("\u{26c1}{n}")
}

/// Build the cost segment (`$X.XX`).
///
/// Why: running cost visibility helps operators monitor API spend.
/// What: formats `usd` to two decimal places with a `$` prefix.
/// Test: `render_statusline_full_payload`.
fn cost_segment(usd: f64) -> String {
    format!("${usd:.2}")
}

/// Construct the HTTP base URL for the daemon from `DaemonInfo.addr`.
///
/// Why: the lock file stores `addr = "127.0.0.1:7880"` without the `http://`
/// scheme; the probe needs a full URL.
/// What: prepends `http://` when the addr does not already start with it.
/// Test: covered indirectly by `render_statusline_minimal_input`.
fn daemon_base_url(daemon: &DaemonInfo) -> String {
    if daemon.addr.starts_with("http") {
        daemon.addr.clone()
    } else {
        format!("http://{}", daemon.addr)
    }
}

/// Run `git rev-parse --abbrev-ref HEAD` in `cwd` with a hard 100 ms wall-clock
/// timeout and return the branch name.
///
/// Why: `statusline` is on Claude Code's hot render path; a stuck credential
/// helper, network filesystem, or git hook would otherwise block every render
/// cycle. The bounded thread pattern matches `try_get_session_count`.
/// What: spawns a detached thread that calls `git rev-parse`, sends the result
/// over an mpsc channel; the caller waits ≤100 ms, returns `None` on timeout.
/// Test: `render_statusline_full_payload` exercises the detected branch path;
/// `render_statusline_minimal_input` covers the empty-cwd (None) path.
fn git_branch(cwd: &str) -> Option<String> {
    use std::sync::mpsc;
    use std::time::Duration;

    let cwd = cwd.to_string();
    let (tx, rx) = mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        let result = (|| -> Option<String> {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&cwd)
                .args(["rev-parse", "--abbrev-ref", "HEAD"])
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if branch.is_empty() {
                None
            } else {
                Some(branch)
            }
        })();
        let _ = tx.send(result);
    });
    rx.recv_timeout(Duration::from_millis(100)).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use compaction::ContextWindow;

    fn full_input() -> StatusInput {
        StatusInput {
            session_id: String::new(),
            cwd: "/home/user/my-project".to_string(),
            model: ModelInfo {
                id: "claude-sonnet-4-6".to_string(),
                display_name: "Claude Sonnet 4.6".to_string(),
            },
            cost: CostInfo {
                total_cost_usd: 1.23,
            },
            exceeds_200k_tokens: false,
            context_window: None,
        }
    }

    #[test]
    fn render_statusline_minimal_input() {
        // Empty StatusInput (all defaults) must not panic; daemon segment always present.
        let out = render_statusline(&StatusInput::default());
        assert!(out.contains("tm "), "daemon segment must always appear");
        assert!(!out.is_empty(), "output must not be empty");
    }

    #[test]
    fn render_statusline_full_payload() {
        // Full payload shows model name, cost; project segment when cwd is set.
        let input = full_input();
        let out = render_statusline(&input);
        assert!(
            out.contains("Claude Sonnet 4.6"),
            "model display name must appear"
        );
        assert!(out.contains("$1.23"), "cost must appear");
        assert!(
            out.contains("my-project"),
            "project name from cwd must appear"
        );
    }

    #[test]
    fn render_statusline_missing_model_omits_segment() {
        // When both model.id and model.display_name are empty, no model segment.
        let mut input = full_input();
        input.model = ModelInfo::default();
        let out = render_statusline(&input);
        assert!(!out.contains("claude"), "model segment must be omitted");
        assert!(out.contains("tm "), "daemon segment must still appear");
        assert!(out.contains("$1.23"), "cost must still appear");
    }

    #[test]
    fn render_statusline_exceeds_200k_shows_ctx_segment() {
        let mut input = full_input();
        input.exceeds_200k_tokens = true;
        let out = render_statusline(&input);
        assert!(out.contains("ctx>200k"), "ctx>200k segment must appear");
    }

    #[test]
    fn render_statusline_zero_cost_omits_cost_segment() {
        let mut input = full_input();
        input.cost.total_cost_usd = 0.0;
        let out = render_statusline(&input);
        assert!(!out.contains('$'), "cost segment must be omitted when zero");
    }

    #[test]
    fn render_statusline_invalid_json_falls_back_gracefully() {
        // Simulates invalid/empty JSON by using StatusInput::default() — same path
        // as run_statusline's unwrap_or_default on parse failure.
        let out = render_statusline(&StatusInput::default());
        assert!(
            out.contains("tm "),
            "graceful fallback must still emit daemon segment"
        );
        assert!(
            !out.contains("│ │"),
            "must not produce empty separator pairs"
        );
    }

    #[test]
    fn render_statusline_no_empty_separator_pairs() {
        // With every optional field empty, the only segment is the daemon row.
        let out = render_statusline(&StatusInput::default());
        assert!(!out.contains(" \u{2502}  \u{2502} "), "no empty │ │ pairs");
        assert!(!out.contains("│ │"), "no adjacent separators");
    }

    #[test]
    fn render_statusline_with_context_window_shows_live_fill() {
        // When session_id is empty, compaction_segment returns None; ctx% is skipped.
        // When session_id is present and no state file exists, shows live fill.
        // We test via render_compaction_segment directly to avoid real file I/O.
        use compaction::{CompactionState, render_compaction_segment};

        let state = CompactionState::default();
        let cw = ContextWindow {
            total_input_tokens: 82_000,
            context_window_size: 200_000,
            used_percentage: 41.0,
        };
        let seg = render_compaction_segment(&state, &cw);
        assert_eq!(seg.as_deref(), Some("ctx 41%"));
    }

    #[test]
    fn render_statusline_with_context_window_no_session_id_omits_segment() {
        // session_id="" → compaction_segment() → None → no compaction segment
        let mut input = full_input();
        input.context_window = Some(ContextWindow {
            total_input_tokens: 82_000,
            context_window_size: 200_000,
            used_percentage: 41.0,
        });
        let out = render_statusline(&input);
        // No compaction/ctx segment because session_id is empty
        assert!(!out.contains("ctx "), "no ctx segment without session_id");
    }
}
