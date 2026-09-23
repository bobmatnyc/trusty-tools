//! `tm session adopt-worktree` — transfer a dead owner's worktree to a live
//! session (#6497).
//!
//! Why: `session.rs` is a dispatch table, and this verb needs a refusal path of
//! its own: the daemon answers a refusal with 409 and the gate's own reason,
//! which the operator has to read verbatim rather than as a generic HTTP error.
//! What: one POST; the refusal reason is printed and the process exits
//! non-zero, so a script cannot mistake a refusal for a transfer. #8318: a
//! transfer that left the branch held, or failed to clear a dead agent's lock,
//! also exits non-zero, because the next agent still cannot use the tree.
//! Test: `cli_parses_session_adopt_worktree`,
//! `cli_adopt_worktree_requires_the_adopting_session`,
//! `adoption_report_prints_a_released_branch_and_a_cleared_lock`,
//! `adoption_report_fails_when_the_branch_is_kept`,
//! `adoption_report_fails_when_the_daemon_reports_no_branch_outcome`,
//! `adoption_report_fails_when_the_dead_lock_could_not_be_cleared`,
//! `adoption_outcome_fails_on_a_malformed_body`.

use std::path::Path;

use serde_json::Value;

/// POST the adoption request and report what the daemon decided (#6497).
///
/// Why: the daemon owns the decision — it is the only process holding both the
/// delegation map and the session-record store — so this function's whole job
/// is to carry the answer back without softening it.
/// What: 200 prints [`adoption_report`]'s lines, which can still fail the
/// command (#8318); 409 prints the refusal reason and returns an error so the
/// exit status is non-zero; anything else surfaces as the HTTP error it is.
/// Test: `cli_parses_session_adopt_worktree`, `adoption_outcome_fails_on_a_malformed_body`.
pub(crate) async fn session_adopt_worktree(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
    as_session: &str,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed/adopt-worktree"))
        .json(&serde_json::json!({ "path": path, "as_session": as_session }))
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status.as_u16() == 409 {
        // The gate's own words. Paraphrasing them would hide which of the three
        // refusal arms fired, which is the only thing that tells the operator
        // what to do next.
        anyhow::bail!("adoption refused: {body}");
    }
    if !status.is_success() {
        anyhow::bail!("adopt-worktree failed ({status}): {body}");
    }
    println!("{}", adoption_outcome(&body, path, as_session)?);
    Ok(())
}

/// Parse a 200 body and render it through [`adoption_report`] (#8318).
///
/// Why: a malformed body used to become `Value::Null`, which the report read as
/// "the daemon predates #8318" — a wrong diagnosis for a corrupt response.
/// What: a parse failure is an error naming the parse error and the body.
/// Test: `adoption_outcome_fails_on_a_malformed_body`.
pub(crate) fn adoption_outcome(
    body: &str,
    path: &Path,
    as_session: &str,
) -> anyhow::Result<String> {
    // #8318: fail with the parse error, never a guessed daemon version.
    let parsed: Value = serde_json::from_str(body).map_err(|e| {
        anyhow::anyhow!("adopt-worktree answered 200 with a body that is not JSON ({e}): {body}")
    })?;
    adoption_report(&parsed, path, as_session)
}

