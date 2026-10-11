//! The `ws:<session>/resume` Tier C slot a pause writes (#9390).
//!
//! Why: two of 13 relaunches on 2026-10-07 resumed from snapshots carrying PR
//! head SHAs that were wrong within 35 minutes. A resume needs the identifiers
//! it can re-read (branch, PR numbers) and the command that re-reads them, not
//! a head value. Written as an ADR-0028 Tier C slot, the next pause of the same
//! session supersedes it and an abandoned one expires.
//! What: [`record_resume_slot`] sends one `memory_remember` over the
//! trusty-memory socket with `fact_key` `ws:<session>/resume`, an `expires_at`
//! [`RESUME_SLOT_TTL_HOURS`] out, a text naming the branch, the PR numbers and
//! the snapshot path, and a `verify` of one command per identifier
//! (`gh pr view <n> --json headRefOid,state`, `git rev-parse <branch>`), built
//! from parsed numbers and a checked branch name, never from PM text. It is
//! fail-open: it returns the `resume_slot` field of the pause response and
//! never an error, and the socket call is bounded by [`SLOT_WRITE_TIMEOUT`].
//! Test: `pause_writes_a_resume_slot_with_identifiers_and_a_verify_command`,
//! `pause_survives_an_unreachable_memory_socket`,
//! `a_verify_record_failure_is_reported_as_stored_without_verify`,
//! `pr_numbers_come_from_pr_mentions_only`, `an_unsafe_branch_gets_no_command`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use trusty_common::catchup::git::capture_git_status;
use trusty_common::catchup::pause::{PauseSnapshotInput, PauseSnapshotOutcome};
use trusty_common::memory_rpc::{MemoryRpcError, call_memory_tool_at_with_timeout};

use crate::core::memory_verbs::{MemoryVerbOptions, REMEMBER_METHOD, resolve_palace_for};
// #9390: the reader runs only these shapes, so the writer formats with them too.
use crate::daemon::verify_allowlist::{AllowedCommand, MAX_PRS, is_safe_branch};

/// How long the slot write may hold the pause. A pause is interactive, and the
/// snapshot file, already written, is what it must not lose.
const SLOT_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// The slot's lifetime: ADR-0028's 24-hour Tier C default, stated explicitly.
const RESUME_SLOT_TTL_HOURS: i64 = 24;

/// Part of the text trusty-memory's `VerifyNotStored` renders. The type does
/// not cross the socket, only its message does.
const VERIFY_NOT_STORED_MARKER: &str = "is stored, but its verify command was not";

/// Where the resume slot goes: the memory socket and an optional palace.
#[derive(Debug, Clone)]
pub(crate) struct ResumeSlotHost {
    /// The trusty-memory socket to dial.
    pub(crate) socket: PathBuf,
    /// A palace named explicitly; `None` resolves it from the project.
    pub(crate) palace: Option<String>,
}

impl ResumeSlotHost {
    /// The daemon's own socket, palace resolved from the project.
    pub(crate) fn resolve() -> Self {
        Self {
            socket: trusty_common::memory_rpc::resolve_memory_socket_or_unreachable(),
            palace: None,
        }
    }
}

/// Write the resume slot for one pause; returns the `resume_slot` field.
///
/// What: `stored`; `stored_without_verify:<reason>` (`verify_not_stored` when
/// trusty-memory kept the drawer but not its command, `no_identifiers` when
/// there was nothing to re-read); `stored_unslotted:slot_refused` when the
/// daemon stored an ordinary drawer instead of a slot; or `skipped:<reason>`
/// (`invalid_session_id`, `no_palace`, `memory_unreachable`, `memory_error`,
/// `not_stored`). Every skip is logged with its detail.
/// Test: see the module doc.
pub(crate) async fn record_resume_slot(
    host: &ResumeSlotHost,
    project_path: &Path,
    input: &PauseSnapshotInput<'_>,
    outcome: &PauseSnapshotOutcome,
) -> String {
    let Some(fact_key) = slot_key(input.session_id) else {
        return skipped("invalid_session_id", input.session_id);
    };
    let opts = MemoryVerbOptions {
        palace: host.palace.clone(),
        socket: None,
        cwd: Some(project_path.to_path_buf()),
    };
    let palace = match resolve_palace_for(true, &opts) {
        Ok(Some(palace)) => palace,
        Ok(None) => return skipped("no_palace", "nothing resolved"),
        Err(e) => return skipped("no_palace", &e.to_string()),
    };
    let branch = capture_git_status(project_path)
        .branch
        .filter(|b| is_safe_branch(b));
    let prs = pr_numbers(input);
    let verify = verify_command(&prs, branch.as_deref());
    let expires_at = (chrono::Utc::now() + chrono::Duration::hours(RESUME_SLOT_TTL_HOURS))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let snapshot = outcome
        .snapshot_path
        .strip_prefix(project_path)
        .unwrap_or(&outcome.snapshot_path);
    let prs_text = if prs.is_empty() {
        "(none)".to_string()
    } else {
        let named: Vec<String> = prs.iter().map(|n| format!("#{n}")).collect();
        named.join(", ")
    };
    let text = format!(
        "Resume slot for session {}: branch {}; PRs {}; snapshot {}; expires_at {expires_at}.",
        input.session_id,
        branch.as_deref().unwrap_or("(none)"),
        prs_text,
        snapshot.display(),
    );
    let mut args = json!({
        "palace": palace,
        "text": text,
        "fact_key": fact_key,
        "expires_at": expires_at,
        // An app-managed write: bypass the quality gates, never the secret gate.
        "force": true,
    });
    if let Some(command) = &verify {
        args["verify"] = json!(command);
    }
    match call_memory_tool_at_with_timeout(&host.socket, REMEMBER_METHOD, args, SLOT_WRITE_TIMEOUT)
        .await
    {
        Ok(result) => stored_outcome(&result, verify.is_some()),
        Err(e) => error_outcome(&e),
    }
}

