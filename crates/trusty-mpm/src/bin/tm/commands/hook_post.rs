//! Retry-plus-log delivery of the `SubagentStop` hook POST (#6556).
//!
//! Why: the POST that tells the daemon a subagent stopped was fire-and-forget —
//! `let _ = client.post(...).send().await`. A daemon blip during that one
//! attempt loses the stop outright, and the delegation stays `Running` until the
//! six-hour `RUNNING_STALE_AFTER_SECS` sweep, holding a builder slot and a
//! checkout for the whole window. That is #6556's own incident. Every other hook
//! event is genuinely advisory — a lost `PreToolUse` observation costs an audit
//! row — so only the stop is retried, and only the stop is parked on disk.
//!
//! What: [`post_hook_with_retry`] makes up to [`HOOK_POST_ATTEMPTS`] attempts
//! with a bounded backoff, the whole loop capped by [`HOOK_POST_BUDGET`] so the
//! worst case stays well inside the five-second timeout the `SubagentStop` hook
//! is registered with. It returns the attempt count and one log line per
//! attempt plus a verdict line; [`emit_hook_post_log`] writes them to stderr,
//! which is where an operator sees a hook's output — this process installs no
//! tracing subscriber, so `tracing::warn!` here would go nowhere (the #2610
//! idle-parking warning uses stderr for the same reason).
//! [`spool_undelivered_stop`] parks a permanently undelivered stop through
//! [`trusty_mpm::core::stop_spool`], where the daemon's reap loop replays it.
//!
//! Test: `hook_post_tests.rs`.

use std::path::Path;
use std::time::Duration;

/// Log prefix every line in this module carries.
///
/// Why: an operator greps a session log for one string to find a lost stop.
pub(crate) const HOOK_POST_LOG_PREFIX: &str = "trusty-mpm: SUBAGENT-STOP POST (#6556)";

/// How long one attempt's TCP connect may take.
///
/// Why: unchanged from the pre-#6556 single-shot client — a pathological
/// OS-level connect stall must not eat the whole budget.
pub(crate) const HOOK_POST_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

/// How long one attempt may take end to end.
///
/// Why: three attempts have to fit inside [`HOOK_POST_BUDGET`], so the
/// single-shot 2 s total of the pre-#6556 client is too long for one of them. A
/// daemon that is up answers `POST /hooks` in single-digit milliseconds — it is
/// an in-memory `DashMap` write — so 800 ms is a wide margin over a healthy
/// answer and a fast verdict on an unhealthy one. 680 ms rather than a round
/// 800: three of those plus the two backoffs is 2490 ms, so all three attempts
/// FIT inside [`HOOK_POST_BUDGET`] instead of the third being truncated by it
/// while the log claimed three (critic LOW on PR #8052).
pub(crate) const HOOK_POST_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(680);

/// How many times the stop POST is attempted before it is parked on disk.
///
/// Why: the failure this covers is a blip — a daemon mid-restart, a refused
/// connect on a busy host. Two retries cross a restart that is already in
/// flight; more would only spend budget on a daemon that is genuinely down,
/// which the spool covers better than another second of waiting.
pub(crate) const HOOK_POST_ATTEMPTS: u32 = 3;

/// Base backoff between attempts; attempt `n` waits `n ×` this.
pub(crate) const HOOK_POST_BACKOFF: Duration = Duration::from_millis(150);

/// Hard ceiling on the whole retry loop.
///
/// Why: the sum of the per-attempt bounds is not itself a guarantee — a
/// `reqwest` client under a stalled DNS resolver can overrun one. This wraps the
/// loop in `tokio::time::timeout` so the hook's worst case is a number, not an
/// estimate. Chosen against the four bounds a `SubagentStop` invocation pays in
/// series: the daemon-URL probe
/// ([`trusty_mpm::core::discovery::GATEWAY_PROBE_TIMEOUT`], 500 ms), the stdin
/// read ([`crate::commands::hook_stdin::HOOK_STDIN_TIMEOUT`], 500 ms), the
/// idle-parking detection
/// ([`crate::commands::misc::IDLE_PARK_DETECT_TIMEOUT`], 300 ms) and this — 3.8 s
/// of the 5 s Claude Code is told to allow, leaving 1.2 s for exec, the spool
/// write and process teardown.
/// Test: `the_stop_post_budget_stays_inside_the_registered_hook_timeout`, which
/// sums all four so lowering any one of them trips the gate.
pub(crate) const HOOK_POST_BUDGET: Duration = Duration::from_millis(2500);

