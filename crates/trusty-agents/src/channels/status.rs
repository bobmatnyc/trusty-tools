//! Per-binding dispatch health, so a failed inbound wake stops being invisible.
//!
//! Why: `agent_channels::receive_inbound` spawned the wake dispatch and threw
//! the result away — `let _result = …`. A `run_pm_task_with_persona` that failed
//! (model error, missing persona, cancelled session) produced no log line, no
//! counter, and no change in what the channel view showed: the binding still
//! read as healthy while every inbound message was being dropped. That is
//! fail-open on the one path a user cannot observe from the outside, because
//! the reply they are waiting for simply never arrives.
//!
//! What: [`record_dispatch`] awaits the dispatch future and, on `Err`, logs at
//! error level with the assistant and binding id and increments a per-binding
//! counter. [`status_json`] publishes those counters beside the bindings in the
//! channel view, so a binding that is failing says so. The counter is
//! process-lifetime — it answers "has this binding been dropping wakes since
//! the daemon started", which is the question the view asks — and follows the
//! same `LazyLock<Mutex<HashMap<…>>>` shape as
//! `crate::tools::mcp_live::cache`'s process-wide cache.
//!
//! A poisoned lock recovers rather than dropping the update, and the retained
//! reason is bounded in bytes: both are fences that must not themselves fail
//! silently, which is what dropping the write or over-running the payload
//! budget would be.
//!
//! Test: `channel_dispatch_failure_is_counted_per_binding`,
//! `channel_dispatch_success_leaves_the_counter_alone`,
//! `channel_dispatch_reason_is_bounded_in_bytes_at_a_char_boundary`.

// #7427: the discarded wake-dispatch result becomes an observable failure.
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// Longest error text retained per binding, in BYTES. Bounds what one failing
/// binding can hold, and keeps the channel-view payload small.
///
/// Why bytes (#7427): the budget exists to bound the JSON payload, and JSON is
/// measured in bytes. Counting characters let a reason of 240 multi-byte
/// characters — a provider error carrying a non-ASCII channel name, say —
/// occupy up to four times the intended budget. [`truncate_reason`] cuts at a
/// character boundary at or below this many bytes, so the bound holds and the
/// result is still valid UTF-8.
const MAX_REASON_BYTES: usize = 240;

/// The stored prefix of `reason`: at most [`MAX_REASON_BYTES`] bytes, cut on a
/// character boundary.
///
/// Test: `channel_dispatch_reason_is_bounded_in_bytes_at_a_char_boundary`.
fn truncate_reason(reason: &str) -> String {
    if reason.len() <= MAX_REASON_BYTES {
        return reason.to_string();
    }
    let mut end = MAX_REASON_BYTES;
    while end > 0 && !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason[..end].to_string()
}

/// Read a poisoned lock's contents rather than dropping the write.
///
/// Why (#7427): a `LazyLock<Mutex<…>>` is poisoned when a thread panics while
/// holding it. The map behind it is a plain `HashMap` of counters — a panic
/// mid-update cannot leave it in a state that misleads a later reader, so
/// refusing to touch it afterwards buys nothing and costs everything: every
/// subsequent dispatch failure would go uncounted, silently, which is the exact
/// invisibility this module exists to end. Recovering with `into_inner` keeps
/// the fence working, and the panic that poisoned the lock has already been
/// reported by the panicking thread.
fn health() -> std::sync::MutexGuard<'static, HashMap<(String, String), BindingHealth>> {
    HEALTH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What one binding's dispatches have been doing.
#[derive(Clone, Debug, Default)]
struct BindingHealth {
    /// Wake dispatches that returned `Err` since process start.
    failures: u64,
    /// Text of the most recent failure.
    last_error: Option<String>,
}

/// Process-wide dispatch health, keyed by `(assistant, binding id)`.
static HEALTH: LazyLock<Mutex<HashMap<(String, String), BindingHealth>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Await a wake dispatch and record the failure arm.
///
/// Why: the fence itself. Every inbound dispatch goes through here, so there is
/// one place that can drop a result — and it does not.
/// What: on `Err`, logs at error level with the assistant and binding id, then
/// bumps that binding's counter. Returns `()` either way: an inbound event that
/// fails to wake the assistant must not poison the intake loop for the other
/// bindings that matched the same event.
/// Test: `channel_dispatch_failure_is_counted_per_binding`,
/// `channel_dispatch_success_leaves_the_counter_alone`.
pub(crate) async fn record_dispatch<T, E: std::fmt::Display>(
    agent: &str,
    binding_id: &str,
    dispatch: impl Future<Output = Result<T, E>>,
) {
    let Err(error) = dispatch.await else {
        return;
    };
    tracing::error!(
        assistant = agent,
        binding = binding_id,
        error = %error,
        "channel wake dispatch failed; the inbound message produced no assistant turn"
    );
    record_failure(agent, binding_id, &error.to_string());
}

