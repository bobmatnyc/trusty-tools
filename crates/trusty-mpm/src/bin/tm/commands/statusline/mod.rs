//! `tm statusline` — emit one status-bar line for Claude Code's `statusLine` hook.
//!
//! Why: Claude Code's `statusLine` hook fires on every render cycle and calls this
//! command; this handler parses the hook JSON from stdin and emits a compact
//! ` | `-separated segment string on stdout. All fields degrade gracefully.
//! What: reads a JSON object from stdin and renders the fixed-order layout
//! `TM <ver> <port> | <project> ⎇ <branch> | @<gh> | <model> | ctx% | <cost>`
//! (#2011) to stdout. Missing or invalid fields produce empty/omitted segments;
//! nothing ever panics or blocks the render path.
//! Test: `render_statusline_minimal_input`, `render_statusline_full_payload`,
//! `render_statusline_full_payload_matches_pipe_format` in tests.

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
/// Why: keeping rendering pure aside from bounded, timeout-guarded probes makes
/// the layout unit-testable and keeps Claude Code's hot render path fast (#2011:
/// the prior blocking-I/O + path-traversal bug means every probe here must stay
/// non-blocking and infallible).
/// What: builds the fixed-order layout `TM <ver> <port> | <project> ⎇ <branch>
/// | @<gh> | <model> | ctx% | <cost>`, joined with ` | `. The leading `TM <ver>`
/// segment is always present; every other segment is omitted (not left blank)
/// when its data source is unavailable, so the separators never double up.
/// Test: `render_statusline_minimal_input`, `render_statusline_full_payload`,
/// `render_statusline_full_payload_matches_pipe_format`.
pub(crate) fn render_statusline(input: &StatusInput) -> String {
    let mut parts: Vec<String> = Vec::new();

    // Lock-file read only — cheap and non-blocking; no HTTP session-count probe
    // is needed since the reformatted layout (#2011) no longer surfaces a count.
    let daemon = DaemonInfo::from_lock_file();
    parts.push(version_port_segment(&daemon));

    if let Some(seg) = project_segment(&input.cwd) {
        parts.push(seg);
    }
    if let Some(seg) = gh_account_segment_probe() {
        parts.push(seg);
    }
    if let Some(seg) = model_segment(&input.model) {
        parts.push(seg);
    }

    // Compaction efficiency / live context fill; falls back to a bare
    // `ctx>200k` marker when no context-window payload was sent at all.
    if let Some(seg) = compaction_segment(&input.session_id, input.context_window.as_ref()) {
        parts.push(seg);
    } else if input.exceeds_200k_tokens {
        parts.push("ctx>200k".to_string());
    }

    if input.cost.total_cost_usd > 0.0 {
        parts.push(cost_segment(input.cost.total_cost_usd));
    }

    parts.join(" | ")
}

/// Build the leading `TM <ver> <port>` segment.
///
/// Why (#2011): the statusline reformat replaces the old `tm ●:<port>` /
/// `tm ○` online-indicator glyphs with a plain-text `TM <ver> <port>` lead-in;
/// the version always renders (it is a compile-time constant) so the segment
/// degrades gracefully to just `TM <ver>` when the daemon is offline or its
/// port cannot be parsed, rather than showing a stale or misleading port.
/// What: reads `env!("CARGO_PKG_VERSION")` and appends the port parsed from
/// `daemon.addr` (`host:port` → `port`) only when `daemon.online` is true and
/// a port is present.
/// Test: `version_port_segment_online_includes_port`,
/// `version_port_segment_offline_omits_port`.
fn version_port_segment(daemon: &DaemonInfo) -> String {
    let ver = env!("CARGO_PKG_VERSION");
    match daemon_port(daemon) {
        Some(port) if daemon.online => format!("TM {ver} {port}"),
        _ => format!("TM {ver}"),
    }
}

/// Extract the bare port substring from a `host:port` daemon address.
///
/// Why: centralises the `rfind(':')` parse so `version_port_segment` stays
/// focused on assembly rather than string surgery.
/// What: returns everything after the last `:`, or `None` when `addr` has no
/// colon (e.g. empty/unset).
/// Test: `version_port_segment_online_includes_port` (via the parsed port).
fn daemon_port(daemon: &DaemonInfo) -> Option<&str> {
    daemon.addr.rfind(':').map(|i| &daemon.addr[i + 1..])
}

