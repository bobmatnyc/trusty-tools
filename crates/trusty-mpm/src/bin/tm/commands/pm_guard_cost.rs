//! `tm hook --pm-guard` — per-subagent context ceiling (issue #4837).
//!
//! Why: the policy half of this guard lives in
//! [`trusty_mpm::core::agent_cost`] and explains the cost model. This module is
//! the I/O half: it answers "how many tokens is the agent making this call
//! actually carrying?" from the `PreToolUse` payload, which is the only place
//! the guard can see it. Claude Code's `PreToolUse` payload carries **no token
//! counts** — the empirically-confirmed field set is `session_id`,
//! `transcript_path`, `cwd`, `prompt_id`, `permission_mode`, `hook_event_name`,
//! `tool_name`, `tool_input`, `tool_use_id`, plus `agent_id` inside a subagent
//! (see [`super::hook_payload`]'s capture notes). The counts live one hop away,
//! in the transcript that payload points at, which is why this module resolves
//! a path rather than reading a field.
//!
//! What: [`resolve_agent_transcript`] finds the calling *subagent's* own
//! transcript; [`evaluate_agent_cost`] tail-reads it under a hard timeout and
//! classifies the result via [`evaluate_cost`]; [`is_persistence_escape`]
//! answers whether a stopped agent's tool call is one of the few that stay
//! permitted so it can save and report its work.
//!
//! **Fails OPEN at every step**, per the policy module's asymmetry note: not a
//! subagent, no resolvable transcript, an unreadable or truncated file, a tail
//! with no usage record, or a disabled/zeroed config all yield
//! [`BudgetStatus::Ok`]. The PM is never a subagent and so is never evaluated
//! at all — a bug here cannot halt orchestration.
//!
//! #7278 added the one thing that path was missing: the resolved transcript is
//! screened for containment under the Claude config directory before a byte of
//! it is read, because every candidate field comes from the payload. Fail-open
//! is unchanged, but a REFUSAL is no longer silent — [`AgentCost`] carries it
//! back so the caller says out loud that it allowed without measuring, rather
//! than reporting a 0 it never read.
//!
//! Test: `resolves_*`/`fails_open_*` below cover path resolution and the
//! fail-open matrix; the threshold policy is pinned in
//! [`trusty_mpm::core::agent_cost`]'s own suite.

use std::path::{Path, PathBuf};

use trusty_mpm::core::agent_cost::{AgentCostConfig, BudgetStatus, evaluate_cost};

/// Cap on transcript bytes read on the FIRST tail pass.
///
/// Why: the newest `usage` block sits at the very end of the JSONL, so a small
/// window is usually sufficient — and bounding it is what keeps the guard's
/// cost constant on the multi-hundred-megabyte transcripts it exists to catch.
/// 64 KiB spans several assistant turns in the common case and is the cheap
/// path this guard takes on nearly every call.
/// What: byte count passed to [`super::misc::read_transcript_tail`] first; when
/// it yields no usage record the read is retried once at
/// [`RETRY_TRANSCRIPT_TAIL`].
/// Test: `retries_with_a_larger_tail_when_64k_holds_no_usage_record`; the
/// truncated-line tolerance it relies on is pinned by
/// `core::agent_cost::tolerates_a_truncated_leading_line`.
const MAX_TRANSCRIPT_TAIL: u64 = 64 * 1024;

/// Cap on transcript bytes read on the RETRY pass.
///
/// Why (#4837 review, MEDIUM): 64 KiB alone degrades on exactly the transcripts
/// this guard exists to catch. Measured on a working machine, 1 of the 12
/// largest subagent transcripts carried no complete `usage` record in its final
/// 64 KiB — a single oversized tool result at the tail is enough to push the
/// newest assistant turn out of the window — and a missing record fails OPEN,
/// so coverage silently drops off at the top of the distribution. Retrying once
/// at 16x restores it without making the common case pay: the second read
/// happens only when the first found nothing, and it is still a bounded
/// constant, so the guard's cost stays independent of transcript size.
/// What: byte count for the second [`super::misc::read_transcript_tail`] call.
/// 1 MiB spans roughly twenty maximum-size tool results; beyond that the file
/// is pathological in a way this guard is not the right place to diagnose, and
/// the fail-open answer is correct again.
/// Test: `retries_with_a_larger_tail_when_64k_holds_no_usage_record`,
/// `still_fails_open_when_even_the_larger_tail_has_no_record`.
const RETRY_TRANSCRIPT_TAIL: u64 = 1024 * 1024;

/// Hard ceiling on the whole cost evaluation.
///
/// Why: `PreToolUse` is configured with a 5-second timeout and runs before
/// EVERY tool call, so the guard must be invisible in the common case. 200 ms
/// is far above a 64 KiB tail read from page cache and far below the point at
/// which a user would notice; blowing it fails open rather than stalling.
/// What: timeout wrapped around the tail read, passed to
/// [`evaluate_agent_cost_in`] by the production entry point.
/// Test: `fails_open_when_the_transcript_is_missing` covers the failure branch
/// this timeout shares;
/// `the_production_budget_stays_far_inside_the_pretooluse_hook_timeout` pins
/// the two bounds the value has to sit between.
const EVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

/// Locate the transcript of the subagent issuing this `PreToolUse` call.
///
/// Why: this is the whole difficulty of #4837 on the observation side. The
/// guard must measure the *subagent's* context, and reading the parent's
/// transcript instead would charge a cheap agent for its PM's history — a
/// false stop, the failure mode this design refuses. Claude Code has changed
/// which path field it emits between releases (`SubagentStop` carries the
/// subagent's transcript as `agent_transcript_path` while `transcript_path` is
/// the *parent's*), so rather than pin one field and silently no-op if it
/// moves, this tries three shapes and accepts only a path that exists on disk.
/// What, in order: (1) an explicit `agent_transcript_path`; (2) a
/// `transcript_path` that already sits under a `subagents/` directory, i.e.
/// the field already points at the subagent; (3) the documented layout
/// `<dir>/<parent-stem>/subagents/agent-<agent_id>.jsonl` derived from
/// `transcript_path` + `agent_id` — confirmed against a live transcript tree
/// on 2026-08-04. Returns `None` when no candidate resolves to an existing
/// file, which is the FAIL-OPEN answer.
/// Test: `resolves_explicit_agent_transcript_path`,
/// `resolves_a_transcript_path_already_under_subagents`,
/// `derives_the_subagent_path_from_agent_id`,
/// `fails_open_without_any_transcript_field`.
fn agent_transcript_candidate(payload: &serde_json::Value) -> Option<PathBuf> {
    let field = |k: &str| {
        payload
            .get(k)
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
    };

    // 1. The field that names it outright.
    if let Some(p) = field("agent_transcript_path") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }

    let parent = field("transcript_path")?;
    let parent = Path::new(parent);

    // 2. Already the subagent's own transcript.
    if parent
        .parent()
        .is_some_and(|d| d.file_name().is_some_and(|n| n == "subagents"))
        && parent.is_file()
    {
        return Some(parent.to_path_buf());
    }

    // 3. Derive it from the documented layout.
    let agent_id = field("agent_id")?;
    let stem = parent.file_stem()?;
    let derived = parent
        .parent()?
        .join(stem)
        .join("subagents")
        .join(format!("agent-{agent_id}.jsonl"));
    derived.is_file().then_some(derived)
}

