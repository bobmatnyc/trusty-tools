//! Handler for `trusty-memory prompt-context`.
//!
//! Why: Claude Code's `UserPromptSubmit` hooks inject any stdout produced by
//! the hook command as additional context for the model. The trusty-memory
//! setup command installs a hook that runs `trusty-memory prompt-context`
//! before every prompt, so the model gets a freshly-rendered block of
//! palace context (drawer recall + KG triples + global hot facts) without
//! paying the per-message MCP tool-call tax. This handler is the actual
//! command the hook invokes.
//!
//! What: a side-effect-only command that talks to the running trusty-memory
//! HTTP daemon and prints a formatted Markdown injection block to stdout.
//! Composition (issue #134):
//!
//!   1. Workspace hot facts (existing `GET /api/v1/kg/prompt-context`).
//!   2. Drawers recalled from the cwd-resolved palace
//!      (`GET /api/v1/palaces/{slug}/recall?q=<prompt>`).
//!   3. Knowledge-graph triples whose subject appears in the prompt
//!      (`GET /api/v1/palaces/{slug}/kg/all?limit=200`).
//!
//! Failures on any branch are isolated — each fetch is bounded by
//! `HTTP_TIMEOUT` and individual errors are skipped without failing the
//! hook. If everything is empty, the existing placeholder is emitted so
//! the empty-palace fallback behaviour is preserved.
//!
//! Note on MPM sub-agents: unlike `trusty-mpm hook`, this command is
//! **intentionally NOT** gated on the `CLAUDE_MPM_SUB_AGENT` environment
//! variable. Sub-agents benefit from the parent palace's prompt-fact block
//! just as much as the PM does — withholding it would force every nested
//! agent to rediscover project conventions from scratch. The token cost is
//! a single rendered fact list and the signal payoff (consistent style,
//! vocabulary, and architectural facts across the agent tree) is high. The
//! suppression of nested hook traffic happens at the `trusty-mpm hook`
//! layer instead, where doubled audit events are the real failure mode.
//!
//! Test: daemon-touching paths are exercised via integration tests in this
//! module (`prompt_context_recalls_palace_drawers`,
//! `prompt_context_empty_palace_falls_back_to_global`,
//! `prompt_context_returns_ok_without_daemon`).

mod fetch;
mod filter;
mod format;

use anyhow::Result;
use serde_json::Value;
use std::time::{Duration, Instant};

use crate::hook_emit::{post_hook_event, HookEventPayload};
use crate::prompt_log::{PromptLogEntry, PromptLogger};
use crate::{hook_prompt_excerpt, HookType, InjectionKind};

use fetch::{fetch_global_prompt_context, fetch_palace_kg_triples, fetch_palace_recall};
use filter::{filter_drawers_by_deny_tags, select_relevant_triples};
use format::{compose_injection, count_facts};

/// HTTP path for the global hot-facts block.
pub(super) const PROMPT_CONTEXT_PATH: &str = "/api/v1/kg/prompt-context";

/// HTTP path template for per-palace recall. Substitute `{slug}`.
pub(super) const PALACE_RECALL_PATH: &str = "/api/v1/palaces/{slug}/recall";

/// HTTP path template for per-palace KG list. Substitute `{slug}`.
pub(super) const PALACE_KG_ALL_PATH: &str = "/api/v1/palaces/{slug}/kg/all";

/// Connect + total request timeout. Kept short so a slow/dead daemon can
/// never block a Claude Code prompt for more than a couple seconds.
pub(super) const HTTP_TIMEOUT: Duration = Duration::from_millis(2500);

/// Default top-K for drawer recall and KG triple selection.
///
/// Why: 5 + 5 keeps the injection focused on the strongest signal without
/// flooding the prompt. With a 4 KB cap on total output, this leaves ample
/// budget for hot facts and per-bullet content.
/// What: a `usize` constant used unless the env override below is set.
/// Test: `prompt_context_recalls_palace_drawers` uses the default.
pub(super) const DEFAULT_TOP_K: usize = 5;

/// Hard byte cap on the rendered injection.
///
/// Why: hook-injection budgets in Claude Code are small (~few KB) and
/// every byte we emit is a token the model has to spend reading. 4 KB is
/// a comfortable ceiling above the typical render and well under any
/// downstream limit.
/// What: `4 * 1024` bytes. Sections are appended until the cap is hit;
/// truncation emits an explicit `…` marker so downstream readers know
/// the block was cut.
/// Test: `prompt_context_recalls_palace_drawers` exercises the budget
/// implicitly by asserting a real (non-placeholder) injection.
pub(super) const INJECTION_BYTE_CAP: usize = 4 * 1024;