// ── Segment builders ──────────────────────────────────────────────────────────

/// Build the project-identity + git-branch segment from `cwd`.
///
/// Why (#1913-followup): a managed tm session runs in `.worktrees/<uuid>/`, so the
/// cwd basename is just the session UUID — useless as a project label and a
/// duplicate of the `session/<uuid>` branch. The leading field should instead be
/// the GitHub `owner/repo` slug (the codebase's canonical `source_id`), and the
/// redundant `session/<…>` branch is dropped so the operator can actually tell
/// which project and branch they are on.
/// What: derives `owner/repo` from the git remote (reusing
/// [`trusty_common::github_path::derive_github_path`] — the same parser
/// `derive_source_id_for_record` uses; handles https and scp-style remotes),
/// falling back to the cwd basename only when there is no parseable GitHub remote.
/// Appends ` ⎇ <branch>` for meaningful branches, but omits it for the auto-created
/// `session/<…>` managed-session branch and for detached `HEAD`.
/// Test: `project_segment_basename_fallback_non_git`, `git_owner_repo_reads_remote`,
/// `render_project_segment_*`, `render_statusline_minimal_input` (empty cwd → None).
fn project_segment(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    // Prefer the GitHub owner/repo slug from the git remote; fall back to the cwd
    // basename only when there is no parseable remote (non-GitHub / non-git dir).
    let project = git_owner_repo(cwd).unwrap_or_else(|| {
        std::path::Path::new(cwd)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    });
    render_project_segment(project, git_branch(cwd).as_deref())
}

/// Assemble the project segment string from an already-resolved label and branch.
///
/// Why: keeping the label/branch combination pure (no git I/O) makes the
/// branch-omission logic unit-testable without spawning subprocesses.
/// What: returns `None` for an empty label; otherwise `"<project> ⎇ <branch>"`
/// (single space, per the #2011 layout) for a meaningful branch, or just
/// `"<project>"` when the branch is empty, detached `HEAD`, or a managed
/// `session/<…>` branch (see [`is_managed_session_branch`]).
/// Test: `render_project_segment_omits_managed_session_branch`,
/// `render_project_segment_keeps_real_branch`, `render_project_segment_empty_label`.
fn render_project_segment(project: String, branch: Option<&str>) -> Option<String> {
    if project.is_empty() {
        return None;
    }
    Some(match branch {
        Some(b) if !b.is_empty() && b != "HEAD" && !is_managed_session_branch(b) => {
            format!("{project} \u{2387} {b}")
        }
        _ => project,
    })
}

/// Report whether `branch` is an auto-created managed tm session branch.
///
/// Why: tm provisions each managed session on a `session/<uuid>` branch; showing
/// it in the status bar just repeats the session id and crowds out the real
/// project identity, so it is elided.
/// What: matches the literal `session` branch and any `session/<…>` prefix.
/// Test: `is_managed_session_branch_matches_session_prefix`.
fn is_managed_session_branch(branch: &str) -> bool {
    branch == "session" || branch.starts_with("session/")
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

/// Build the cost segment (`$X.XX`).
///
/// Why: running cost visibility helps operators monitor API spend.
/// What: formats `usd` to two decimal places with a `$` prefix.
/// Test: `render_statusline_full_payload`.
fn cost_segment(usd: f64) -> String {
    format!("${usd:.2}")
}

/// Run `git rev-parse --abbrev-ref HEAD` in `cwd` with a hard 100 ms wall-clock
/// timeout and return the branch name.
///
/// Why: `statusline` is on Claude Code's hot render path; a stuck credential
/// helper, network filesystem, or git hook would otherwise block every render
/// cycle. The bounded thread + `recv_timeout` pattern matches
/// [`compaction::compaction_segment`].
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

/// Derive the GitHub `owner/repo` slug for `cwd` with a 100 ms wall-clock timeout.
///
/// Why: the leading status-bar field must identify the project, not the worktree
/// UUID; the canonical identity is the repo's `owner/repo` (the `source_id`). The
/// bounded-thread pattern (matching [`git_branch`]) keeps a stuck git config /
/// filesystem from blocking Claude Code's hot render path.
/// What: spawns a detached thread that calls the shared parser
/// [`trusty_common::github_path::derive_github_path`] (which shells to
/// `git -C <cwd> config --get remote.origin.url` — no network — and parses both
/// https and scp-style remotes), formats `owner/repo`, and sends it over an mpsc
/// channel; the caller waits ≤100 ms and returns `None` on timeout, no remote, or
/// an unparseable / non-GitHub URL.
/// Test: `git_owner_repo_reads_remote` (temp repo with a github origin);
/// `git_owner_repo_none_outside_repo` (bare temp dir → None).
fn git_owner_repo(cwd: &str) -> Option<String> {
    use std::sync::mpsc;
    use std::time::Duration;

    let cwd = cwd.to_string();
    let (tx, rx) = mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        let result = trusty_common::github_path::derive_github_path(std::path::Path::new(&cwd))
            .map(|gh| format!("{}/{}", gh.owner, gh.repo));
        let _ = tx.send(result);
    });
    rx.recv_timeout(Duration::from_millis(100)).ok().flatten()
}

