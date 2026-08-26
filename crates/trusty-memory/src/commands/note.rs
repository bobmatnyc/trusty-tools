//! Handler for `trusty-memory note` — a memory save from a shell.
//!
//! Why: sub-agents spawned via Claude Code's Agent tool inherit no MCP
//! connections, so the `mcp__trusty-memory__memory_remember` tool is
//! unreachable to them. They *can* run shell commands. This subcommand gives
//! them a writable handle that needs no MCP plumbing — it calls the daemon's
//! `memory.remember_async` method, which queues the redb write on a detached
//! task and answers `{"status":"queued"}` without waiting for it.
//!
//! What: builds the params, calls the method through [`crate::client`], prints
//! `"Queued."` on success.
//!
//! **A dropped write exits non-zero (#6286).** This handler used to print a
//! warning and `return Ok(())` on every failure arm — daemon down, transport
//! error, refused params — so a caller that saved nothing saw exit 0 and had no
//! way to learn its memory was lost. Fire-and-forget describes what the DAEMON
//! does after it accepts the write, not what the CLI reports about whether it
//! was accepted: the queue-side outcome is genuinely unavailable to the caller,
//! but the accept-side outcome is exactly what the call returns. An agent that
//! wants the old behaviour appends `|| true`; one that wants to retry now can.
//!
//! Test: `note_builds_expected_body` covers the params shape;
//! `note_reports_a_dropped_write_as_a_failure` covers the exit contract; the
//! method itself is covered by `transport::uds::tests::rpc_remember_async_*`.

use anyhow::{Context, Result};
use serde_json::Value;
use std::time::Duration;

/// Budget for the note call.
///
/// Why: the daemon's response path is "validate params, spawn task, answer
/// queued", which should never take more than a few hundred milliseconds. A
/// two-second ceiling leaves room for a paged-out cache while still letting the
/// CLI fail promptly when the daemon is not running.
const CALL_TIMEOUT: Duration = Duration::from_secs(2);

/// The method that accepts the queued write.
///
/// Named as a constant because `trusty-memory note` is the only caller and the
/// name is what a rename has to change in one place. Folded in
/// `transport::methods::admin`.
const REMEMBER_ASYNC: &str = "memory.remember_async";

/// Build the params for [`REMEMBER_ASYNC`] from CLI inputs.
///
/// Why: pulled out so the shape can be asserted from a unit test without a
/// daemon. It is the contract between this CLI and
/// `transport::methods::admin::RememberAsyncParams` — drift here is a silent
/// break.
/// What: `content` always; `palace` and `tags` only when non-empty, so the
/// daemon can fall back to its `--palace` default and a tag-less store.
/// Test: `note_builds_expected_body`.
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
/// Why: the CLI face of the queued save path. Sub-agents call this from `Bash`
/// to land a memory without inheriting an MCP connection.
///
/// # Errors
///
/// When the daemon is not running, when the call fails in transport, or when
/// the daemon refuses the content (empty, too short, a secret). Every one of
/// those means nothing was written, and the caller learns so from the exit
/// status — see the module doc for why this is not fire-and-forget.
///
/// Test: `note_reports_a_dropped_write_as_a_failure`.
pub async fn handle_note(content: String, palace: Option<String>, tags: Vec<String>) -> Result<()> {
    // Validated locally so the CLI does not contact the daemon at all for an
    // empty string. Exit 2 (rather than 1) marks a usage error, which is the
    // code this arm has always used.
    if content.trim().is_empty() {
        eprintln!("trusty-memory note: 'content' must not be empty");
        std::process::exit(2);
    }

    let params = build_request_body(&content, palace.as_deref(), &tags);
    crate::client::call_with_timeout(REMEMBER_ASYNC, params, CALL_TIMEOUT)
        .await
        .context(
            "trusty-memory note: the memory was NOT written. \
             If the daemon is not running, start it with `trusty-memory start`.",
        )?;
    println!("Queued.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `build_request_body` emits exactly the fields
    /// `admin::RememberAsyncParams` deserialises, omitting `palace` and `tags`
    /// when the caller does not set them.
    ///
    /// Why: drift between the CLI's params and the daemon's struct would
    /// silently break the agent workflow. Pin every field name and the omission
    /// rules.
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

    /// Why: this is the finding. Every failure arm used to be `eprintln!` plus
    /// `return Ok(())`, so a caller whose write was dropped got exit 0 and no
    /// way to know. The daemon being absent is the common case of that, and it
    /// must now be an error the shell can branch on.
    /// What: points the data dir at an empty tempdir, so the derived socket path
    /// exists nowhere and the dial is refused, then asserts `handle_note`
    /// reports it rather than swallowing it.
    /// Test: itself.
    #[tokio::test]
    async fn note_reports_a_dropped_write_as_a_failure() {
        let _guard = crate::commands::env_test_lock().lock().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: test serialised by env_test_lock.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
        }
        let result = handle_note("a note nobody will store".to_string(), None, Vec::new()).await;
        unsafe {
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
        let err = result.expect_err("a write nothing accepted must not report success");
        assert!(
            format!("{err:#}").contains("NOT written"),
            "the failure must say the memory was not stored: {err:#}"
        );
    }
}