/// Per-drawer-content preview cap inside the injection.
///
/// Why: dumping the full drawer body would burn the byte budget on a
/// single entry; a short single-line preview is enough to remind the model
/// what's available and lets it pull more via MCP recall if needed.
/// What: `220` characters of the whitespace-collapsed content.
/// Test: indirectly via `prompt_context_recalls_palace_drawers`.
pub(super) const DRAWER_PREVIEW_CHARS: usize = 220;

/// Env override for the top-K used by both recall and KG walks.
///
/// Why: gives operators an emergency knob without re-deploying. Optional
/// — when unset / unparseable / zero, [`DEFAULT_TOP_K`] is used.
/// What: a string env var parsed as a `usize`; clamped to `[1, 20]` to
/// keep the byte budget meaningful.
/// Test: not unit-tested (env mutation across parallel tests is hostile).
pub const ENV_TOP_K: &str = "TRUSTY_MEMORY_PROMPT_TOP_K";

/// Env override for the deny-listed drawer tags filtered out of recall.
///
/// Why (issue #139): operators need a way to widen or narrow the noise
/// filter without a rebuild — e.g. add project-specific synthetic tags
/// that have polluted a palace from an upstream hook source. Optional —
/// when unset, the recall path uses [`DEFAULT_DENY_TAGS`].
/// What: a comma-separated list of tag strings. Whitespace around each
/// entry is trimmed; empty entries are ignored; matching is case-
/// insensitive against the drawer's tag list.
/// Test: `prompt_context_recall_env_override_extends_deny_list` exercises
/// the env-driven path with a synthetic noise tag.
pub const ENV_RECALL_DENY_TAGS: &str = "TRUSTY_MEMORY_PROMPT_RECALL_DENY_TAGS";

/// Default deny list applied to recalled drawer tags before composition.
///
/// Why (issue #139): live evidence in the user's palace showed that the
/// auto-capture hook (`trusty-memory hooks fire claude.user-prompt`,
/// wired by `trusty-mpm-core::session_launch`) persists every user prompt
/// as a drawer tagged `claude-session` + `user-prompt`. These drawers
/// dominate recall and crowd out signal content — three sample sessions
/// returned the literal token "yes" five times across semantically
/// distinct prompts. Filtering by tag is cheap, safe (empty tag lists
/// pass through unchanged), and reversible via [`ENV_RECALL_DENY_TAGS`].
/// What: a `&[&str]` of tag names. A drawer is filtered when ANY of its
/// tags matches (case-insensitive) ANY entry in this list.
/// Test: `prompt_context_recall_filters_deny_tags` covers the default
/// path; `prompt_context_recall_all_filtered_falls_back_to_global` covers
/// the all-filtered fallback.
const DEFAULT_DENY_TAGS: &[&str] = &["claude-session", "user-prompt"];

/// Placeholder body emitted when no daemon is reachable or every fetch
/// returned nothing useful.
///
/// Why: kept verbatim from the pre-#134 behaviour so the empty-palace
/// case is byte-identical for downstream tooling. The non-empty palace
/// path now overrides it with real content.
/// What: a static string.
/// Test: `prompt_context_empty_palace_falls_back_to_global`.
pub(super) const EMPTY_PLACEHOLDER: &str = "No prompt facts stored yet.";

