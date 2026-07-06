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
//! `assemble_statusline_pins_segment_order_with_markers` in tests.

mod branch;
pub(crate) mod compaction;

use std::io::Read as _;

use crate::formatters::info_box::DaemonInfo;
use branch::project_segment;
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
/// non-blocking and infallible). Probing each segment here and handing the
/// results to the pure [`assemble_statusline`] lets the fixed segment ORDER be
/// pinned by a deterministic unit test with hand-supplied strings, independent
/// of real git/gh/daemon state.
/// What: probes each of the six segments (version+port, project+branch, gh
/// account, model, ctx%, cost) in spec order and joins them via
/// [`assemble_statusline`].
/// Test: `render_statusline_minimal_input`, `render_statusline_full_payload`,
/// `render_statusline_full_payload_matches_pipe_format`.
pub(crate) fn render_statusline(input: &StatusInput) -> String {
    // Lock-file read only — cheap and non-blocking; no HTTP session-count probe
    // is needed since the reformatted layout (#2011) no longer surfaces a count.
    let daemon = DaemonInfo::from_lock_file();
    let version_port = version_port_segment(&daemon);

    let project = project_segment(&input.cwd);
    let gh = gh_account_segment_probe();
    let model = model_segment(&input.model);

    // Compaction efficiency / live context fill; falls back to a bare
    // `ctx>200k` marker when no context-window payload was sent at all.
    let ctx = compaction_segment(&input.session_id, input.context_window.as_ref())
        .or_else(|| input.exceeds_200k_tokens.then(|| "ctx>200k".to_string()));

    let cost = (input.cost.total_cost_usd > 0.0).then(|| cost_segment(input.cost.total_cost_usd));

    assemble_statusline(version_port, project, gh, model, ctx, cost)
}

/// Join the six statusline segments in spec order, omitting absent ones.
///
/// Why (#2011 follow-up): separating ORDER + JOIN from the I/O-bound probes
/// that produce each segment value is what makes the exact layout —
/// `TM <ver> <port> | <project> ⎇ <branch> | @<gh> | <model> | ctx% | <cost>`
/// — pinned by a deterministic test using hand-supplied synthetic strings.
/// Without this split, a future refactor could transpose or silently drop a
/// middle segment and no test would catch it.
/// What: takes the six segments in fixed spec order (`version_port` is always
/// present; the rest are `Option<String>`), filters out `None`s, and joins the
/// survivors with ` | `. Pure — no I/O, no panics.
/// Test: `assemble_statusline_full_payload_pins_order_and_format`,
/// `assemble_statusline_pins_segment_order_with_markers`,
/// `assemble_statusline_omits_none_segments`.
fn assemble_statusline(
    version_port: String,
    project: Option<String>,
    gh: Option<String>,
    model: Option<String>,
    ctx: Option<String>,
    cost: Option<String>,
) -> String {
    [Some(version_port), project, gh, model, ctx, cost]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" | ")
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
//
// The project-identity + branch-label segment (`project_segment` and its
// tmux/git helpers) lives in the sibling `branch` module (#2031 follow-up,
// split out to stay under the 500-SLOC production file cap).

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

/// Probe the active `gh` github.com account for the statusline, cheaply and
/// fail-soft, with a 100 ms wall-clock bound.
///
/// Why (#gh-account-awareness): `tm` shells to `gh` constantly (PR merge, issue
/// edits) but the operator had no visibility into WHICH github.com identity was
/// active. A non-admin active account silently broke `gh pr merge --admin`.
/// Surfacing `@<login>` in the status bar — and flagging the multi-account
/// ambiguity that hides the bug — makes the active identity impossible to miss.
/// The bounded-thread pattern (matching the `branch` module's git/tmux probes)
/// keeps a slow config read from ever blocking Claude Code's hot render path.
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

    /// Why (#2011 follow-up): `render_statusline_full_payload_matches_pipe_format`
    /// only checks a prefix/substring/absence, so a refactor that transposed or
    /// silently dropped a middle segment (`@gh` / model / ctx% / cost) would
    /// still pass it. This test drives the pure join layer directly with
    /// synthetic, realistic segment strings — no git/gh/daemon I/O — and
    /// asserts the exact joined output, pinning both the format AND the order.
    /// Test: itself.
    #[test]
    fn assemble_statusline_full_payload_pins_order_and_format() {
        let out = assemble_statusline(
            "TM 1.2.3 7880".to_string(),
            Some("bobmatnyc/trusty-tools \u{2387} main".to_string()),
            Some("@bobmatnyc".to_string()),
            Some("Claude Sonnet 4.6".to_string()),
            Some("ctx 41%".to_string()),
            Some("$1.23".to_string()),
        );
        assert_eq!(
            out,
            "TM 1.2.3 7880 | bobmatnyc/trusty-tools \u{2387} main | @bobmatnyc | Claude Sonnet 4.6 | ctx 41% | $1.23"
        );
    }

    /// Why (#2011 follow-up): guards specifically against segment
    /// transposition — using distinct single-letter markers instead of
    /// realistic strings means a swap between any two positions (e.g. gh and
    /// model) produces a different, easily-diffed string rather than two
    /// plausible-looking segments that a reviewer might not notice swapped.
    /// What: asserts both the exact joined string AND the split-on-` | `
    /// vector equal `[version_port, project, gh, model, ctx, cost]`.
    /// Test: itself.
    #[test]
    fn assemble_statusline_pins_segment_order_with_markers() {
        let out = assemble_statusline(
            "VERSION_PORT".to_string(),
            Some("PROJECT".to_string()),
            Some("GH".to_string()),
            Some("MODEL".to_string()),
            Some("CTX".to_string()),
            Some("COST".to_string()),
        );
        assert_eq!(out, "VERSION_PORT | PROJECT | GH | MODEL | CTX | COST");
        assert_eq!(
            out.split(" | ").collect::<Vec<_>>(),
            vec!["VERSION_PORT", "PROJECT", "GH", "MODEL", "CTX", "COST"],
            "segment order must exactly match [version_port, project, gh, model, ctx, cost]"
        );
    }

    /// Why (#2011 follow-up): every segment except `version_port` must be
    /// omittable independently without leaving a stray/empty ` | ` pair, and
    /// omitting all of them must fall back to just the lead-in segment.
    /// Test: itself.
    #[test]
    fn assemble_statusline_omits_none_segments() {
        // All optional segments absent → only the lead-in segment appears.
        let out = assemble_statusline("TM 1.2.3".to_string(), None, None, None, None, None);
        assert_eq!(out, "TM 1.2.3");

        // A gap in the middle (gh absent) must not leave an empty separator pair.
        let out = assemble_statusline(
            "TM 1.2.3".to_string(),
            Some("proj".to_string()),
            None,
            Some("model".to_string()),
            None,
            Some("$0.50".to_string()),
        );
        assert_eq!(out, "TM 1.2.3 | proj | model | $0.50");
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