/// What [`resolve_agent_transcript_in`] concluded about the payload's path.
///
/// Why (#7278): "no transcript" and "a transcript this guard refuses to open"
/// were the same answer — `None` — and both reached [`evaluate_agent_cost`]'s
/// fail-open arm as a silent `(Ok, 0)`. They are not the same event. The first
/// is the ordinary case for a PM or a fresh agent; the second means a payload
/// aimed the guard's read at a file outside the Claude config directory, which
/// nothing legitimate does. Keeping them distinct is what lets the caller say
/// so out loud instead of allowing on a number it never measured.
/// What: `Contained` carries the CANONICAL screened path — the only variant
/// whose bytes are ever read. `Refused` carries the candidate's own spelling,
/// unread, for callers that need an identity string rather than a file (see
/// [`warn_notice_key`]). `Absent` is no candidate at all.
/// Test: `refuses_a_transcript_outside_the_config_dir`,
/// `refuses_every_transcript_without_a_config_dir`,
/// `fails_open_without_any_transcript_field`,
/// `resolves_explicit_agent_transcript_path`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AgentTranscript {
    /// Screened and contained — safe to read.
    Contained(PathBuf),
    /// A candidate the containment screen rejected. Never opened.
    Refused(PathBuf),
    /// No candidate field resolved to an existing file.
    Absent,
}

impl AgentTranscript {
    /// The path whose bytes may be read, if any.
    fn contained(&self) -> Option<&Path> {
        match self {
            Self::Contained(p) => Some(p),
            Self::Refused(_) | Self::Absent => None,
        }
    }

    /// The path a candidate named, screened or not — for identity only.
    ///
    /// Why: [`warn_notice_key`] needs a per-agent string, not a file, and
    /// derives it from the transcript's stem. A refused path still identifies
    /// the agent, and nothing here opens it, so refusing to hand it back would
    /// re-introduce the #4850 collision (every sibling of one parent falling
    /// back to the shared `session_id`) for no security gain.
    /// Test: `warn_notice_is_claimed_per_sibling_not_per_parent_session`.
    fn candidate(&self) -> Option<&Path> {
        match self {
            Self::Contained(p) | Self::Refused(p) => Some(p),
            Self::Absent => None,
        }
    }
}

/// [`resolve_agent_transcript_in`] against the process's own config directory.
///
/// What: resolves the boundary through
/// [`trusty_mpm::core::session_record::claude_config_dir`] and delegates.
/// Test: `refuses_a_transcript_outside_the_config_dir`.
pub(crate) fn resolve_agent_transcript(payload: &serde_json::Value) -> AgentTranscript {
    resolve_agent_transcript_in(
        payload,
        trusty_mpm::core::session_record::claude_config_dir().as_deref(),
    )
}

/// Find the calling subagent's transcript and screen it for containment.
///
/// Why (#7278): [`agent_transcript_candidate`] takes three path fields straight
/// from the `PreToolUse` payload and checked only `is_file()`, so a crafted
/// `transcript_path` steered [`evaluate_agent_cost`]'s read — and through it the
/// guard's halt/warn/ok decision — at any regular file the `tm` user could
/// read. The candidate logic is unchanged; what is added is the screen between
/// finding a path and reading it. The boundary arrives as an argument because
/// this bin target may not write `CLAUDE_CONFIG_DIR` or `HOME` (#5544), so the
/// rule is otherwise unassertable.
/// What: screens the candidate with
/// [`trusty_mpm::core::session_record::contained_transcript_file`] — absolute,
/// no `..`, canonicalizing under the canonicalized `claude_config_dir` — and
/// returns [`AgentTranscript::Contained`] with the canonical path on success.
/// A candidate that fails, and every candidate when `claude_config_dir` is
/// `None` (no boundary, so nothing to contain against — #7290), comes back as
/// [`AgentTranscript::Refused`] and is never opened.
/// Test: `refuses_a_transcript_outside_the_config_dir`,
/// `refuses_a_traversing_transcript_path`,
/// `refuses_every_transcript_without_a_config_dir`,
/// `resolves_explicit_agent_transcript_path`,
/// `resolves_a_transcript_path_already_under_subagents`,
/// `derives_the_subagent_path_from_agent_id`,
/// `fails_open_without_any_transcript_field`.
pub(crate) fn resolve_agent_transcript_in(
    payload: &serde_json::Value,
    claude_config_dir: Option<&Path>,
) -> AgentTranscript {
    let Some(candidate) = agent_transcript_candidate(payload) else {
        return AgentTranscript::Absent;
    };
    let Some(root) = claude_config_dir else {
        return AgentTranscript::Refused(candidate);
    };
    match trusty_mpm::core::session_record::contained_transcript_file(root, &candidate) {
        Some(canonical) => AgentTranscript::Contained(canonical),
        None => AgentTranscript::Refused(candidate),
    }
}

/// What [`evaluate_agent_cost`] measured, and what it refused to measure.
///
/// Why (#7278 Fail-Open Check): the evaluator used to hand back a bare
/// `(BudgetStatus, u64)`, so a refused transcript was indistinguishable from a
/// healthy agent at 0 tokens — the guard allowed on a number it had never read.
/// Fail-open is still the right decision (a broken counter must not halt real
/// work), but it has to be a decision the caller can SEE. `refused_transcript`
/// is that signal: `Some(path)` means "allowed without measuring, because the
/// payload named a file outside the Claude config directory".
/// What: `status` and `tokens` as before, plus the refused candidate when one
/// existed. `refused_transcript` is `None` on every ordinary path, including a
/// genuinely absent transcript.
/// Test: `refusal_is_surfaced_not_silently_allowed`,
/// `fails_open_when_the_transcript_is_missing`,
/// `allows_a_healthy_agent`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AgentCost {
    pub(crate) status: BudgetStatus,
    pub(crate) tokens: u64,
    pub(crate) refused_transcript: Option<PathBuf>,
}