/// Entry point for `trusty-memory prompt-context`.
///
/// Why: every error path in this handler must result in a clean exit 0 — the
/// `UserPromptSubmit` hook is wired into every Claude Code prompt the user
/// types, so any non-zero exit (or panic) would either block the prompt or
/// inject a confusing error into the model's context. Logging to stderr is
/// fine because Claude Code only ingests stdout from hook commands.
/// What:
///   1. Read stdin (the UserPromptSubmit JSON payload) — extract `cwd` and
///      `prompt`.
///   2. Resolve the palace slug from the stdin `cwd` (fall back to process
///      cwd).
///   3. Fetch the global prompt-context block + per-palace recall + per-
///      palace KG triples (each best-effort, bounded by [`HTTP_TIMEOUT`]).
///   4. Compose a single Markdown injection capped at
///      [`INJECTION_BYTE_CAP`] bytes and print it.
///   5. Log a [`PromptLogEntry`] for the hook event (failure-isolated).
///
/// Sub-agent behaviour: deliberately unguarded. MPM-spawned sub-agents inject
/// the same prompt-context block as the PM because the marginal token cost
/// is small and the convention/style signal is high — see the module-level
/// note for the full rationale.
/// Test: `prompt_context_returns_ok_without_daemon` covers the no-daemon
/// branch; live-daemon paths are exercised by
/// `prompt_context_recalls_palace_drawers` and
/// `prompt_context_empty_palace_falls_back_to_global`.
pub async fn handle_prompt_context() -> Result<()> {
    let start = Instant::now();
    let trigger_payload = read_stdin_best_effort();
    let body = build_injection_body(&trigger_payload).await;
    if body.ends_with('\n') {
        print!("{body}");
    } else {
        println!("{body}");
    }

    // Submission-logging Part A: emit a `HookFired` activity event so the
    // dashboard / TUI feed shows this prompt-context invocation. Best-effort
    // — failures are swallowed inside `post_hook_event` so the hook never
    // fails because of activity-emit problems.
    emit_hook_event(&trigger_payload, &body, start).await;

    Ok(())
}

/// POST a `HookFired` event to the daemon's activity ingestion endpoint.
///
/// Why: surfaces every prompt-context hook firing in the activity feed
/// (issue: TUI activity feed was empty in sessions whose only daemon
/// traffic was hooks).
/// What: builds a `HookEventPayload` carrying the resolved palace, the
/// rendered injection length, a short excerpt of the user prompt, and
/// the hook's elapsed wall-clock duration, then calls `post_hook_event`.
/// Test: `hook_fired_activity_emit_smoke` in this module.
async fn emit_hook_event(trigger_payload: &str, injection: &str, start: Instant) {
    let user_prompt = parse_user_prompt(trigger_payload);
    let palace_id = resolve_palace_slug(trigger_payload);
    let payload = HookEventPayload {
        palace_id: palace_id.clone(),
        palace_name: palace_id,
        hook_type: HookType::UserPromptSubmit,
        injection_kind: InjectionKind::PromptContext,
        injection_length: injection.len() as u64,
        trigger_prompt_excerpt: hook_prompt_excerpt(&user_prompt),
        duration_ms: start.elapsed().as_millis() as u64,
    };
    post_hook_event(payload).await;
}

/// Build the prompt-context injection body for a given stdin payload.
///
/// Why: factored out of [`handle_prompt_context`] so integration tests can
/// drive the full enrichment pipeline against a real HTTP daemon without
/// trampling the process' stdout. Production code wraps this with
/// [`handle_prompt_context`] which prints the result and returns `Ok(())`.
/// What: same flow as the original — resolve daemon → resolve palace →
/// fetch global facts + recall + KG → compose injection → log. Returns
/// the rendered body verbatim. Never panics; every failure path degrades
/// to the legacy placeholder or an empty string.
/// Test: `prompt_context_recalls_palace_drawers`,
/// `prompt_context_empty_palace_falls_back_to_global`.
pub(crate) async fn build_injection_body(trigger_payload: &str) -> String {
    let start = Instant::now();
    let user_prompt = parse_user_prompt(trigger_payload);

    // 1. Discover the running daemon. Missing file → daemon not running →
    //    return empty so the caller exits silently with no stdout output.
    let addr = match trusty_common::read_daemon_addr("trusty-memory") {
        Ok(Some(addr)) => addr,
        Ok(None) | Err(_) => {
            log_entry(trigger_payload, "", 0, start);
            return String::new();
        }
    };

    // The shared helper persists the bare `host:port`. The web daemon binds
    // HTTP, so prepend the scheme when callers haven't already.
    let base = if addr.starts_with("http://") || addr.starts_with("https://") {
        addr
    } else {
        format!("http://{addr}")
    };

    // 2. Tightly-bounded HTTP client. Any failure → return empty silently so
    //    the Claude Code prompt is never blocked by a degraded daemon.
    let client = match reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(HTTP_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            log_entry(trigger_payload, "", 0, start);
            return String::new();
        }
    };

    // 3. Resolve the palace slug from the stdin `cwd` first, then fall back
    //    to the process cwd. Both lookups are wrapped in `ok()` so failure
    //    just yields `None` (we'll skip palace-specific sections).
    let palace_slug = resolve_palace_slug(trigger_payload);

    // 4. Fan out the fetches. Each is best-effort; failures are skipped.
    let global_facts = fetch_global_prompt_context(&client, &base).await;
    let (drawers, kg_triples) = match &palace_slug {
        Some(slug) => {
            let top_k = configured_top_k();
            let drawers_fut = fetch_palace_recall(&client, &base, slug, &user_prompt, top_k);
            let kg_fut = fetch_palace_kg_triples(&client, &base, slug);
            let (drawers, kg_all) = tokio::join!(drawers_fut, kg_fut);
            // Issue #139: drop low-signal drawers (e.g. `claude-session` /
            // `user-prompt` auto-captures) before composition. When this
            // filter empties the recall set, `compose_injection` falls
            // back to global hot facts via the existing branch below.
            let deny_tags = configured_deny_tags();
            let drawers = filter_drawers_by_deny_tags(drawers, &deny_tags);
            let kg_filtered = select_relevant_triples(&kg_all, &user_prompt, top_k);
            (drawers, kg_filtered)
        }
        None => (Vec::new(), Vec::new()),
    };

    // 5. Compose the injection. If every section is empty, emit the legacy
    //    placeholder so downstream consumers see byte-identical behaviour
    //    on a brand-new install.
    let composed = compose_injection(
        global_facts.as_deref(),
        &drawers,
        &kg_triples,
        palace_slug.as_deref(),
    );
    let body = if composed.is_empty() {
        EMPTY_PLACEHOLDER.to_string()
    } else {
        composed
    };

    // Best-effort log entry — `count_facts` approximates the number of
    // bulleted facts in the rendered Markdown block. Errors are swallowed
    // inside the logger.
    let facts_count = count_facts(&body);
    log_entry(trigger_payload, &body, facts_count, start);

    body
}

