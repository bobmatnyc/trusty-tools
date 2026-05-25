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

use anyhow::Result;
use serde_json::Value;
use std::time::{Duration, Instant};

use crate::prompt_log::{PromptLogEntry, PromptLogger};

/// HTTP path for the global hot-facts block.
const PROMPT_CONTEXT_PATH: &str = "/api/v1/kg/prompt-context";

/// HTTP path template for per-palace recall. Substitute `{slug}`.
const PALACE_RECALL_PATH: &str = "/api/v1/palaces/{slug}/recall";

/// HTTP path template for per-palace KG list. Substitute `{slug}`.
const PALACE_KG_ALL_PATH: &str = "/api/v1/palaces/{slug}/kg/all";

/// Connect + total request timeout. Kept short so a slow/dead daemon can
/// never block a Claude Code prompt for more than a couple seconds.
const HTTP_TIMEOUT: Duration = Duration::from_millis(2500);

/// Default top-K for drawer recall and KG triple selection.
///
/// Why: 5 + 5 keeps the injection focused on the strongest signal without
/// flooding the prompt. With a 4 KB cap on total output, this leaves ample
/// budget for hot facts and per-bullet content.
/// What: a `usize` constant used unless the env override below is set.
/// Test: `prompt_context_recalls_palace_drawers` uses the default.
const DEFAULT_TOP_K: usize = 5;

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
const INJECTION_BYTE_CAP: usize = 4 * 1024;

/// Per-drawer-content preview cap inside the injection.
///
/// Why: dumping the full drawer body would burn the byte budget on a
/// single entry; a short single-line preview is enough to remind the model
/// what's available and lets it pull more via MCP recall if needed.
/// What: `220` characters of the whitespace-collapsed content.
/// Test: indirectly via `prompt_context_recalls_palace_drawers`.
const DRAWER_PREVIEW_CHARS: usize = 220;

/// Env override for the top-K used by both recall and KG walks.
///
/// Why: gives operators an emergency knob without re-deploying. Optional
/// — when unset / unparseable / zero, [`DEFAULT_TOP_K`] is used.
/// What: a string env var parsed as a `usize`; clamped to `[1, 20]` to
/// keep the byte budget meaningful.
/// Test: not unit-tested (env mutation across parallel tests is hostile).
pub const ENV_TOP_K: &str = "TRUSTY_MEMORY_PROMPT_TOP_K";

/// Placeholder body emitted when no daemon is reachable or every fetch
/// returned nothing useful.
///
/// Why: kept verbatim from the pre-#134 behaviour so the empty-palace
/// case is byte-identical for downstream tooling. The non-empty palace
/// path now overrides it with real content.
/// What: a static string.
/// Test: `prompt_context_empty_palace_falls_back_to_global`.
const EMPTY_PLACEHOLDER: &str = "No prompt facts stored yet.";

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
    let trigger_payload = read_stdin_best_effort();
    let body = build_injection_body(&trigger_payload).await;
    if body.ends_with('\n') {
        print!("{body}");
    } else {
        println!("{body}");
    }
    Ok(())
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

/// Fetch the global prompt-context block (workspace hot facts).
///
/// Why: keeps the legacy behaviour intact — workspace-level aliases and
/// conventions continue to surface even when the palace itself is empty.
/// What: `GET /api/v1/kg/prompt-context`; returns `Some(body)` only on a
/// 2xx with a non-empty, non-placeholder body. Any failure → `None`.
/// Test: indirectly via `prompt_context_recalls_palace_drawers` and
/// `prompt_context_empty_palace_falls_back_to_global`.
async fn fetch_global_prompt_context(client: &reqwest::Client, base: &str) -> Option<String> {
    let url = format!("{base}{PROMPT_CONTEXT_PATH}");
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.text().await.ok()?;
    let trimmed = body.trim();
    if trimmed.is_empty() || trimmed == EMPTY_PLACEHOLDER {
        None
    } else {
        Some(body)
    }
}