/// The `timeout` Claude Code is told to allow the `SubagentStop` hook, in
/// seconds.
///
/// Why (#6556): the retry budget above is only safe relative to this number, so
/// the budget gate has to know it. It is READ from
/// [`trusty_mpm::core::standalone::hooks::mpm_hook_additions_with_exe`] rather
/// than mirrored as a literal (critic MEDIUM on PR #8052): a mirrored copy makes
/// the gate assert its own assumption, so lowering the registered timeout would
/// pass here and kill the hook in production.
/// What: the `SubagentStop` group's `timeout`, in seconds, or `None` when the
/// block cannot be built on this host (no resolvable installed binary).
/// Test: `the_stop_post_budget_stays_inside_the_registered_hook_timeout`,
/// `the_registered_subagent_stop_timeout_is_readable`.
#[cfg(test)]
pub(crate) fn registered_subagent_stop_timeout() -> Option<Duration> {
    let exe = std::path::Path::new("/usr/local/bin/tm");
    let block = trusty_mpm::core::standalone::hooks::mpm_hook_additions_with_exe(Some(exe)).ok()?;
    let secs = block
        .get("hooks")?
        .get("SubagentStop")?
        .get(0)?
        .get("hooks")?
        .get(0)?
        .get("timeout")?
        .as_u64()?;
    Some(Duration::from_secs(secs))
}

/// What one stop POST did, and what to tell the operator about it.
///
/// Why: `attempts` and `lines` are what the tests assert and what the caller
/// prints, so the function reports rather than logs — this process installs no
/// tracing subscriber, and a test that had to install one to see the attempts
/// would be asserting the subscriber.
/// What: `attempts` counts every request actually issued; `delivered` is whether
/// the daemon accepted one; `lines` is one line per attempt plus a final verdict.
/// Test: `a_stop_delivered_on_the_third_attempt_logs_every_one`.
#[derive(Debug)]
pub(crate) struct HookPostOutcome {
    /// Requests issued, including the one that succeeded.
    pub(crate) attempts: u32,
    /// Did the daemon accept the stop?
    pub(crate) delivered: bool,
    /// One line per attempt, then the verdict.
    pub(crate) lines: Vec<String>,
}

/// POST `body` to `<url>/hooks`, retrying a transient failure.
///
/// Why: see the module header — this is the delivery a lost delegation record
/// depends on.
/// What: up to [`HOOK_POST_ATTEMPTS`] attempts, each bounded by
/// [`HOOK_POST_ATTEMPT_TIMEOUT`], separated by a linearly growing
/// [`HOOK_POST_BACKOFF`], the whole loop capped by [`HOOK_POST_BUDGET`]. An HTTP
/// status the daemon returns as a refusal (4xx other than 408/429) is
/// PERMANENT and stops the loop — retrying a malformed body only spends budget.
/// A 5xx, a 408, a 429 and every transport error are transient. Returns without
/// ever propagating an error: a hook that fails blocks the user's prompt.
/// Test: `a_stop_delivered_on_the_third_attempt_logs_every_one`,
/// `a_daemon_that_never_answers_exhausts_the_attempts`,
/// `a_refused_body_is_not_retried`,
/// `a_stop_delivered_first_try_makes_one_attempt`.
pub(crate) async fn post_hook_with_retry(url: &str, body: &serde_json::Value) -> HookPostOutcome {
    let mut outcome = HookPostOutcome {
        attempts: 0,
        delivered: false,
        lines: Vec::new(),
    };
    let budget = tokio::time::timeout(HOOK_POST_BUDGET, attempt_loop(url, body, &mut outcome));
    if budget.await.is_err() {
        outcome.lines.push(format!(
            "{HOOK_POST_LOG_PREFIX} budget {HOOK_POST_BUDGET:?} exhausted"
        ));
    }
    outcome.lines.push(format!(
        "{HOOK_POST_LOG_PREFIX} {} after {} attempt(s)",
        if outcome.delivered {
            "delivered"
        } else {
            "UNDELIVERED"
        },
        outcome.attempts
    ));
    outcome
}