/// Read the hook's stdin into a string, capped at 64 KiB.
///
/// Why (issue #105): the UserPromptSubmit hook delivers the user prompt as
/// stdin so we capture it for the enriched-prompt log. Stdin may be empty
/// (e.g. when the daemon is probed manually). The cap defends against an
/// adversarial prompt the size of a novel from inflating the log file.
/// What: synchronously reads stdin to EOF (or 64 KiB), returns the trimmed
/// payload. Failures degrade to an empty string — the hook continues either
/// way.
/// Test: not unit-tested (process stdin is hard to mock); covered by the
/// integration test which writes the entry directly.
fn read_stdin_best_effort() -> String {
    use std::io::Read;
    const STDIN_CAP_BYTES: usize = 64 * 1024;
    // `is_terminal()` lets us bail when stdin is the controlling TTY — there
    // is no prompt to read in that case and `read_to_string` would block.
    let stdin = std::io::stdin();
    if std::io::IsTerminal::is_terminal(&stdin) {
        return String::new();
    }
    let mut buf = String::new();
    let _ = stdin
        .lock()
        .take(STDIN_CAP_BYTES as u64)
        .read_to_string(&mut buf);
    buf
}

/// Extract the user prompt string from the stdin JSON payload.
///
/// Why (issue #134): the recall query against the palace's vectors needs
/// the actual prompt text the user typed. Claude Code's UserPromptSubmit
/// hook payload carries `"prompt": "..."`; without it we'd have to recall
/// against an empty string and return generic results.
/// What: best-effort `serde_json` parse — on success and when the JSON has
/// a string `prompt` field, returns it trimmed. On any failure (non-JSON,
/// missing field) returns the raw stdin payload trimmed, so a manually-
/// piped prompt still drives recall.
/// Test: `parse_user_prompt_prefers_prompt_field`.
fn parse_user_prompt(stdin_payload: &str) -> String {
    if stdin_payload.trim().is_empty() {
        return String::new();
    }
    if let Ok(value) = serde_json::from_str::<Value>(stdin_payload) {
        if let Some(p) = value.get("prompt").and_then(|v| v.as_str()) {
            return p.trim().to_string();
        }
    }
    stdin_payload.trim().to_string()
}

/// Read the optional [`ENV_TOP_K`] env var, clamped to a sane range.
///
/// Why: operator escape hatch with a strict ceiling so accidental large
/// values can't blow the byte budget.
/// What: parses the env string as a `usize`; on success clamps to
/// `[1, 20]`; on failure returns [`DEFAULT_TOP_K`].
/// Test: not unit-tested (env mutation races); covered by the default
/// path through `prompt_context_recalls_palace_drawers`.
fn configured_top_k() -> usize {
    std::env::var(ENV_TOP_K)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map(|k| k.clamp(1, 20))
        .unwrap_or(DEFAULT_TOP_K)
}