/// Increment a binding's failure counter and retain the reason.
///
/// Why: the inbound path can also fail before the dispatch exists — an adapter
/// that cannot build a prompt, a credential that will not resolve — and those
/// failures are just as invisible.
/// Test: `channel_dispatch_failure_is_counted_per_binding`.
pub(crate) fn record_failure(agent: &str, binding_id: &str, reason: &str) {
    let mut health = health();
    let entry = health
        .entry((agent.to_string(), binding_id.to_string()))
        .or_default();
    entry.failures = entry.failures.saturating_add(1);
    entry.last_error = Some(truncate_reason(reason));
}

/// This assistant's per-binding dispatch health, for the channel view.
///
/// Why: the counter is only a fence if someone sees it. `agent_channels::read`
/// publishes this beside `bindings` and `providers`, which is what the UI and
/// the `channel` tool already read.
/// What: an object keyed by binding id; bindings with no failures are absent,
/// so a healthy assistant returns `{}`.
/// Test: `channel_dispatch_failure_is_counted_per_binding`.
pub(crate) fn status_json(agent: &str) -> Value {
    let health = health();
    let mut out = serde_json::Map::new();
    for ((owner, binding_id), entry) in health.iter() {
        if owner != agent {
            continue;
        }
        out.insert(
            binding_id.clone(),
            json!({"dispatch_failures":entry.failures,"last_error":entry.last_error}),
        );
    }
    Value::Object(out)
}

/// Failures recorded for one binding.
#[cfg(test)]
pub(crate) fn dispatch_failures(agent: &str, binding_id: &str) -> u64 {
    health()
        .get(&(agent.to_string(), binding_id.to_string()))
        .map_or(0, |e| e.failures)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pre-change this test does not compile: there is no counter to read.
    /// the inbound path discarded the dispatch result outright.
    #[tokio::test]
    async fn channel_dispatch_failure_is_counted_per_binding() {
        let agent = "fixture-failure";
        record_dispatch(agent, "team", async {
            Err::<(), _>("persona dispatch exploded")
        })
        .await;

        assert_eq!(dispatch_failures(agent, "team"), 1);
        assert_eq!(dispatch_failures(agent, "other"), 0);
        let status = status_json(agent);
        assert_eq!(status["team"]["dispatch_failures"], json!(1));
        assert_eq!(
            status["team"]["last_error"],
            json!("persona dispatch exploded")
        );
        assert!(status.get("other").is_none());
    }

    /// The retained reason is bounded in bytes and stays valid UTF-8.
    ///
    /// Pre-change this fails on the first assertion: the cut took 240
    /// CHARACTERS, so a reason of multi-byte characters was stored at up to
    /// four times the intended budget.
    #[test]
    fn channel_dispatch_reason_is_bounded_in_bytes_at_a_char_boundary() {
        // One ASCII byte then three-byte characters, so byte 240 lands inside a
        // character and the cut has to step back to 238.
        let multibyte = format!("x{}", "日".repeat(200));
        assert_eq!(multibyte.len(), 601);
        let stored = truncate_reason(&multibyte);
        assert!(stored.len() <= MAX_REASON_BYTES);
        assert_eq!(stored.len(), 238);
        assert!(stored.starts_with('x'));
        assert!(stored.chars().skip(1).all(|c| c == '日'));

        // A reason inside the budget is stored whole, multi-byte or not.
        assert_eq!(truncate_reason("persona dispatch exploded 💥").len(), 30);
        assert_eq!(truncate_reason(""), "");
    }

    #[tokio::test]
    async fn channel_dispatch_success_leaves_the_counter_alone() {
        let agent = "fixture-success";
        record_dispatch(agent, "team", async { Ok::<_, String>("assistant reply") }).await;
        assert_eq!(dispatch_failures(agent, "team"), 0);
        assert_eq!(status_json(agent), json!({}));
    }
}