/// Render a successful adoption, failing when the tree is still unusable
/// (#8318).
///
/// Why: ownership moving is not the goal; the next agent checking the branch
/// out is. A kept branch, a lock the daemon could not clear, or a daemon too
/// old to say either leaves the operator with work to do, and a zero exit
/// would hide that from a script.
/// What: `Ok(lines)` when the branch was released or HEAD was already
/// detached and no dead lock failed to clear; `Err` naming the reason
/// otherwise. A running pid's lock is reported but is not a failure.
/// Test: `adoption_report_prints_a_released_branch_and_a_cleared_lock`,
/// `adoption_report_fails_when_the_branch_is_kept`,
/// `adoption_report_fails_when_the_daemon_reports_no_branch_outcome`,
/// `adoption_report_fails_when_the_dead_lock_could_not_be_cleared`.
pub(crate) fn adoption_report(
    body: &Value,
    path: &Path,
    as_session: &str,
) -> anyhow::Result<String> {
    let mut lines = vec![format!("adopted {} as {as_session}", path.display())];
    let lock = &body["harness_lock"];
    match lock["outcome"].as_str() {
        Some("cleared") => lines.push(format!(
            "cleared the harness git lock left by dead pid {}",
            lock["pid"]
        )),
        Some("left_running") => lines.push(format!(
            "left the harness git lock in place: pid {} is running",
            lock["pid"]
        )),
        Some("failed") => anyhow::bail!(
            "{}\nthe harness git lock names dead pid {}, but clearing it failed: {}",
            lines.join("\n"),
            lock["pid"],
            lock["reason"].as_str().unwrap_or("no reason given")
        ),
        _ => {}
    }
    let branch = &body["branch_release"];
    match branch["outcome"].as_str() {
        Some("released") => lines.push(format!(
            "released branch {} (HEAD detached); the next agent can check it out",
            branch["branch"].as_str().unwrap_or("?")
        )),
        Some("already_detached") => lines.push("HEAD was already detached".to_string()),
        Some("kept") => anyhow::bail!(
            "{}\nbranch NOT released: {}",
            lines.join("\n"),
            branch["reason"].as_str().unwrap_or("no reason given")
        ),
        _ => anyhow::bail!(
            "{}\nthe daemon did not report whether it released the branch (it predates \
             #8318); upgrade it, or run `git -C {} switch --detach` on a clean tree",
            lines.join("\n"),
            path.display()
        ),
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn adoption_report_prints_a_released_branch_and_a_cleared_lock() {
        let body = json!({
            "branch_release": {"outcome": "released", "branch": "session/x"},
            "harness_lock": {"outcome": "cleared", "pid": 42},
        });
        let text = adoption_report(&body, Path::new("/t"), "s1").expect("a freed tree succeeds");
        assert!(text.contains("released branch session/x"), "{text}");
        assert!(text.contains("dead pid 42"), "{text}");
    }

    /// #8318 error arm: a dirty tree keeps its branch and the command fails.
    #[test]
    fn adoption_report_fails_when_the_branch_is_kept() {
        let body = json!({
            "branch_release": {"outcome": "kept", "reason": "the tree holds 2 files"},
            "harness_lock": {"outcome": "not_cleared"},
        });
        let err = adoption_report(&body, Path::new("/t"), "s1").expect_err("must fail");
        assert!(err.to_string().contains("the tree holds 2 files"), "{err}");
    }

    /// #8318 critic MEDIUM-2: a dead pid's lock that could not be cleared
    /// fails the command even when the branch was released.
    #[test]
    fn adoption_report_fails_when_the_dead_lock_could_not_be_cleared() {
        let body = json!({
            "branch_release": {"outcome": "released", "branch": "session/x"},
            "harness_lock": {"outcome": "failed", "pid": 42, "reason": "permission denied"},
        });
        let err = adoption_report(&body, Path::new("/t"), "s1").expect_err("must fail");
        let msg = err.to_string();
        assert!(msg.contains("dead pid 42"), "{msg}");
        assert!(msg.contains("permission denied"), "{msg}");
    }

    /// 🔴 #8318 critic LOW-2: a malformed 200 body fails with the parse error,
    /// never the "daemon predates #8318" diagnosis a `Value::Null` produced.
    #[test]
    fn adoption_outcome_fails_on_a_malformed_body() {
        let err = adoption_outcome("<html>proxy error</html>", Path::new("/t"), "s1")
            .expect_err("a malformed body must fail");
        let msg = err.to_string();
        assert!(msg.contains("not JSON"), "{msg}");
        assert!(!msg.contains("predates"), "{msg}");
    }

    #[test]
    fn adoption_report_fails_when_the_daemon_reports_no_branch_outcome() {
        let body = json!({"adopted": true});
        let err = adoption_report(&body, Path::new("/t"), "s1").expect_err("must fail");
        assert!(err.to_string().contains("predates #8318"), "{err}");
    }
}