/// Resolve the effective deny-list of drawer tags for prompt-context recall.
///
/// Why (issue #139): centralises the env-override + default logic so the
/// filter call site stays small and the deny list is testable in isolation.
/// What: returns the lowercase tag strings parsed from
/// [`ENV_RECALL_DENY_TAGS`] when set (comma-separated, whitespace-trimmed,
/// empty entries skipped). Falls back to [`DEFAULT_DENY_TAGS`] when the env
/// var is unset, empty, or contains nothing but whitespace/commas.
/// Test: not unit-tested directly (env mutation races); covered indirectly
/// via `prompt_context_recall_env_override_extends_deny_list` and
/// `prompt_context_recall_filters_deny_tags`.
fn configured_deny_tags() -> Vec<String> {
    if let Ok(raw) = std::env::var(ENV_RECALL_DENY_TAGS) {
        let parsed: Vec<String> = raw
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        if !parsed.is_empty() {
            return parsed;
        }
    }
    DEFAULT_DENY_TAGS.iter().map(|s| s.to_lowercase()).collect()
}

/// Resolve the palace slug from the stdin payload.
///
/// Why (issue #125 + #134): the hook's recall + KG enrichment both target
/// the project palace that owns the user's actual cwd, not the cwd the
/// hook process was launched with. The stdin `cwd` is the source of truth.
/// What: parse stdin as JSON, take `cwd`, derive slug via
/// [`crate::messaging::cwd_palace_slug_at`]. Falls back to the process
/// cwd's slug. Returns `None` only when neither resolves cleanly.
/// Test: `resolve_palace_for_log_prefers_stdin_cwd` (the log helper uses
/// the same chain).
fn resolve_palace_slug(stdin_payload: &str) -> Option<String> {
    if let Some(slug) = palace_slug_from_stdin_cwd(stdin_payload) {
        return Some(slug);
    }
    crate::messaging::cwd_palace_slug().ok()
}

/// Resolve the palace identifier for the log entry.
///
/// Why (issue #125): see [`resolve_palace_slug`]; the log helper keeps
/// the legacy `"<unknown>"` sentinel so log shape stays stable.
/// Test: `resolve_palace_for_log_prefers_stdin_cwd`.
fn resolve_palace_for_log(stdin_payload: &str) -> String {
    resolve_palace_slug(stdin_payload).unwrap_or_else(|| "<unknown>".to_string())
}

/// Parse `stdin_payload` as JSON and, when it carries a `cwd` string, derive
/// the palace slug from that path.
///
/// Why: factored out so the unit test can exercise the stdin-override path
/// without manipulating the process cwd.
/// What: returns `Some(slug)` only when the payload parses as a JSON object,
/// contains a non-empty string `cwd`, and slug derivation succeeds for that
/// path. Returns `None` on every failure mode so the caller can fall back.
/// Test: `resolve_palace_for_log_prefers_stdin_cwd`.
fn palace_slug_from_stdin_cwd(stdin_payload: &str) -> Option<String> {
    if stdin_payload.trim().is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(stdin_payload).ok()?;
    let cwd = value.get("cwd")?.as_str()?;
    if cwd.is_empty() {
        return None;
    }
    crate::messaging::cwd_palace_slug_at(std::path::Path::new(cwd)).ok()
}

/// Append one log entry to the enriched-prompt log, swallowing failures.
///
/// Why: prompt logging is best-effort — a write failure must never block
/// the hook from completing.
/// What: constructs a `PromptLogEntry` and writes it via `PromptLogger`.
/// Test: `prompt_context_logs_attempt_without_daemon`.
fn log_entry(trigger_prompt: &str, injection: &str, facts_count: usize, start: Instant) {
    let logger = PromptLogger::from_env();
    let palace = resolve_palace_for_log(trigger_prompt);
    let entry = PromptLogEntry::new(
        "UserPromptSubmit",
        "prompt-context-facts",
        palace,
        trigger_prompt,
        injection,
    )
    .with_palace_facts_count(facts_count)
    .with_duration_ms(start.elapsed().as_millis() as u64);
    logger.log(entry);
}

#[cfg(test)]
mod tests;
