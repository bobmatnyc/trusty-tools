//! Handler for `trusty-memory note` — fire-and-forget memory save from a shell.
//!
//! Why: sub-agents spawned via Claude Code's Agent tool inherit no MCP
//! connections, so the `mcp__trusty-memory__memory_remember` tool is
//! unreachable to them. They *can* run shell commands. This subcommand
//! gives them a writable handle that needs no MCP plumbing — it POSTs to
//! the running daemon's `POST /api/v1/remember` endpoint and returns
//! immediately. The endpoint queues the dispatch on a `tokio::spawn`, so
//! the CLI does not wait for the redb write or any of the content gates.
//!
//! What: resolves the daemon's HTTP address via
//! `trusty_common::read_daemon_addr`, builds the JSON body, fires a short-
//! timeout POST, prints `"Queued."` on success, and exits zero even when
//! the daemon is unreachable (the contract is fire-and-forget — the caller
//! has no use for a failure that arrives after the agent already exited).
//!
//! Test: `note_builds_expected_body` covers the request payload shape;
//! end-to-end coverage of the endpoint itself lives in
//! `crate::web::tests::remember_async_*`.

use anyhow::{Context, Result};
use serde_json::Value;
use std::time::Duration;

/// Default HTTP timeout for the note call.
///
/// Why: the daemon's response path is "parse body, spawn task, return 202",
/// which should never take more than a few hundred milliseconds. A two-
/// second ceiling leaves room for a paged-out cache while still letting the
/// CLI return promptly when the daemon is unreachable.
/// What: const `Duration` reused for both `timeout` and `connect_timeout`
/// on the `reqwest::Client`.
/// Test: implicitly exercised by the integration tests that drive the
/// endpoint through the router.
const HTTP_TIMEOUT: Duration = Duration::from_secs(2);

/// Build the JSON body for `POST /api/v1/remember` from CLI inputs.
///
/// Why: pulled out so the body shape can be asserted from a unit test
/// without spinning up a daemon. The body shape is the public contract
/// between the CLI and the HTTP endpoint — drift here is a silent break.
/// What: returns a `serde_json::Value` object with `content` always set,
/// `palace` and `tags` set only when non-empty (so the endpoint can fall
/// back to its `--palace` default and tag-less store, respectively).
/// Test: `note_builds_expected_body` (in this module).
fn build_request_body(content: &str, palace: Option<&str>, tags: &[String]) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("content".to_string(), Value::String(content.to_string()));
    if let Some(p) = palace {
        obj.insert("palace".to_string(), Value::String(p.to_string()));
    }
    if !tags.is_empty() {
        obj.insert(
            "tags".to_string(),
            Value::Array(tags.iter().cloned().map(Value::String).collect()),
        );
    }
    Value::Object(obj)
}

/// Entry point for `trusty-memory note <content> [--palace <name>] [--tag <tag>]...`.
///
/// Why: the CLI face of the fire-and-forget save path. Sub-agents call
/// this from `Bash` to land a memory without inheriting an MCP connection.
/// Every failure mode — daemon not running, transport error, non-2xx —
/// degrades to a stderr warning and a zero exit so the agent's task is
/// never blocked by an unreliable memory write.
/// What: discovers the daemon via `trusty_common::read_daemon_addr`,
/// fires a `POST /api/v1/remember` with `{content, palace?, tags?}`, and
/// prints `"Queued."` on 2xx. Daemon-missing prints a warning to stderr
/// and still exits zero (the contract is one-way).
/// Test: covered manually via `trusty-memory start && trusty-memory note
/// "hello"`; the HTTP endpoint itself is covered by
/// `crate::web::tests::remember_async_returns_202_and_persists`.
pub async fn handle_note(content: String, palace: Option<String>, tags: Vec<String>) -> Result<()> {
    // Validate locally so the CLI does not even bother contacting the
    // daemon when the caller passed an empty string. The endpoint would
    // reject it with 400, but failing fast here keeps the failure mode
    // visible (instead of "queued and silently dropped").
    if content.trim().is_empty() {
        eprintln!("trusty-memory note: 'content' must not be empty");
        std::process::exit(2);
    }

    // Discover the running daemon. When no daemon is running we degrade
    // to a warning + zero exit so the agent's parent shell does not break
    // on a missing memory write — the contract is fire-and-forget.
    let addr = match trusty_common::read_daemon_addr("trusty-memory") {
        Ok(Some(a)) => a,
        Ok(None) => {
            eprintln!(
                "trusty-memory note: daemon not running — memory write \
                 dropped. Start it with `trusty-memory start`."
            );
            return Ok(());
        }
        Err(e) => {
            eprintln!("trusty-memory note: could not read daemon address: {e:#}");
            return Ok(());
        }
    };
    let base = if addr.starts_with("http://") || addr.starts_with("https://") {
        addr
    } else {
        format!("http://{addr}")
    };
    let url = format!("{base}/api/v1/remember");

    let body = build_request_body(&content, palace.as_deref(), &tags);

    let client = match reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(HTTP_TIMEOUT)
        .build()
        .context("build http client")
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("trusty-memory note: could not build http client: {e:#}");
            return Ok(());
        }
    };

    match client.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            println!("Queued.");
        }
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            eprintln!(
                "trusty-memory note: daemon returned {status}: {text} (memory write dropped)"
            );
        }
        Err(e) => {
            eprintln!("trusty-memory note: POST {url} failed: {e:#} (memory write dropped)");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `build_request_body` emits exactly the fields the HTTP endpoint
    /// expects, omitting `palace` and `tags` when the caller does not set
    /// them.
    ///
    /// Why: drift between the CLI body shape and the endpoint's
    /// `RememberAsyncBody` deserialiser would silently break the agent
    /// workflow. Pin every field name and the omission rules.
    /// What: builds the body with and without optional fields and asserts
    /// the resulting JSON layout.
    /// Test: this test.
    #[test]
    fn note_builds_expected_body() {
        // Minimal: only content.
        let b = build_request_body("hello", None, &[]);
        assert_eq!(b["content"], "hello");
        assert!(
            b.get("palace").is_none(),
            "palace must be omitted when None"
        );
        assert!(b.get("tags").is_none(), "tags must be omitted when empty");

        // Full: content + palace + tags.
        let b = build_request_body(
            "facts about quokkas",
            Some("wildlife"),
            &["marsupials".to_string(), "australia".to_string()],
        );
        assert_eq!(b["content"], "facts about quokkas");
        assert_eq!(b["palace"], "wildlife");
        let tags = b["tags"].as_array().expect("tags array");
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0], "marsupials");
        assert_eq!(tags[1], "australia");
    }
}