impl AgentCost {
    /// The fail-open answer with nothing to report.
    fn allow() -> Self {
        Self {
            status: BudgetStatus::Ok,
            tokens: 0,
            refused_transcript: None,
        }
    }
}

/// Say out loud that this call was allowed without being measured.
///
/// Why (#7278): the refusal is only useful if someone sees it. A loud,
/// greppable stderr line is the same channel the idle-parking back-stop uses,
/// so an operator reading a session log finds both the same way. It lives here
/// rather than at the [`super::pm_guard`] call site because that file sits one
/// line under the 500-SLOC production cap, and because the wording belongs with
/// the type that carries the refusal.
/// What: prints nothing on an ordinary verdict; on a refusal, names the session
/// and the rejected path and states that the guard allowed the call anyway.
/// Test: `refusal_is_surfaced_not_silently_allowed` pins the signal this reads.
pub(crate) fn warn_if_transcript_refused(session_id: &str, cost: &AgentCost) {
    if let Some(path) = &cost.refused_transcript {
        eprintln!(
            "trusty-mpm: AGENT-COST TRANSCRIPT REFUSED (#7278) session={session_id} \
             path={} — the payload named a transcript outside the Claude config \
             directory; the guard ALLOWED this call without measuring its context.",
            path.display()
        );
    }
}

/// Measure and classify the calling subagent's context spend.
///
/// Why: split from [`resolve_agent_transcript`] so the (I/O-bound, timeout-
/// wrapped) read is separable from the (pure, exhaustively-tested) path logic.
/// Called only after the caller has been positively identified as a subagent,
/// so the PM path pays neither the config load nor the file read.
/// What: resolves the transcript, tail-reads it under [`EVAL_TIMEOUT`] —
/// [`MAX_TRANSCRIPT_TAIL`] first, retried once at [`RETRY_TRANSCRIPT_TAIL`]
/// when that window holds no complete usage record — extracts the newest
/// context size, and returns an [`AgentCost`]. Any failure returns
/// [`AgentCost::allow`] — fail open.
/// Test: `fails_open_when_the_transcript_is_missing`,
/// `respects_a_disabled_config`.
pub(crate) async fn evaluate_agent_cost(
    payload: &serde_json::Value,
    config: &AgentCostConfig,
) -> AgentCost {
    // #7278: the boundary the payload's transcript_path is screened against.
    evaluate_agent_cost_in(
        payload,
        config,
        trusty_mpm::core::session_record::claude_config_dir().as_deref(),
        EVAL_TIMEOUT,
    )
    .await
}

/// [`evaluate_agent_cost`] against an explicit config directory and deadline.
///
/// Why (#7278): the containment boundary is resolved from the process
/// environment, and this bin target may not write `CLAUDE_CONFIG_DIR` or `HOME`
/// (#5544) — so every test of the screen, and every test that must still reach
/// the read past it, needs that directory as an argument.
/// Why (#7028): [`EVAL_TIMEOUT`] is a 200 ms *production latency* budget, and a
/// test that reads a fixture through it is racing a wall clock rather than
/// exercising this module. Expiry fails open, so the loser reads as
/// [`AgentCost::allow`] — indistinguishable from a resolution bug in one
/// direction and, in `still_fails_open_when_even_the_larger_tail_has_no_record`,
/// a false GREEN in the other. It cost `allows_a_healthy_agent` a red main on
/// run 34407013504: that one test took 0.335 s in the `bin/tm` target while
/// every sibling in the same process took 0.010–0.015 s, so the guard hit the
/// deadline and reported 0 against an expected 71540. Passing the budget in lets
/// a test whose subject is the read hold the deadline still, without moving the
/// value that ships. What the expired arm itself answers is [`verdict`]'s to
/// pin.
/// What: as [`evaluate_agent_cost`], with the boundary and the deadline
/// supplied. A disabled config still short-circuits before any resolution, so a
/// disabled guard touches the filesystem no more than it did. A refused
/// transcript returns the fail-open verdict with `refused_transcript` set,
/// having read nothing. `budget` bounds both tail reads together, exactly as
/// [`EVAL_TIMEOUT`] does in production.
/// Test: `refusal_is_surfaced_not_silently_allowed`,
/// `refuses_a_transcript_outside_the_config_dir`,
/// `reports_exceeded_for_an_over_ceiling_transcript`,
/// `allows_a_healthy_agent`,
/// `retries_with_a_larger_tail_when_64k_holds_no_usage_record`,
/// `still_fails_open_when_even_the_larger_tail_has_no_record`.
async fn evaluate_agent_cost_in(
    payload: &serde_json::Value,
    config: &AgentCostConfig,
    claude_config_dir: Option<&Path>,
    budget: std::time::Duration,
) -> AgentCost {
    if !config.enabled {
        return AgentCost::allow();
    }
    let resolved = resolve_agent_transcript_in(payload, claude_config_dir);
    let Some(path) = resolved.contained() else {
        return AgentCost {
            refused_transcript: resolved.candidate().map(Path::to_path_buf),
            ..AgentCost::allow()
        };
    };
    let measured = tokio::time::timeout(budget, read_latest_context(path))
        .await
        .ok()
        .flatten();
    verdict(measured, config)
}

/// The guard's answer once the measurement has either landed or not.
///
/// Why (#7028): `None` here is the expired-deadline case, and it is the one
/// arm no test can reach on purpose — a deadline short enough to be certain of
/// expiring is also short enough for the read to beat it, so asserting through
/// the real clock flakes in whichever direction the machine happens to run.
/// (`Duration::ZERO` did: tokio rounds a deadline up to the next 1 ms tick, and
/// a warm page-cache read lands inside it.) Splitting the mapping out puts the
/// property that actually matters — an unmeasured agent is never denied — on a
/// pure function that answers the same way on every machine.
/// What: `Some` classifies via [`evaluate_cost`] and reports the measurement;
/// `None` — timed out, unreadable, or no usage record in either window — is
/// [`AgentCost::allow`]. A transcript this function was reached for was
/// screened and contained (#7278), so `refused_transcript` is `None` on both
/// arms; refusal returns earlier, in [`evaluate_agent_cost_in`].
/// Test: `an_unmeasured_read_never_denies`.
fn verdict(measured: Option<u64>, config: &AgentCostConfig) -> AgentCost {
    match measured {
        Some(tokens) => AgentCost {
            status: evaluate_cost(tokens, config),
            tokens,
            refused_transcript: None,
        },
        None => AgentCost::allow(),
    }
}