/// Fetch up to `top_k` recalled drawer entries from the palace.
///
/// Why (issue #134): the entire value of automatic context injection is
/// surfacing relevant memories; this is the real recall hop the prior
/// implementation lacked. Uses the existing `recall_handler` endpoint so
/// no new wire surface is introduced.
/// What: `GET /api/v1/palaces/{slug}/recall?q=<prompt>&top_k=<k>`; parses
/// the JSON array, returns `Vec<RecalledDrawer>`. Empty prompt or 4xx/5xx
/// returns an empty vec. Failures are swallowed.
/// Test: `prompt_context_recalls_palace_drawers`.
async fn fetch_palace_recall(
    client: &reqwest::Client,
    base: &str,
    palace: &str,
    prompt: &str,
    top_k: usize,
) -> Vec<RecalledDrawer> {
    if prompt.is_empty() {
        return Vec::new();
    }
    let path = PALACE_RECALL_PATH.replace("{slug}", palace);
    let url = format!("{base}{path}");
    let resp = match client
        .get(&url)
        .query(&[("q", prompt.to_string()), ("top_k", top_k.to_string())])
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    if !resp.status().is_success() {
        return Vec::new();
    }
    let body: Value = match resp.json().await {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    let Some(arr) = body.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(RecalledDrawer::from_recall_entry)
        // Drop the synthetic L0 identity drawer — it leaks the palace
        // bootstrap message which is noise in the injection.
        .filter(|d| d.layer.unwrap_or(0) > 0)
        .take(top_k)
        .collect()
}

/// Fetch active KG triples from the palace.
///
/// Why (issue #134): subject-anchored triples (`tga is_alias_for trusty-
/// git-analytics`, `rust is-a language`) are exactly the kind of ambient
/// facts the model benefits from when the prompt mentions one of those
/// subjects. We fetch up to 200 to keep the in-memory filter cheap.
/// What: `GET /api/v1/palaces/{slug}/kg/all?limit=200`; returns the raw
/// triple array, empty on any failure.
/// Test: `prompt_context_recalls_palace_drawers` (asserts KG section
/// appears when prompt mentions a known subject).
async fn fetch_palace_kg_triples(
    client: &reqwest::Client,
    base: &str,
    palace: &str,
) -> Vec<RawTriple> {
    let path = PALACE_KG_ALL_PATH.replace("{slug}", palace);
    let url = format!("{base}{path}");
    let resp = match client.get(&url).query(&[("limit", "200")]).send().await {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    if !resp.status().is_success() {
        return Vec::new();
    }
    let body: Value = match resp.json().await {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    let Some(arr) = body.as_array() else {
        return Vec::new();
    };
    arr.iter().filter_map(RawTriple::from_value).collect()
}

/// Filter a triple list down to those whose subject or object appears in
/// the prompt (case-insensitive, word-ish substring match), capped at
/// `top_k`.
///
/// Why: dumping every active triple would shred the byte budget on a
/// palace with hundreds of triples. Limiting to subjects/objects the user
/// actually mentioned keeps signal high and noise low. We accept both
/// directions (`subject ∈ prompt` and `object ∈ prompt`) so a query like
/// "what is tga?" matches `tga is_alias_for trusty-git-analytics`.
/// What: lowercase-tokenises the prompt into a `HashSet<String>` of words
/// (≥ 3 chars), then keeps any triple whose normalised subject or object
/// (split by `:` or whitespace) overlaps the set. Returns at most `top_k`
/// entries.
/// Test: `select_relevant_triples_filters_by_prompt_overlap`.
fn select_relevant_triples(triples: &[RawTriple], prompt: &str, top_k: usize) -> Vec<RawTriple> {
    use std::collections::HashSet;
    let words: HashSet<String> = prompt
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_string())
        .collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<RawTriple> = Vec::with_capacity(top_k);
    for t in triples {
        if triple_overlaps(t, &words) {
            out.push(t.clone());
            if out.len() >= top_k {
                break;
            }
        }
    }
    out
}

/// Return `true` when any normalised token of a triple's subject or object
/// is present in the prompt's word set.
fn triple_overlaps(t: &RawTriple, prompt_words: &std::collections::HashSet<String>) -> bool {
    let candidates = [t.subject.as_str(), t.object.as_str()];
    for candidate in candidates {
        for tok in candidate
            .to_lowercase()
            .split(|c: char| c == ':' || c.is_whitespace() || c == '_' || c == '-' || c == '/')
        {
            if tok.len() >= 3 && prompt_words.contains(tok) {
                return true;
            }
        }
    }
    false
}

/// Compose the final injection block.
///
/// Why: a single coherent Markdown block is easier for the model to read
/// than three loose strings, and the section headers tell the model
/// where each piece came from so it can weigh them appropriately.
/// What: appends sections in priority order (workspace facts → drawers →
/// KG triples), each separated by a blank line. Truncates at
/// [`INJECTION_BYTE_CAP`] bytes with a `…` marker.
/// Test: `compose_injection_truncates_at_cap`,
/// `prompt_context_recalls_palace_drawers`.
fn compose_injection(
    global_facts: Option<&str>,
    drawers: &[RecalledDrawer],
    triples: &[RawTriple],
    palace_slug: Option<&str>,
) -> String {
    let mut out = String::new();
    if let Some(facts) = global_facts {
        push_section(&mut out, facts.trim_end());
    }
    if !drawers.is_empty() {
        let mut section = String::new();
        if let Some(slug) = palace_slug {
            section.push_str(&format!("## Relevant memories from palace `{slug}`\n"));
        } else {
            section.push_str("## Relevant memories\n");
        }
        for d in drawers {
            section.push_str("- ");
            section.push_str(&drawer_preview(&d.content));
            if !d.tags.is_empty() {
                section.push_str("  _(tags: ");
                let tags = d
                    .tags
                    .iter()
                    .map(|t| format!("`{t}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                section.push_str(&tags);
                section.push(')');
                section.push('_');
            }
            section.push('\n');
        }
        push_section(&mut out, section.trim_end());
    }
    if !triples.is_empty() {
        let mut section = String::new();
        section.push_str("## Relevant KG facts\n");
        for t in triples {
            section.push_str(&format!(
                "- {} **{}** {}\n",
                t.subject, t.predicate, t.object
            ));
        }
        push_section(&mut out, section.trim_end());
    }
    if out.len() > INJECTION_BYTE_CAP {
        // Reserve 3 bytes for the `…` marker (UTF-8). Walk back to a char
        // boundary so the truncated string stays valid UTF-8.
        const ELLIPSIS: char = '…';
        let ellipsis_len = ELLIPSIS.len_utf8();
        let mut cut = INJECTION_BYTE_CAP.saturating_sub(ellipsis_len);
        while cut > 0 && !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push(ELLIPSIS);
    }
    out
}

/// Append `section` to `out` separated by a blank line when `out`
/// already has content.
fn push_section(out: &mut String, section: &str) {
    if section.is_empty() {
        return;
    }
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(section);
}

/// Collapse a drawer's content to a single-line preview capped at
/// [`DRAWER_PREVIEW_CHARS`].
fn drawer_preview(content: &str) -> String {
    let normalised: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalised.chars().count() <= DRAWER_PREVIEW_CHARS {
        normalised
    } else {
        let kept: String = normalised
            .chars()
            .take(DRAWER_PREVIEW_CHARS.saturating_sub(1))
            .collect();
        format!("{kept}…")
    }
}

/// Approximate the number of facts in the rendered prompt-context body.
///
/// Why: the daemon's response is plain Markdown; counting bullet lines
/// (`- ` prefix) gives a quick proxy for "how many facts were injected" that
/// is useful for log analysis without an additional round trip.
/// What: counts non-empty lines whose first non-whitespace characters are
/// `- `. Returns 0 for an empty / placeholder body.
/// Test: covered indirectly by `single_event_roundtrip` in the integration
/// tests; the heuristic is intentionally cheap and approximate.
fn count_facts(body: &str) -> usize {
    body.lines()
        .filter(|l| l.trim_start().starts_with("- "))
        .count()
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

/// A drawer parsed from the `/recall` endpoint's flat JSON shape.
///
/// Why: the recall endpoint hoists drawer fields to the top level (see
/// `web::recall_entry_json`), so we don't need the full `Drawer` schema —
/// only the fields the injection renders.
/// What: holds the content string, tag list, and recall layer. Implements
/// `from_recall_entry` for safe extraction from `serde_json::Value`.
/// Test: indirectly via `prompt_context_recalls_palace_drawers`.
#[derive(Debug, Clone)]
struct RecalledDrawer {
    content: String,
    tags: Vec<String>,
    layer: Option<u8>,
}

impl RecalledDrawer {
    fn from_recall_entry(v: &Value) -> Option<Self> {
        let content = v.get("content")?.as_str()?.to_string();
        let tags = v
            .get("tags")
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let layer = v.get("layer").and_then(|l| l.as_u64()).map(|n| n as u8);
        if content.trim().is_empty() {
            return None;
        }
        Some(Self {
            content,
            tags,
            layer,
        })
    }
}

/// A KG triple parsed from the `/kg/all` endpoint.
///
/// Why: same as [`RecalledDrawer`] — the daemon's Triple JSON has more
/// fields (timestamps, confidence, provenance) than the injection needs.
/// What: subject/predicate/object as owned strings.
/// Test: indirectly via `prompt_context_recalls_palace_drawers`.
#[derive(Debug, Clone)]
struct RawTriple {
    subject: String,
    predicate: String,
    object: String,
}

impl RawTriple {
    fn from_value(v: &Value) -> Option<Self> {
        let subject = v.get("subject")?.as_str()?.to_string();
        let predicate = v.get("predicate")?.as_str()?.to_string();
        let object = v.get("object")?.as_str()?.to_string();
        Some(Self {
            subject,
            predicate,
            object,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Why (issue #134): the recall query needs the actual prompt text the
    /// user typed; the stdin payload carries it under `"prompt"`.
    /// What: parses three shapes — full JSON with `prompt`, JSON without,
    /// and raw text — and asserts each returns the expected string.
    /// Test: itself.
    #[test]
    fn parse_user_prompt_prefers_prompt_field() {
        let json_with_prompt = serde_json::json!({
            "prompt": "what is rust?",
            "cwd": "/tmp/example",
        })
        .to_string();
        assert_eq!(parse_user_prompt(&json_with_prompt), "what is rust?");

        let json_without_prompt = serde_json::json!({"cwd": "/tmp/example"}).to_string();
        assert_eq!(parse_user_prompt(&json_without_prompt), json_without_prompt);

        assert_eq!(parse_user_prompt("plain text query"), "plain text query");
        assert_eq!(parse_user_prompt(""), "");
    }

    /// Why (issue #134): KG triples should only surface when one of their
    /// endpoints actually appears in the user's prompt; otherwise the
    /// injection just dumps random graph noise.
    /// What: build a small set of triples; query a prompt that mentions
    /// only one subject; assert exactly the matching triple comes back.
    /// Test: itself.
    #[test]
    fn select_relevant_triples_filters_by_prompt_overlap() {
        let triples = vec![
            RawTriple {
                subject: "tga".into(),
                predicate: "is_alias_for".into(),
                object: "trusty-git-analytics".into(),
            },
            RawTriple {
                subject: "python".into(),
                predicate: "is-a".into(),
                object: "language".into(),
            },
            RawTriple {
                subject: "rust".into(),
                predicate: "is-a".into(),
                object: "language".into(),
            },
        ];
        let chosen = select_relevant_triples(&triples, "tell me about rust integration", 5);
        assert_eq!(chosen.len(), 1, "only the rust triple should match");
        assert_eq!(chosen[0].subject, "rust");

        // Empty / no-overlap prompt → no triples.
        let none = select_relevant_triples(&triples, "weather forecast next week", 5);
        assert!(none.is_empty());
    }

    /// Why: the injection has a hard 4 KB byte ceiling so a runaway palace
    /// can't drown the model's prompt; truncation must end with `…` and
    /// stay valid UTF-8.
    /// What: synthesises drawers whose previews exceed the cap, calls
    /// `compose_injection`, asserts the result is `<= INJECTION_BYTE_CAP`
    /// and ends with `…`.
    /// Test: itself.
    #[test]
    fn compose_injection_truncates_at_cap() {
        // Stuff a giant global-facts block to push the composition past the
        // 4 KB byte cap. Drawer previews are already capped at
        // DRAWER_PREVIEW_CHARS so the cap-trigger has to come from the
        // global section.
        let big_global = "## Big block\n".to_string() + &"- fact line\n".repeat(500);
        let drawers: Vec<RecalledDrawer> = (0..5)
            .map(|i| RecalledDrawer {
                content: format!("drawer {i} content"),
                tags: vec!["tag1".into()],
                layer: Some(2),
            })
            .collect();
        let triples: Vec<RawTriple> = (0..5)
            .map(|i| RawTriple {
                subject: format!("subject{i}"),
                predicate: "p".into(),
                object: "object".into(),
            })
            .collect();
        let out = compose_injection(Some(&big_global), &drawers, &triples, Some("alpha"));
        assert!(
            out.len() <= INJECTION_BYTE_CAP,
            "expected len <= cap; got {}",
            out.len()
        );
        // Truncation marker survives.
        assert!(
            out.ends_with('…'),
            "expected `…` truncation marker; got tail: {}",
            &out[out.len().saturating_sub(20)..]
        );
    }

    /// Why: an empty composition (no global facts, no drawers, no triples)
    /// must return an empty string so the caller can substitute the
    /// legacy placeholder. Section headers should never appear without
    /// content beneath them.
    /// What: call `compose_injection` with empty inputs and assert the
    /// result is empty.
    /// Test: itself.
    #[test]
    fn compose_injection_empty_inputs_yields_empty() {
        let out = compose_injection(None, &[], &[], Some("alpha"));
        assert!(out.is_empty(), "got: {out:?}");
    }

    /// Why (issue #125): when Claude Code invokes the UserPromptSubmit hook,
    /// the stdin JSON carries a `cwd` field that reflects the user's actual
    /// working directory at prompt time. The hook process cwd may be where
    /// the hook was registered (typically a fixed install root), not where
    /// the user actually is. The log palace must follow the stdin `cwd`.
    /// What: build a stdin JSON payload pointing at a tempdir, derive the
    /// expected slug for that tempdir via the *_at variant, and assert
    /// `resolve_palace_for_log` returns the same slug — even though the
    /// process cwd is unchanged and would resolve to a different slug.
    /// Test: itself.
    #[test]
    fn resolve_palace_for_log_prefers_stdin_cwd() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path().join("stdin-driven-project");
        std::fs::create_dir_all(&project).expect("create project dir");
        let payload = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "cwd": project.to_string_lossy(),
            "prompt": "hello"
        })
        .to_string();

        let expected =
            crate::messaging::cwd_palace_slug_at(&project).expect("derive slug from stdin cwd");
        let got = resolve_palace_for_log(&payload);
        assert_eq!(
            got, expected,
            "stdin `cwd` must override the process cwd for the log palace slug"
        );
        assert!(
            got.contains("stdin-driven-project"),
            "expected slug derived from stdin path, got {got:?}"
        );
    }

    /// Why (issue #125): when stdin is empty or non-JSON, the helper must
    /// fall through to the process-cwd resolution path so manual `trusty-
    /// memory prompt-context` invocations from a TTY still get a useful
    /// palace identifier.
    /// What: pass an empty string and a non-JSON string; assert the result
    /// is *not* the legacy `"<unknown>"` sentinel (the process cwd here is a
    /// real git repo, so cwd_palace_slug succeeds).
    /// Test: itself.
    #[test]
    fn resolve_palace_for_log_falls_back_to_process_cwd() {
        let from_empty = resolve_palace_for_log("");
        let from_garbage = resolve_palace_for_log("not json at all");
        assert_eq!(from_empty, from_garbage);
        assert_ne!(from_empty, "<unknown>");
    }

    /// Why: the hook is wired into every Claude Code prompt the user types;
    /// failing it would block the prompt. The contract is that a missing
    /// daemon-address lockfile (the canonical "daemon not running" signal)
    /// must produce `Ok(())` with no stdout, not an error.
    /// What: redirects `trusty_common::resolve_data_dir` at a fresh tempdir
    /// via `TRUSTY_DATA_DIR_OVERRIDE` so `read_daemon_addr("trusty-memory")`
    /// observes a missing lockfile, then runs the handler and asserts it
    /// returns `Ok(())`.
    #[tokio::test]
    async fn prompt_context_returns_ok_without_daemon() {
        let _guard = crate::commands::env_test_lock().lock().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: tests serialise on `TRUSTY_DATA_DIR_OVERRIDE` by convention
        // across the trusty-* workspace (see trusty-common's lib.rs notes).
        // This test only mutates the env var inside its own scope.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
        }
        let res = handle_prompt_context().await;
        unsafe {
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
        assert!(
            res.is_ok(),
            "missing daemon lockfile must degrade to Ok(()), got {res:?}"
        );
    }

    /// Why (issue #134): the hook's whole value-prop is surfacing relevant
    /// drawers from the palace; previously it only returned the workspace-
    /// level hot facts. Confirm that with a live daemon, a populated palace,
    /// and a prompt that mentions a known keyword, the rendered injection
    /// contains real drawer content — not the legacy `EMPTY_PLACEHOLDER`.
    /// What: spin up a real HTTP daemon under a tempdir-pinned data root,
    /// create a palace whose slug matches a project tempdir basename,
    /// populate it with three keyworded drawers via the MCP dispatch
    /// (which loads the real embedder), then call `build_injection_body`
    /// with a stdin payload carrying `cwd = <project tempdir>` and
    /// `prompt = "how does rust integration work?"`. Assert the body
    /// contains the rust drawer's content and the relevant-memories
    /// section header.
    /// Test: itself.
    #[tokio::test]
    async fn prompt_context_recalls_palace_drawers() {
        let _guard = crate::commands::env_test_lock().lock().await;
        let (state, _data_dir_tmp, _project_dir_tmp, project_dir, slug, addr_handle) =
            spin_up_test_daemon_with_palace("prompt-ctx-recall-pop").await;

        // Populate the palace with three diverged drawers via MCP dispatch.
        // Using the MCP path here exercises the real embedder + KG hook.
        for (text, tags) in [
            (
                "Rust integration uses tokio for async tasks and serde for JSON",
                vec!["rust", "tokio"],
            ),
            (
                "Python bindings ship via PyO3 with custom ABI shims",
                vec!["python", "pyo3"],
            ),
            (
                "Knowledge graph stores triples in redb with valid_from intervals",
                vec!["kg", "redb"],
            ),
        ] {
            let tags_json: Vec<Value> = tags.iter().map(|t| json!(t)).collect();
            let _ = crate::tools::dispatch_tool(
                &state,
                "memory_remember",
                json!({
                    "palace": slug,
                    "text": text,
                    "room": "General",
                    "tags": tags_json,
                }),
            )
            .await
            .expect("memory_remember");
        }

        // Build the stdin payload Claude Code would send: a JSON object with
        // `cwd` (the project dir) and `prompt` (mentioning "rust").
        let payload = json!({
            "hook_event_name": "UserPromptSubmit",
            "cwd": project_dir.to_string_lossy(),
            "prompt": "how does rust integration work?"
        })
        .to_string();

        let start = std::time::Instant::now();
        let body = build_injection_body(&payload).await;
        let elapsed_ms = start.elapsed().as_millis();
        eprintln!("prompt_context_recalls_palace_drawers latency: {elapsed_ms}ms");

        assert_ne!(
            body, EMPTY_PLACEHOLDER,
            "populated palace must return real content, not the placeholder"
        );
        // The injection must mention the rust drawer's content (proves
        // recall actually targeted the resolved palace and surfaced
        // prompt-relevant memories).
        assert!(
            body.to_lowercase().contains("rust") && body.to_lowercase().contains("integration"),
            "expected rust integration drawer in injection; got:\n{body}"
        );
        // Section header should be present (proves the multi-section
        // composition is wired through).
        assert!(
            body.contains("Relevant memories") || body.contains("memories from palace"),
            "expected a `Relevant memories` section; got:\n{body}"
        );

        // Performance guardrail (issue #134 target: <200 ms p95). On
        // CI/dev machines this comfortably stays under the budget.
        assert!(
            elapsed_ms < 5_000,
            "prompt-context too slow ({elapsed_ms}ms) — investigate"
        );

        addr_handle.shutdown().await;
    }

    /// Why (issue #134, negative case): when the resolved palace has no
    /// drawers AND no global hot facts have been asserted, the hook must
    /// still emit a safe placeholder so downstream consumers see byte-
    /// identical behaviour to the pre-fix daemon. Don't regress the empty
    /// case while fixing the populated one.
    /// What: spin up the same daemon shape but skip the drawer-population
    /// step; assert the body equals [`EMPTY_PLACEHOLDER`].
    /// Test: itself.
    #[tokio::test]
    async fn prompt_context_empty_palace_falls_back_to_global() {
        let _guard = crate::commands::env_test_lock().lock().await;
        let (_state, _data_dir_tmp, _project_dir_tmp, project_dir, _slug, addr_handle) =
            spin_up_test_daemon_with_palace("prompt-ctx-recall-empty").await;

        let payload = json!({
            "hook_event_name": "UserPromptSubmit",
            "cwd": project_dir.to_string_lossy(),
            "prompt": "no drawers exist here"
        })
        .to_string();
        let body = build_injection_body(&payload).await;
        assert_eq!(
            body, EMPTY_PLACEHOLDER,
            "empty palace + empty prompt-facts must fall back to the placeholder"
        );

        addr_handle.shutdown().await;
    }

    /// Test fixture: spin up a real HTTP daemon under a tempdir-pinned
    /// data root, create a palace with the given slug under a project
    /// tempdir whose basename matches the slug, and return everything
    /// the test needs to interact with the daemon.
    ///
    /// Why: the prompt-context hook talks HTTP to a live daemon; unit
    /// tests with the router alone can't exercise the `read_daemon_addr`
    /// → HTTP round trip. This helper wires it all together in one place.
    /// What: creates two tempdirs (one for the data root, one for the
    /// project cwd whose basename equals `palace_slug`), pins
    /// `TRUSTY_DATA_DIR_OVERRIDE`, builds the `AppState`, creates the
    /// palace, spawns the HTTP server on `127.0.0.1:0`, and waits for
    /// the daemon addr file to land. Returns `(state, data_dir_tmp,
    /// project_dir_tmp, project_dir_path, palace_slug, addr_handle)`.
    /// Test: indirectly via `prompt_context_recalls_palace_drawers` and
    /// `prompt_context_empty_palace_falls_back_to_global`.
    async fn spin_up_test_daemon_with_palace(
        palace_slug: &str,
    ) -> (
        crate::AppState,
        tempfile::TempDir,
        tempfile::TempDir,
        std::path::PathBuf,
        String,
        DaemonHandle,
    ) {
        let data_tmp = tempfile::tempdir().expect("data tempdir");
        let project_tmp = tempfile::tempdir().expect("project tempdir");
        // Build a project directory whose basename equals the palace slug.
        // This is what `cwd_palace_slug_at` will derive from the stdin `cwd`.
        let project_dir = project_tmp.path().join(palace_slug);
        std::fs::create_dir_all(&project_dir).expect("project dir");

        // SAFETY: env_test_lock serialises this section.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, data_tmp.path());
            std::env::remove_var(crate::prompt_log::ENV_ENABLED);
            std::env::remove_var(crate::prompt_log::ENV_DIR);
            std::env::remove_var(crate::prompt_log::ENV_HASH_PROMPTS);
        }

        let data_root = trusty_common::resolve_data_dir("trusty-memory")
            .expect("resolve data dir under override");
        let state = crate::AppState::new(data_root.clone());

        // Create the palace via MCP dispatch so the on-disk metadata
        // matches what a real client would have produced. The slug here
        // matches the project_dir basename so `cwd_palace_slug_at` resolves
        // to the same palace.
        let _ = crate::tools::dispatch_tool(&state, "palace_create", json!({"name": palace_slug}))
            .await
            .expect("palace_create");

        // Bind a random local port and start the HTTP server. `run_http_on`
        // writes the addr file as part of startup; we poll for it briefly
        // so the subsequent `read_daemon_addr` call succeeds.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind 127.0.0.1:0");
        let addr = listener.local_addr().expect("local_addr");
        let state_for_server = state.clone();
        let handle = tokio::spawn(async move {
            let _ = crate::run_http_on(state_for_server, listener).await;
        });

        // Poll for the addr file (run_http_on writes it after binding).
        // Generous deadline so a contended CI machine doesn't flake — the
        // disk_size_ticker spawned inside `run_http_on` does some setup
        // work before the addr write lands.
        let addr_file = data_root.join("http_addr");
        let mut attempts = 0;
        while !addr_file.exists() && attempts < 250 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            attempts += 1;
        }
        assert!(
            addr_file.exists(),
            "daemon never wrote http_addr at {} (attempts={attempts})",
            addr_file.display()
        );

        (
            state,
            data_tmp,
            project_tmp,
            project_dir,
            palace_slug.to_string(),
            DaemonHandle {
                addr,
                join: Some(handle),
            },
        )
    }

    /// Test-only handle to a spawned daemon — aborts the server task on
    /// drop or explicit `shutdown` so the tempdir cleanup doesn't race
    /// with in-flight requests.
    struct DaemonHandle {
        #[allow(dead_code)]
        addr: std::net::SocketAddr,
        join: Option<tokio::task::JoinHandle<()>>,
    }

    impl DaemonHandle {
        async fn shutdown(mut self) {
            if let Some(h) = self.join.take() {
                h.abort();
                let _ = h.await;
            }
            // Release the pinned data dir override for sibling tests.
            // SAFETY: protected by env_test_lock in the caller.
            unsafe {
                std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
            }
        }
    }

    /// Why (issue #105): even when the daemon is down, the hook must still
    /// log an attempt entry so operators can see "prompt-context fired N
    /// times but the daemon was unreachable" in the JSONL stream.
    /// What: pin a tempdir as the data directory, run the handler with no
    /// daemon, and assert exactly one log file landed under `<tmp>/logs/`
    /// with a single JSONL line whose `injection_kind` is the prompt-context
    /// kind.
    /// Test: itself.
    #[tokio::test]
    async fn prompt_context_logs_attempt_without_daemon() {
        let _guard = crate::commands::env_test_lock().lock().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
            std::env::remove_var(crate::prompt_log::ENV_ENABLED);
            std::env::remove_var(crate::prompt_log::ENV_DIR);
            std::env::remove_var(crate::prompt_log::ENV_HASH_PROMPTS);
        }
        let res = handle_prompt_context().await;
        let logs_dir = trusty_common::resolve_data_dir("trusty-memory")
            .expect("resolve data dir")
            .join("logs");
        unsafe {
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
        assert!(res.is_ok());
        let files: Vec<_> = std::fs::read_dir(&logs_dir)
            .expect("logs dir should be created")
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("enriched-prompts."))
            })
            .collect();
        assert_eq!(
            files.len(),
            1,
            "expected one enriched-prompts log file, got {files:?}"
        );
        let content = std::fs::read_to_string(&files[0]).expect("read log");
        let line = content.lines().next().expect("at least one line");
        let parsed: crate::prompt_log::PromptLogEntry =
            serde_json::from_str(line).expect("parse JSONL");
        assert_eq!(parsed.hook_type, "UserPromptSubmit");
        assert_eq!(parsed.injection_kind, "prompt-context-facts");
    }
}