/// Probe the active `gh` github.com account for the statusline, cheaply and
/// fail-soft, with a 100 ms wall-clock bound.
///
/// Why (#gh-account-awareness): `tm` shells to `gh` constantly (PR merge, issue
/// edits) but the operator had no visibility into WHICH github.com identity was
/// active. A non-admin active account silently broke `gh pr merge --admin`.
/// Surfacing `@<login>` in the status bar — and flagging the multi-account
/// ambiguity that hides the bug — makes the active identity impossible to miss.
/// The bounded-thread pattern (matching [`git_branch`]/[`git_owner_repo`]) keeps
/// a slow config read from ever blocking Claude Code's hot render path.
/// What: spawns a detached thread that reads `gh`'s `hosts.yml` via the cheap,
/// subprocess-free [`trusty_mpm::core::gh_account::gh_account_status_local`],
/// waits ≤100 ms, and renders the segment via [`render_gh_account_segment`].
/// Returns `None` (segment omitted) when `gh` is unconfigured, the read times
/// out, or no active account is set — never errors or blocks.
/// Test: the render is unit-tested via [`render_gh_account_segment`]; the probe
/// itself is thin bounded glue over the tested local reader.
fn gh_account_segment_probe() -> Option<String> {
    use std::sync::mpsc;
    use std::time::Duration;

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(trusty_mpm::core::gh_account::gh_account_status_local());
    });
    let status = rx.recv_timeout(Duration::from_millis(100)).ok().flatten()?;
    render_gh_account_segment(status.active.as_deref(), status.logged_in.len())
}