/// Two-pass tail read yielding the newest context size, or `None`.
///
/// Why (#4837 review, MEDIUM): split out of [`evaluate_agent_cost`] so the
/// whole retry sits inside the single [`EVAL_TIMEOUT`] — the guard's latency
/// budget is per-call, not per-read, so a slow disk cannot turn the retry into
/// two timeouts. Ordering matters: the cheap 64 KiB read is tried first and
/// the 1 MiB read happens only when it found nothing, which is rare.
/// What: reads [`MAX_TRANSCRIPT_TAIL`] bytes and parses; on no record, reads
/// [`RETRY_TRANSCRIPT_TAIL`] bytes and parses again. `None` — fail open — when
/// both come up empty or the file is unreadable.
/// Test: `retries_with_a_larger_tail_when_64k_holds_no_usage_record`,
/// `still_fails_open_when_even_the_larger_tail_has_no_record`.
async fn read_latest_context(path: &Path) -> Option<u64> {
    use trusty_mpm::core::agent_cost::latest_context_tokens;

    for bytes in [MAX_TRANSCRIPT_TAIL, RETRY_TRANSCRIPT_TAIL] {
        let jsonl = super::misc::read_transcript_tail(path, bytes).await?;
        if let Some(tokens) = latest_context_tokens(&jsonl) {
            return Some(tokens);
        }
        // A short file cannot hide anything in a bigger window — the first
        // read already covered it, so skip the pointless second pass.
        if (jsonl.len() as u64) < bytes {
            return None;
        }
    }
    None
}