/// The attempt loop [`post_hook_with_retry`] wraps in its budget.
///
/// Why: split out so the budget is the only thing its caller does, and so the
/// loop can return early on a permanent refusal without unwinding a timeout.
/// What: see [`post_hook_with_retry`]; appends one line per attempt to
/// `outcome`.
async fn attempt_loop(url: &str, body: &serde_json::Value, outcome: &mut HookPostOutcome) {
    let client = match reqwest::Client::builder()
        .connect_timeout(HOOK_POST_CONNECT_TIMEOUT)
        .timeout(HOOK_POST_ATTEMPT_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            // Programmer-class: no client, so no attempt is possible. Say so
            // rather than reporting zero attempts with no reason.
            outcome
                .lines
                .push(format!("{HOOK_POST_LOG_PREFIX} client build failed: {e}"));
            return;
        }
    };
    let endpoint = format!("{url}/hooks");
    for attempt in 1..=HOOK_POST_ATTEMPTS {
        if attempt > 1 {
            tokio::time::sleep(HOOK_POST_BACKOFF * (attempt - 1)).await;
        }
        // Stamped AFTER the request returns, never before (critic LOW on PR
        // #8052): if the budget cancels this future mid-await the count must not
        // claim an attempt whose result nobody saw.
        let result = client.post(&endpoint).json(body).send().await;
        outcome.attempts = attempt;
        match result {
            Ok(resp) if resp.status().is_success() => {
                outcome.delivered = true;
                outcome.lines.push(format!(
                    "{HOOK_POST_LOG_PREFIX} attempt {attempt}/{HOOK_POST_ATTEMPTS}: {}",
                    resp.status()
                ));
                return;
            }
            Ok(resp) => {
                let status = resp.status();
                let transient = status_is_transient(status);
                outcome.lines.push(format!(
                    "{HOOK_POST_LOG_PREFIX} attempt {attempt}/{HOOK_POST_ATTEMPTS}: {status} \
                     ({})",
                    if transient { "transient" } else { "permanent" }
                ));
                if !transient {
                    return;
                }
            }
            Err(e) => outcome.lines.push(format!(
                "{HOOK_POST_LOG_PREFIX} attempt {attempt}/{HOOK_POST_ATTEMPTS}: {e}"
            )),
        }
    }
}

/// Is this response status worth another attempt?
///
/// What: a 5xx is the daemon failing to serve a request it understood; 408 and
/// 429 are explicit "try again". Every other 4xx says the body itself is wrong,
/// which no retry fixes. A 3xx reaching here means the client did not follow it,
/// which is also not a retry case.
/// Test: `a_refused_body_is_not_retried`.
fn status_is_transient(status: reqwest::StatusCode) -> bool {
    status.is_server_error()
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
}

/// Print an outcome's lines where an operator sees them.
///
/// What: stderr, one line each. Claude Code captures a hook's stderr into the
/// session log, and this process installs no tracing subscriber — see the module
/// header.
pub(crate) fn emit_hook_post_log(outcome: &HookPostOutcome) {
    for line in &outcome.lines {
        eprintln!("{line}");
    }
}

/// Park an undelivered stop where the daemon's reap loop will replay it.
///
/// Why: retrying covers a blip inside the hook's budget; it cannot cover a
/// daemon that is down. Without this the stop is simply lost and the delegation
/// waits out `RUNNING_STALE_AFTER_SECS`.
/// What: writes `body` through
/// [`trusty_mpm::core::stop_spool::record_unposted_stop`] under the framework
/// root, and prints what happened either way. The stderr line is a breadcrumb,
/// not the alarm: Claude Code does not surface an exit-0 hook's stderr, so the
/// operator-visible surface for a spool that is unwritable or at its cap is the
/// `stop_spool` `tm doctor` row
/// ([`trusty_mpm::core::stop_spool::check_stop_spool`]).
/// Test: `a_parked_stop_lands_where_the_daemon_drains_it`,
/// `an_unusable_root_reports_the_stop_as_lost` — both through
/// [`spool_undelivered_stop_under`], which carries everything this function does
/// except the `$HOME` lookup.
pub(crate) fn spool_undelivered_stop(body: &serde_json::Value) {
    let root = trusty_mpm::core::paths::FrameworkPaths::default().root;
    eprintln!("{}", spool_undelivered_stop_under(&root, body));
}

/// Park an undelivered stop under `root` and say what happened.
///
/// Why: [`spool_undelivered_stop`] resolves its root from the process-global
/// `$HOME`, which a test can only reach by mutating it. The write and its two
/// verdicts are the part worth pinning, so they take the root explicitly — the
/// `_under` split `FrameworkPaths::under` exists for (critic MEDIUM on PR
/// #8052, which found the `Test:` pointer here naming a file that never existed).
/// What: writes `body` through
/// [`trusty_mpm::core::stop_spool::record_unposted_stop`] and returns the single
/// operator line for whichever arm ran. Never fails: a hook must not die on a
/// bookkeeping write.
/// Test: `a_parked_stop_lands_where_the_daemon_drains_it`,
/// `an_unusable_root_reports_the_stop_as_lost`.
pub(crate) fn spool_undelivered_stop_under(root: &Path, body: &serde_json::Value) -> String {
    match trusty_mpm::core::stop_spool::record_unposted_stop(root, body) {
        Some(path) => format!(
            "{HOOK_POST_LOG_PREFIX} parked at {} — the daemon replays it on its next reap tick",
            path.display()
        ),
        None => format!(
            "{HOOK_POST_LOG_PREFIX} could not park the stop under {} — this delegation will \
             stay Running until the staleness sweep",
            root.display()
        ),
    }
}

#[cfg(test)]
#[path = "hook_post_tests.rs"]
mod hook_post_tests;