/// `ws:<session>/resume`, when the session id fits the slot-key grammar.
pub(crate) fn slot_key(session_id: &str) -> Option<String> {
    let fits = !session_id.is_empty()
        && session_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    let key = format!("ws:{session_id}/resume");
    // ADR-0028 D5 caps a key at 128 bytes.
    (fits && key.len() <= 128).then_some(key)
}

/// One re-read command per identifier, one per line; `None` when there are none.
fn verify_command(prs: &[u64], branch: Option<&str>) -> Option<String> {
    let mut lines: Vec<String> = prs
        .iter()
        .map(|n| AllowedCommand::GhPrView(*n).line())
        .collect();
    lines.extend(
        branch
            .and_then(AllowedCommand::git_rev_parse)
            .map(|c| c.line()),
    );
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// The PR numbers the pause's free text names, in order of first mention.
///
/// What: a number counts when it follows `PR`, `PRs` or `pull/` at a word
/// start, optionally after spaces and one `#` (`PR #12`, `pr 12`, `PR#12`,
/// `.../pull/12`), and ends at a word boundary. A bare `#9390` is not a PR: it
/// may name an issue. At most [`MAX_PRS`].
/// Test: `pr_numbers_come_from_pr_mentions_only`.
fn pr_numbers(input: &PauseSnapshotInput<'_>) -> Vec<u64> {
    let mut out = Vec::new();
    let texts = std::iter::once(input.summary)
        .chain(input.completed.iter().map(String::as_str))
        .chain(input.in_progress.iter().map(String::as_str))
        .chain(input.next_steps.iter().map(String::as_str));
    for text in texts {
        prs_in(text.as_bytes(), &mut out);
    }
    out
}

/// Append each PR number `text` names to `out`; see [`pr_numbers`].
fn prs_in(text: &[u8], out: &mut Vec<u64>) {
    let starts = |at: usize, word: &[u8]| {
        text.get(at..at + word.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(word))
    };
    for i in 0..text.len() {
        if out.len() >= MAX_PRS {
            return;
        }
        if i > 0 && text[i - 1].is_ascii_alphanumeric() {
            continue;
        }
        let mut j = if starts(i, b"pull/") {
            i + 5
        } else if starts(i, b"prs") && !text.get(i + 3).is_some_and(u8::is_ascii_alphabetic) {
            i + 3
        } else if starts(i, b"pr") && !text.get(i + 2).is_some_and(u8::is_ascii_alphabetic) {
            i + 2
        } else {
            continue;
        };
        while text.get(j) == Some(&b' ') {
            j += 1;
        }
        if text.get(j) == Some(&b'#') {
            j += 1;
        }
        let digits = text[j..].iter().take_while(|b| b.is_ascii_digit()).count();
        let end = j + digits;
        if digits == 0 || digits > 9 || text.get(end).is_some_and(u8::is_ascii_alphanumeric) {
            continue;
        }
        let number = std::str::from_utf8(&text[j..end])
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0);
        if let Some(n) = number
            && !out.contains(&n)
        {
            out.push(n);
        }
    }
}

/// The `resume_slot` field for a call trusty-memory answered.
fn stored_outcome(result: &Value, has_verify: bool) -> String {
    if result.get("status").and_then(Value::as_str) != Some("stored") {
        return skipped("not_stored", &result.to_string());
    }
    if result.get("tier").and_then(Value::as_str) != Some("C") {
        let refused = result.get("tier_c_refused").and_then(Value::as_str);
        tracing::warn!(
            "session_context_pause: the resume slot was stored as an ordinary drawer: {}",
            refused.unwrap_or("no reason given")
        );
        return "stored_unslotted:slot_refused".to_string();
    }
    if has_verify {
        "stored".to_string()
    } else {
        "stored_without_verify:no_identifiers".to_string()
    }
}

/// The `resume_slot` field for a call that failed.
fn error_outcome(e: &anyhow::Error) -> String {
    let detail = format!("{e:#}");
    match e.downcast_ref::<MemoryRpcError>() {
        // #9390: PR B keeps the drawer and its slot; only the command is missing.
        Some(rpc) if rpc.message.contains(VERIFY_NOT_STORED_MARKER) => {
            tracing::warn!("session_context_pause: resume slot stored without verify: {detail}");
            "stored_without_verify:verify_not_stored".to_string()
        }
        Some(_) => skipped("memory_error", &detail),
        None => skipped("memory_unreachable", &detail),
    }
}

/// `skipped:<reason>`, logged with `detail`.
fn skipped(reason: &str, detail: &str) -> String {
    tracing::warn!("session_context_pause: resume slot skipped ({reason}): {detail}");
    format!("skipped:{reason}")
}

#[cfg(test)]
#[path = "pause_resume_slot_tests.rs"]
mod tests;