/// Whether this tool call is the stopped agent's escape hatch.
///
/// Why (#4837 review, BLOCK 1(b)): the `Exceeded` arm denied every tool, so an
/// agent that had produced a correct fix could not commit it, push it, or
/// report it — the deny text pointed at a channel the same deny had closed.
/// Traced against a real case: the #4841 engineer reached 434k while producing
/// a correct fix and would have been stranded. A guard that strands work is
/// worse than the overrun it prevents, so the stop keeps a narrow allowlist
/// open. An allowlist beats a one-shot grace budget on shape, not on cost:
/// [`claim_warn_notice`] already keeps durable per-agent state, so "the hook is
/// stateless" is not the argument — it was in the first cut of this doc, and the
/// marker file makes it false. The reason that stands on its own is what each
/// hatch lets through: a grace *count* is spendable on anything the agent likes,
/// while naming the tools makes the hatch exactly as wide as "persist and
/// report" and no wider.
/// What: `true` for `is_persistence_tool` (`SendMessage`), and for `Bash`
/// when [`command_is_persistence_only`](crate::commands::pm_guard_bash::command_is_persistence_only) proves every segment of its command is
/// an allowlisted git call carrying no exec-capable flag — see that function's
/// module docs for the four bypass classes the #4850 review closed. Everything
/// else is `false` → denied.
/// Test: `escape_hatch_permits_send_message_and_git_persistence`,
/// `escape_hatch_denies_work_tools`, `escape_hatch_denies_exec_capable_git`.
pub(crate) fn is_persistence_escape(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
) -> bool {
    use trusty_mpm::core::agent_cost::is_persistence_tool;

    if is_persistence_tool(tool_name) {
        return true;
    }
    if tool_name != "Bash" {
        return false;
    }
    let command = tool_input
        .and_then(|v| v.get("command"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    super::pm_guard_bash::command_is_persistence_only(command)
}

/// Claim the right to show this subagent its cost warning, once.
///
/// Why (#4837 review, HIGH): the warning now reaches the agent through
/// `hookSpecificOutput.additionalContext`, which is the fix — but emitting it
/// on EVERY call while the agent sits in the warn band would re-send the same
/// ~90 tokens for the rest of that agent's life, spending context to complain
/// about context. Since the shipped default has no hard stop, the warn band is
/// unbounded and that spam would be unbounded with it. A single filesystem
/// marker per agent turns the notice into what it is meant to be: one nudge at
/// the moment the threshold is crossed. `create_new` makes the claim atomic, so
/// concurrent tool calls from the same agent cannot both win it.
/// What: `true` the first time it is called for a given agent (see
/// [`warn_notice_key`]), `false` afterwards. Any I/O failure — and any payload
/// the key cannot be derived from — returns `true`, failing toward informing the
/// agent, since a missed nudge is worse than a duplicated one. Markers live in
/// the OS temp dir, which is reaped for us; the only cost of losing them early
/// is one extra nudge.
/// Test: `warn_notice_is_claimed_once_per_agent`,
/// `warn_notice_is_claimed_per_sibling_not_per_parent_session`,
/// `warn_notice_fails_open_when_no_key_can_be_derived`.
pub(crate) fn claim_warn_notice(payload: &serde_json::Value) -> bool {
    let Some(key) = warn_notice_key(payload) else {
        return true;
    };
    let dir = std::env::temp_dir().join("trusty-mpm-agent-cost");
    if std::fs::create_dir_all(&dir).is_err() {
        return true;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(key))
    {
        Ok(_) => true,
        // Already claimed — this agent has been told.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(_) => true,
    }
}

/// A marker-file name that identifies ONE agent, or `None` to fail open.
///
/// Why (#4850 review, MEDIUM): the marker mechanism is per-key, so the key has
/// to be per-agent — and `agent_id` → `session_id` is not. In
/// [`resolve_agent_transcript`]'s case 2 the payload's `transcript_path`
/// already points at the subagent and there is no `agent_id` at all, so every
/// sibling of one parent fell back to the SAME `session_id`: the first to cross
/// the warn threshold claimed the marker and silenced all the others. The
/// subagent's own transcript path is the identity the resolver just established,
/// so its file stem (`agent-<id>`) is the key that case needs.
///
/// Why `None` rather than a constant fallback (#4850 review, LOW): the earlier
/// `.unwrap_or("unknown")` plus a character filter could produce an EMPTY key,
/// and `dir.join("")` is `dir` itself — `create_new` on the existing directory
/// returns `AlreadyExists`, i.e. `false`, i.e. the notice suppressed
/// permanently, for every agent at once. That is the exact inverse of the
/// documented "any I/O failure returns `true`". An underivable key now says so
/// and the caller fails open.
/// What: `agent_id` if present, else the resolved subagent transcript's file
/// stem, else `session_id`; filtered to ASCII alphanumerics and `-` and capped
/// at 64 characters so it is a safe single path component. `None` when no field
/// yields anything, or when filtering leaves nothing.
/// Test: `warn_notice_is_claimed_per_sibling_not_per_parent_session`,
/// `warn_notice_fails_open_when_no_key_can_be_derived`.
fn warn_notice_key(payload: &serde_json::Value) -> Option<String> {
    let field = |k: &str| {
        payload
            .get(k)
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    // #7278: identity only — `candidate()` hands back a REFUSED path too,
    // because nothing here opens it and dropping it would re-introduce #4850's
    // sibling collision on `session_id`.
    let raw = field("agent_id")
        .or_else(|| {
            resolve_agent_transcript(payload)
                .candidate()
                .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        })
        .or_else(|| field("session_id"))?;
    let key: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(64)
        .collect();
    (!key.is_empty()).then_some(key)
}

// Temp dirs come from `crate::test_support::hermetic_temp_dir`, never a bare
// `tempfile::tempdir()`: the bare constructor honors `$TMPDIR`, so a sibling
// test mutating it reddens these (PR #4914 run 31023632348) — see `test_support`.
#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::core::agent_cost::stop_reason;

    /// Read deadline for every test whose subject is the READ, not the clock.
    ///
    /// Why (#7028): see [`evaluate_agent_cost_in`]. A budget no scheduling
    /// stall can plausibly consume turns "did the guard measure this fixture?"
    /// back into a question about this module. It is not a licence to be slow —
    /// a genuine hang still ends the test rather than running forever.
    const TEST_EVAL_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

    /// Build a transcript tree matching Claude Code's real layout and return
    /// `(parent_transcript, subagent_transcript)`.
    fn transcript_tree(dir: &Path, agent_id: &str, context_tokens: u64) -> (PathBuf, PathBuf) {
        let parent = dir.join("5b4e60d2-d5d9-4ec5-927f-4fb9198a296d.jsonl");
        std::fs::write(&parent, "{}\n").expect("write parent");
        let sub_dir = dir
            .join("5b4e60d2-d5d9-4ec5-927f-4fb9198a296d")
            .join("subagents");
        std::fs::create_dir_all(&sub_dir).expect("mkdir");
        let sub = sub_dir.join(format!("agent-{agent_id}.jsonl"));
        let line = serde_json::json!({
            "type": "assistant",
            "message": { "usage": {
                "input_tokens": 8,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": context_tokens - 8,
                "output_tokens": 100
            }}
        });
        std::fs::write(&sub, format!("{line}\n")).expect("write sub");
        (parent, sub)
    }

    /// The `Contained` answer for `path`, spelled canonically.
    ///
    /// #7278: the screen returns the CANONICAL path, and on macOS a temp
    /// directory's own spelling (`/var/folders/…`) differs from it
    /// (`/private/var/folders/…`), so a resolution test must compare against
    /// the resolved form or it fails for the wrong reason.
    fn contained(path: &Path) -> AgentTranscript {
        AgentTranscript::Contained(path.canonicalize().expect("canonicalize fixture"))
    }

    #[test]
    fn derives_the_subagent_path_from_agent_id() {
        // The load-bearing case: PreToolUse inside a subagent carries the
        // PARENT's transcript_path plus an agent_id, and the subagent's own
        // transcript must be reached from those two.
        let tmp = crate::test_support::hermetic_temp_dir();
        let (parent, sub) = transcript_tree(tmp.path(), "a1d57cf5a7f59b877", 100_000);
        let payload = serde_json::json!({
            "transcript_path": parent.to_str().expect("utf8"),
            "agent_id": "a1d57cf5a7f59b877",
        });
        assert_eq!(
            resolve_agent_transcript_in(&payload, Some(tmp.path())),
            contained(&sub)
        );
    }

    #[test]
    fn resolves_explicit_agent_transcript_path() {
        let tmp = crate::test_support::hermetic_temp_dir();
        let (parent, sub) = transcript_tree(tmp.path(), "abc123", 100_000);
        let payload = serde_json::json!({
            "transcript_path": parent.to_str().expect("utf8"),
            "agent_transcript_path": sub.to_str().expect("utf8"),
            "agent_id": "abc123",
        });
        assert_eq!(
            resolve_agent_transcript_in(&payload, Some(tmp.path())),
            contained(&sub)
        );
    }

    #[test]
    fn resolves_a_transcript_path_already_under_subagents() {
        // If a future Claude Code release points transcript_path straight at
        // the subagent, the guard must still work with no agent_id at all.
        let tmp = crate::test_support::hermetic_temp_dir();
        let (_, sub) = transcript_tree(tmp.path(), "abc123", 100_000);
        let payload = serde_json::json!({
            "transcript_path": sub.to_str().expect("utf8"),
        });
        assert_eq!(
            resolve_agent_transcript_in(&payload, Some(tmp.path())),
            contained(&sub)
        );
    }

    #[test]
    fn fails_open_without_any_transcript_field() {
        // Every indeterminate payload shape must resolve to Absent (→ ALLOW),
        // and Absent specifically — #7278 distinguishes "no transcript" from
        // "a transcript this guard refuses to open", and these are the first.
        for payload in [
            serde_json::json!({}),
            serde_json::json!({"tool_name": "Bash"}),
            serde_json::json!({"transcript_path": ""}),
            // A parent transcript with no agent_id: this is the PM, and the PM
            // must never be measured against a subagent ceiling.
            serde_json::json!({"transcript_path": "/tmp/nope/parent.jsonl"}),
            // agent_id present but the derived file does not exist.
            serde_json::json!({
                "transcript_path": "/tmp/nope/parent.jsonl",
                "agent_id": "ghost",
            }),
        ] {
            let tmp = crate::test_support::hermetic_temp_dir();
            assert_eq!(
                resolve_agent_transcript_in(&payload, Some(tmp.path())),
                AgentTranscript::Absent,
                "expected fail-open for {payload}"
            );
        }
    }

    // ── #7278: the payload chose the path, so the guard screens it ──

    /// Why (#7278): the reported attack. `resolve_agent_transcript` checked
    /// only `is_file()`, so a `PreToolUse` payload naming any regular file the
    /// `tm` user could read steered the guard's read — and through it its
    /// halt/warn/ok decision. Refusing means the bytes are never touched.
    /// Test: itself.
    #[test]
    fn refuses_a_transcript_outside_the_config_dir() {
        let config = crate::test_support::hermetic_temp_dir();
        let elsewhere = crate::test_support::hermetic_temp_dir();
        let (_, sub) = transcript_tree(elsewhere.path(), "outside", 100_000);
        let payload = serde_json::json!({
            "transcript_path": sub.to_str().expect("utf8"),
        });
        assert_eq!(
            resolve_agent_transcript_in(&payload, Some(config.path())),
            AgentTranscript::Refused(sub),
            "a transcript outside the config directory must be refused, not read"
        );
    }

    /// Why (#7278): `..` walks out of a boundary the prefix check would
    /// otherwise accept, so it is refused before the path is resolved at all.
    /// Test: itself.
    #[test]
    fn refuses_a_traversing_transcript_path() {
        let config = crate::test_support::hermetic_temp_dir();
        let elsewhere = crate::test_support::hermetic_temp_dir();
        let (_, sub) = transcript_tree(elsewhere.path(), "traverse", 100_000);
        let traversing = config
            .path()
            .join("..")
            .join(elsewhere.path().file_name().expect("temp dir name"))
            .join(sub.strip_prefix(elsewhere.path()).expect("under fixture"));
        let payload = serde_json::json!({
            "transcript_path": traversing.to_str().expect("utf8"),
        });
        assert_eq!(
            resolve_agent_transcript_in(&payload, Some(config.path())),
            AgentTranscript::Refused(traversing),
            "a `..` component must be refused"
        );
    }

    /// Why (#7290): with no config directory there is no boundary at all, and
    /// a resolver falling back to `FrameworkPaths::default()` would silently
    /// re-scope containment to `<cwd>/.claude`. Refuse instead.
    /// Test: itself.
    #[test]
    fn refuses_every_transcript_without_a_config_dir() {
        let tmp = crate::test_support::hermetic_temp_dir();
        let (_, sub) = transcript_tree(tmp.path(), "nodir", 100_000);
        let payload = serde_json::json!({
            "transcript_path": sub.to_str().expect("utf8"),
        });
        assert_eq!(
            resolve_agent_transcript_in(&payload, None),
            AgentTranscript::Refused(sub),
            "no boundary means no read, even for a path that would have passed"
        );
    }

    /// Why (#7278 Fail-Open Check): a refusal and a healthy agent at 0 tokens
    /// used to be the same `(Ok, 0)`, so the guard allowed on a number it had
    /// never read and nothing said so. The verdict stays ALLOW — a broken
    /// counter must not halt real work — but the refusal now rides back with
    /// it, and the same fixture placed INSIDE the boundary still measures, so
    /// the contrast is the screen and not a broken fixture.
    /// Test: itself.
    #[tokio::test]
    async fn refusal_is_surfaced_not_silently_allowed() {
        let config = crate::test_support::hermetic_temp_dir();
        let elsewhere = crate::test_support::hermetic_temp_dir();
        let (parent, sub) = transcript_tree(elsewhere.path(), "loud", 622_200);
        let payload = serde_json::json!({
            "transcript_path": parent.to_str().expect("utf8"),
            "agent_id": "loud",
        });

        let refused = evaluate_agent_cost_in(
            &payload,
            &opted_in_stop(),
            Some(config.path()),
            TEST_EVAL_BUDGET,
        )
        .await;
        assert_eq!(
            refused,
            AgentCost {
                status: BudgetStatus::Ok,
                tokens: 0,
                refused_transcript: Some(sub),
            },
            "an out-of-boundary transcript must allow, and say that it was refused"
        );

        // The contrast: the identical 622.2k transcript inside the boundary is
        // measured and stopped, so the refusal above is the screen at work.
        let measured = evaluate_agent_cost_in(
            &payload,
            &opted_in_stop(),
            Some(elsewhere.path()),
            TEST_EVAL_BUDGET,
        )
        .await;
        assert_eq!(measured.tokens, 622_200);
        assert_eq!(measured.status, BudgetStatus::Exceeded);
        assert_eq!(measured.refused_transcript, None);
    }

    #[tokio::test]
    async fn fails_open_when_the_transcript_is_missing() {
        // The core safety property: a broken counter allows the work.
        let config = crate::test_support::hermetic_temp_dir();
        let payload = serde_json::json!({
            "transcript_path": "/tmp/definitely-not-here/p.jsonl",
            "agent_id": "ghost",
        });
        assert_eq!(
            evaluate_agent_cost_in(
                &payload,
                &AgentCostConfig::default(),
                Some(config.path()),
                TEST_EVAL_BUDGET
            )
            .await,
            AgentCost::allow(),
            "a missing transcript is Absent, not Refused — nothing to report"
        );
    }

    /// A config with the hard stop opted in. The shipped default is warn-only
    /// (#4837 review BLOCK 1(a)), so stop-path tests must ask for a stop.
    fn opted_in_stop() -> AgentCostConfig {
        AgentCostConfig {
            enabled: true,
            max_tokens: 400_000,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn reports_exceeded_for_an_over_ceiling_transcript() {
        let tmp = crate::test_support::hermetic_temp_dir();
        let (parent, _) = transcript_tree(tmp.path(), "big", 622_200);
        let payload = serde_json::json!({
            "transcript_path": parent.to_str().expect("utf8"),
            "agent_id": "big",
        });
        let AgentCost { status, tokens, .. } = evaluate_agent_cost_in(
            &payload,
            &opted_in_stop(),
            Some(tmp.path()),
            TEST_EVAL_BUDGET,
        )
        .await;
        assert_eq!(status, BudgetStatus::Exceeded);
        assert_eq!(tokens, 622_200);
        // And the reason handed back must carry the measured number.
        assert!(stop_reason(tokens, 400_000).contains("622200"));
    }

    #[tokio::test]
    async fn default_config_only_warns_on_the_same_transcript() {
        // BLOCK 1(a) end to end: the identical 622.2k transcript that the
        // opted-in ceiling stops must merely WARN under what actually ships.
        let tmp = crate::test_support::hermetic_temp_dir();
        let (parent, _) = transcript_tree(tmp.path(), "big", 622_200);
        let payload = serde_json::json!({
            "transcript_path": parent.to_str().expect("utf8"),
            "agent_id": "big",
        });
        let AgentCost { status, tokens, .. } = evaluate_agent_cost_in(
            &payload,
            &AgentCostConfig::default(),
            Some(tmp.path()),
            TEST_EVAL_BUDGET,
        )
        .await;
        assert_eq!(status, BudgetStatus::Warning);
        assert_eq!(tokens, 622_200);
    }

    #[tokio::test]
    async fn respects_a_disabled_config() {
        // Config override reaches the I/O path too, not just the classifier —
        // a disabled guard must not even touch the filesystem.
        let tmp = crate::test_support::hermetic_temp_dir();
        let (parent, _) = transcript_tree(tmp.path(), "big", 900_000);
        let payload = serde_json::json!({
            "transcript_path": parent.to_str().expect("utf8"),
            "agent_id": "big",
        });
        let disabled = AgentCostConfig {
            enabled: false,
            ..Default::default()
        };
        assert_eq!(
            evaluate_agent_cost_in(&payload, &disabled, Some(tmp.path()), TEST_EVAL_BUDGET).await,
            AgentCost::allow()
        );
    }

    #[tokio::test]
    async fn allows_a_healthy_agent() {
        let tmp = crate::test_support::hermetic_temp_dir();
        let (parent, _) = transcript_tree(tmp.path(), "small", 71_540);
        let payload = serde_json::json!({
            "transcript_path": parent.to_str().expect("utf8"),
            "agent_id": "small",
        });
        // #7028: the budget is the generous test one, so a scheduling stall on
        // a CI runner cannot expire the read and report 0 against 71540.
        let measured = evaluate_agent_cost_in(
            &payload,
            &AgentCostConfig::default(),
            Some(tmp.path()),
            TEST_EVAL_BUDGET,
        )
        .await;
        // #7278: assert on the refusal channel too. Before it existed, a
        // reading of 0 here was indistinguishable from a transcript the guard
        // had declined to open — which is exactly the ambiguity that made the
        // 0-token sighting on #7028 unattributable.
        assert_eq!(measured.refused_transcript, None);
        assert_eq!(measured.status, BudgetStatus::Ok);
        assert_eq!(measured.tokens, 71_540);
    }

    #[test]
    fn an_unmeasured_read_never_denies() {
        // #7028: the state the runner reached on `allows_a_healthy_agent` —
        // 71540 sitting on disk and a deadline that expired before the read
        // landed. The guard must answer Ok with 0, never a stop, and it must
        // still stop on a measurement that genuinely arrived. Asserted on the
        // mapping because the clock cannot be raced deterministically either
        // way; see [`verdict`].
        assert_eq!(verdict(None, &opted_in_stop()), AgentCost::allow());
        assert_eq!(
            verdict(Some(622_200), &opted_in_stop()),
            AgentCost {
                status: BudgetStatus::Exceeded,
                tokens: 622_200,
                // #7278: reaching `verdict` at all means the transcript passed
                // the containment screen, so neither arm can report a refusal.
                refused_transcript: None,
            }
        );
    }

    #[test]
    fn the_production_budget_stays_far_inside_the_pretooluse_hook_timeout() {
        // The two bounds EVAL_TIMEOUT has to sit between, now that the value is
        // a parameter the tests can override (#7028): Claude Code gives
        // PreToolUse 5 seconds and this guard runs before every tool call, so
        // the read has to be invisible inside it — and a zeroed budget would
        // expire on every call, silently retiring the guard.
        assert!(EVAL_TIMEOUT > std::time::Duration::ZERO);
        assert!(EVAL_TIMEOUT <= std::time::Duration::from_millis(500));
    }

    // ── #4837 review MEDIUM: the 64 KiB window misses the largest transcripts ──

    /// One JSONL line of `bytes` of filler carrying no `usage` block — a stand-in
    /// for the oversized tool result that pushes the newest assistant turn out
    /// of a 64 KiB window.
    fn filler_line(bytes: usize) -> String {
        format!(
            "{}\n",
            serde_json::json!({"type": "user", "pad": "x".repeat(bytes)})
        )
    }

    #[tokio::test]
    async fn retries_with_a_larger_tail_when_64k_holds_no_usage_record() {
        // Measured: 1 of the 12 largest subagent transcripts on this machine
        // had no complete usage record in its final 64 KiB, so the guard failed
        // open on exactly the transcripts it exists to catch.
        let tmp = crate::test_support::hermetic_temp_dir();
        let (parent, sub) = transcript_tree(tmp.path(), "huge", 500_000);
        // Bury the usage record behind more than 64 KiB of unparseable tail.
        let mut jsonl = std::fs::read_to_string(&sub).expect("read");
        jsonl.push_str(&filler_line(100 * 1024));
        std::fs::write(&sub, &jsonl).expect("write");
        assert!(
            jsonl.len() as u64 > MAX_TRANSCRIPT_TAIL,
            "the fixture must actually exceed the first window"
        );

        // The first pass alone finds nothing — this is the bug being fixed.
        let first = super::super::misc::read_transcript_tail(&sub, MAX_TRANSCRIPT_TAIL)
            .await
            .expect("tail read");
        assert_eq!(
            trusty_mpm::core::agent_cost::latest_context_tokens(&first),
            None,
            "fixture invalid: 64 KiB must NOT contain a usage record"
        );

        // The retry recovers it, and the guard classifies normally.
        let payload = serde_json::json!({
            "transcript_path": parent.to_str().expect("utf8"),
            "agent_id": "huge",
        });
        // #6663 saw this same fixture lose the 200 ms production deadline under
        // a loaded run and patched it with a retry loop; #7028 removes the race
        // instead. Both tail reads share one budget and this fixture is
        // ~600 KiB read twice, so the budget is the generous test one.
        let AgentCost { status, tokens, .. } = evaluate_agent_cost_in(
            &payload,
            &opted_in_stop(),
            Some(tmp.path()),
            TEST_EVAL_BUDGET,
        )
        .await;
        assert_eq!(tokens, 500_000, "the larger window must find the record");
        assert_eq!(status, BudgetStatus::Exceeded);
    }

    #[tokio::test]
    async fn still_fails_open_when_even_the_larger_tail_has_no_record() {
        // Growing the window must not weaken the fail-open contract: a
        // transcript with no usage record anywhere still ALLOWS. #7028: the
        // generous budget is what makes that claim mean anything — under the
        // 200 ms production deadline an expiry returned the expected `(Ok, 0)`
        // too, so this test could pass without reading the fixture at all.
        let tmp = crate::test_support::hermetic_temp_dir();
        let (parent, sub) = transcript_tree(tmp.path(), "norec", 500_000);
        std::fs::write(&sub, filler_line(100 * 1024)).expect("write");
        let payload = serde_json::json!({
            "transcript_path": parent.to_str().expect("utf8"),
            "agent_id": "norec",
        });
        assert_eq!(
            evaluate_agent_cost_in(
                &payload,
                &opted_in_stop(),
                Some(tmp.path()),
                TEST_EVAL_BUDGET
            )
            .await,
            AgentCost::allow()
        );
    }

    // ── #4837 review BLOCK 1(b): the stop must never strand finished work ──

    #[test]
    fn escape_hatch_permits_send_message_and_git_persistence() {
        // The #4841 engineer reached 434k holding a correct fix. Under the
        // first cut it could not have committed, pushed, or reported it.
        assert!(is_persistence_escape("SendMessage", None));
        for command in [
            "git add -A",
            "git commit -m 'fix: the thing'",
            "git push origin HEAD",
            "git -C /repo/wt commit -m x",
            "git add -A && git commit -m x && git push",
        ] {
            assert!(
                is_persistence_escape("Bash", Some(&serde_json::json!({"command": command}))),
                "a stopped agent must still be able to run {command:?}"
            );
        }
    }

    #[test]
    fn escape_hatch_denies_work_tools() {
        // The hatch is exactly "persist and report" wide. Anything that lets
        // the agent keep working would make the stop decorative.
        for tool in ["Write", "Edit", "Read", "Grep", "WebFetch", "Task", "Agent"] {
            assert!(
                !is_persistence_escape(tool, Some(&serde_json::json!({}))),
                "{tool} must stay denied past the ceiling"
            );
        }
        // Bash with no command, or with work smuggled behind an allowed verb.
        assert!(!is_persistence_escape("Bash", None));
        for command in [
            "cargo test",
            "git commit -m x && cargo test",
            "git checkout main",
        ] {
            assert!(
                !is_persistence_escape("Bash", Some(&serde_json::json!({"command": command}))),
                "{command:?} must stay denied past the ceiling"
            );
        }
    }

    #[test]
    fn escape_hatch_denies_exec_capable_git() {
        // #4850 review HIGH: these four shapes were all "persistence" under the
        // first cut's flag surface, and every one of them runs `cargo test`
        // inside a listed git subcommand.
        for command in [
            "git -c diff.external='cargo test' diff",
            "git -c protocol.ext.allow=always push ext::sh -c 'cargo test' HEAD",
            "git push --receive-pack='cargo test' /tmp/repo HEAD",
            "git diff <(cargo test)",
        ] {
            assert!(
                !is_persistence_escape("Bash", Some(&serde_json::json!({"command": command}))),
                "{command:?} executes a program and must not count as persistence"
            );
        }
    }

    #[test]
    fn warn_notice_is_claimed_per_sibling_not_per_parent_session() {
        // #4850 review MEDIUM: in resolution case 2 the payload carries the
        // SUBAGENT's transcript_path and no agent_id, so both siblings fell back
        // to the parent's session_id — one key, and the first to warn silenced
        // the other. The transcript stem is what distinguishes them.
        let tmp = crate::test_support::hermetic_temp_dir();
        let session = format!("parent-{}", std::process::id());
        let ids = [
            format!("sib-a-{}", std::process::id()),
            format!("sib-b-{}", std::process::id()),
        ];
        let subs: Vec<PathBuf> = ids
            .iter()
            .map(|id| transcript_tree(tmp.path(), id, 300_000).1)
            .collect();
        let markers: Vec<PathBuf> = ids
            .iter()
            .map(|id| {
                std::env::temp_dir()
                    .join("trusty-mpm-agent-cost")
                    .join(format!("agent-{id}"))
            })
            .collect();
        for m in &markers {
            let _ = std::fs::remove_file(m);
        }

        for sub in &subs {
            let payload = serde_json::json!({
                "transcript_path": sub.to_str().expect("utf8"),
                "session_id": session,
            });
            assert!(
                claim_warn_notice(&payload),
                "each sibling must get its own notice, not share the parent's"
            );
            assert!(!claim_warn_notice(&payload), "…and only one each");
        }

        for m in &markers {
            let _ = std::fs::remove_file(m);
        }
    }

    #[test]
    fn warn_notice_fails_open_when_no_key_can_be_derived() {
        // #4850 review LOW: an agent_id that filters to empty used to make the
        // marker path the DIRECTORY, whose create_new always reports
        // AlreadyExists — suppressing the notice permanently, for everyone. The
        // documented contract is that an unusable key fails OPEN.
        for payload in [
            serde_json::json!({"agent_id": "!!!"}),
            serde_json::json!({"agent_id": "", "session_id": ""}),
            serde_json::json!({}),
        ] {
            assert!(warn_notice_key(&payload).is_none(), "{payload}");
            for _ in 0..3 {
                assert!(
                    claim_warn_notice(&payload),
                    "an underivable key must keep informing the agent: {payload}"
                );
            }
        }
    }

    #[test]
    fn warn_notice_is_claimed_once_per_agent() {
        // The nudge must not be re-sent on every tool call — that would spend
        // context complaining about context.
        let id = format!("test-{}", std::process::id());
        let payload = serde_json::json!({"agent_id": id});
        let marker = std::env::temp_dir().join("trusty-mpm-agent-cost").join(&id);
        let _ = std::fs::remove_file(&marker);

        assert!(claim_warn_notice(&payload), "first call must claim");
        assert!(!claim_warn_notice(&payload), "second call must not");
        assert!(!claim_warn_notice(&payload), "and neither must the third");

        // A different agent gets its own nudge.
        let other = serde_json::json!({"agent_id": format!("{id}-other")});
        assert!(claim_warn_notice(&other));

        let _ = std::fs::remove_file(&marker);
        let _ = std::fs::remove_file(
            std::env::temp_dir()
                .join("trusty-mpm-agent-cost")
                .join(format!("{id}-other")),
        );
    }
}