/// Assemble the `@<login>` gh-account segment from a resolved active login.
///
/// Why: keeping the render pure (no file/subprocess I/O) makes the
/// present/absent/multi-account cases unit-testable without touching real `gh`
/// state, and centralises the ambiguity marker.
/// What: returns `None` for an absent/blank active login (segment omitted);
/// `"@<login>"` for a single logged-in account; and `"@<login>⚠"` when more than
/// one account is logged in (`logged_in_count > 1`) so the operator notices they
/// may be on the wrong identity.
/// Test: `render_gh_account_segment_single`, `render_gh_account_segment_multi`,
/// `render_gh_account_segment_absent`.
fn render_gh_account_segment(active: Option<&str>, logged_in_count: usize) -> Option<String> {
    let active = active?.trim();
    if active.is_empty() {
        return None;
    }
    Some(if logged_in_count > 1 {
        format!("@{active}\u{26a0}")
    } else {
        format!("@{active}")
    })
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
        // Empty StatusInput (all defaults) must not panic; the `TM <ver>` lead-in
        // segment always appears (#2011).
        let out = render_statusline(&StatusInput::default());
        assert!(out.starts_with("TM "), "TM <ver> segment must always lead");
        assert!(!out.is_empty(), "output must not be empty");
    }

    #[test]
    fn render_statusline_full_payload() {
        // Full payload shows model name, cost; project segment when cwd is set.
        // `/home/user/my-project` is not a git repo → basename fallback ("my-project").
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

    /// Why (#2011): pins the exact reformatted layout — `TM <ver>` lead-in,
    /// `|`-joined segments (not the old `│` glyph), and no legacy session-count
    /// (`⛁N`) segment.
    /// Test: itself.
    #[test]
    fn render_statusline_full_payload_matches_pipe_format() {
        let input = full_input();
        let out = render_statusline(&input);
        let ver = env!("CARGO_PKG_VERSION");
        assert!(
            out.starts_with(&format!("TM {ver}")),
            "must lead with TM <ver>: {out}"
        );
        assert!(
            out.contains(" | "),
            "segments must be pipe-separated: {out}"
        );
        assert!(
            !out.contains('\u{2502}'),
            "old │ separator must not appear: {out}"
        );
        assert!(
            !out.contains('\u{26c1}'),
            "session-count segment must be dropped: {out}"
        );
    }

    /// Why (#2011): `version_port_segment` must append the parsed port only
    /// when the daemon is online, matching the `TM <ver> <port>` spec.
    /// Test: itself.
    #[test]
    fn version_port_segment_online_includes_port() {
        let daemon = DaemonInfo {
            addr: "127.0.0.1:7880".to_string(),
            online: true,
            session_count: None,
        };
        let seg = version_port_segment(&daemon);
        assert_eq!(seg, format!("TM {} 7880", env!("CARGO_PKG_VERSION")));
    }

    /// Why (#2011): an offline/absent daemon must degrade to `TM <ver>` alone
    /// rather than showing a stale or empty port.
    /// Test: itself.
    #[test]
    fn version_port_segment_offline_omits_port() {
        let daemon = DaemonInfo::default();
        let seg = version_port_segment(&daemon);
        assert_eq!(seg, format!("TM {}", env!("CARGO_PKG_VERSION")));
    }

    /// Why: a managed tm session runs on a `session/<uuid>` branch that just
    /// repeats the session id; it must be elided while real branches stay.
    /// Test: itself.
    #[test]
    fn is_managed_session_branch_matches_session_prefix() {
        assert!(is_managed_session_branch("session/2dfb23d1-579f-4168-9d28"));
        assert!(is_managed_session_branch("session/anything"));
        assert!(is_managed_session_branch("session"));
        assert!(!is_managed_session_branch("main"));
        assert!(!is_managed_session_branch("fix/foo"));
        // Not the managed prefix (note the trailing `s`).
        assert!(!is_managed_session_branch("sessions/foo"));
    }

    /// Why: a managed session must render `owner/repo` with NO branch (and hence
    /// no duplicated UUID) even though a `session/<uuid>` branch is checked out.
    /// Test: itself.
    #[test]
    fn render_project_segment_omits_managed_session_branch() {
        let seg = render_project_segment(
            "bobmatnyc/trusty-tools".to_string(),
            Some("session/2dfb23d1-579f-4168-9d28"),
        );
        assert_eq!(seg.as_deref(), Some("bobmatnyc/trusty-tools"));
        // No branch marker and no UUID leaked in.
        let s = seg.unwrap();
        assert!(!s.contains('\u{2387}'), "managed branch must be omitted");
        assert!(!s.contains("2dfb23d1"), "session UUID must not appear");
    }

    /// Why: a normal checkout on a meaningful branch must keep `⎇ <branch>`.
    /// Test: itself.
    #[test]
    fn render_project_segment_keeps_real_branch() {
        let seg = render_project_segment("bobmatnyc/trusty-tools".to_string(), Some("main"));
        assert_eq!(seg.as_deref(), Some("bobmatnyc/trusty-tools \u{2387} main"));

        // Detached HEAD and empty branch collapse to just the label.
        assert_eq!(
            render_project_segment("owner/repo".to_string(), Some("HEAD")).as_deref(),
            Some("owner/repo")
        );
        assert_eq!(
            render_project_segment("owner/repo".to_string(), None).as_deref(),
            Some("owner/repo")
        );
    }

    /// Why: an empty label (basename resolution failed) must omit the segment
    /// entirely rather than emit a stray `⎇`.
    /// Test: itself.
    #[test]
    fn render_project_segment_empty_label() {
        assert_eq!(render_project_segment(String::new(), Some("main")), None);
    }

    /// Why: a bare directory with no git repo must fall back to the cwd basename
    /// and never panic.
    /// Test: itself.
    #[test]
    fn project_segment_basename_fallback_non_git() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let cwd = tmp.path().to_string_lossy().to_string();
        let seg = project_segment(&cwd).expect("basename segment");
        let basename = tmp
            .path()
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        assert_eq!(seg, basename, "non-git cwd → basename, no branch");
        assert!(!seg.contains('\u{2387}'), "no branch marker for a bare dir");
    }

    /// Why: the leading field must resolve to the GitHub `owner/repo` slug parsed
    /// from the git remote (both scp-style and https), matching `source_id`.
    /// Test: itself (temp git repo; skipped when git is unavailable on the runner).
    #[test]
    fn git_owner_repo_reads_remote() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let dir = tmp.path();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !git(&["init"]) {
            return; // no git on runner
        }
        assert!(git(&[
            "remote",
            "add",
            "origin",
            "git@github.com:bobmatnyc/trusty-tools.git",
        ]));
        let cwd = dir.to_string_lossy().to_string();
        assert_eq!(
            git_owner_repo(&cwd).as_deref(),
            Some("bobmatnyc/trusty-tools"),
            "scp-style remote → owner/repo slug"
        );
    }

    /// Why: outside any git repo `git_owner_repo` must yield `None` so callers
    /// fall back to the basename.
    /// Test: itself.
    #[test]
    fn git_owner_repo_none_outside_repo() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let cwd = tmp.path().to_string_lossy().to_string();
        assert_eq!(git_owner_repo(&cwd), None);
    }

    /// Why: a single logged-in account renders `@<login>` with no warning mark.
    /// Test: itself.
    #[test]
    fn render_gh_account_segment_single() {
        assert_eq!(
            render_gh_account_segment(Some("bobmatnyc"), 1).as_deref(),
            Some("@bobmatnyc")
        );
        // Zero count is treated the same as one (unambiguous single identity).
        assert_eq!(
            render_gh_account_segment(Some("bobmatnyc"), 0).as_deref(),
            Some("@bobmatnyc")
        );
    }

    /// Why: multiple logged-in accounts must append the `⚠` ambiguity marker so
    /// the operator notices they may be merging as the wrong identity (the bug).
    /// Test: itself.
    #[test]
    fn render_gh_account_segment_multi() {
        let seg = render_gh_account_segment(Some("bob-duetto"), 2).expect("segment");
        assert_eq!(seg, "@bob-duetto\u{26a0}");
        assert!(seg.contains('\u{26a0}'), "multi-account marker must appear");
    }

    /// Why: an absent/blank active login must omit the segment entirely rather
    /// than emit a stray `@`.
    /// Test: itself.
    #[test]
    fn render_gh_account_segment_absent() {
        assert_eq!(render_gh_account_segment(None, 0), None);
        assert_eq!(render_gh_account_segment(Some("   "), 3), None);
        assert_eq!(render_gh_account_segment(Some(""), 1), None);
    }

    #[test]
    fn render_statusline_missing_model_omits_segment() {
        // When both model.id and model.display_name are empty, no model segment.
        let mut input = full_input();
        input.model = ModelInfo::default();
        let out = render_statusline(&input);
        assert!(!out.contains("claude"), "model segment must be omitted");
        assert!(out.starts_with("TM "), "TM <ver> segment must still appear");
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
            out.starts_with("TM "),
            "graceful fallback must still emit the TM <ver> segment"
        );
        assert!(
            !out.contains("| |"),
            "must not produce empty separator pairs"
        );
    }

    #[test]
    fn render_statusline_no_empty_separator_pairs() {
        // With every optional field empty, the only segment is the TM <ver> lead-in.
        let out = render_statusline(&StatusInput::default());
        assert!(!out.contains("|  | "), "no empty | | pairs");
        assert!(!out.contains("| |"), "no adjacent separators");
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
