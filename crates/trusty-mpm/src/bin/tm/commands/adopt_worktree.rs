//! `tm session adopt-worktree` — transfer a dead owner's worktree to a live
//! session (#6497).
//!
//! Why: `session.rs` is a dispatch table, and this verb needs a refusal path of
//! its own: the daemon answers a refusal with 409 and the gate's own reason,
//! which the operator has to read verbatim rather than as a generic HTTP error.
//! What: one POST; the refusal reason is printed and the process exits
//! non-zero, so a script cannot mistake a refusal for a transfer.
//! Test: `cli_parses_session_adopt_worktree`,
//! `cli_adopt_worktree_requires_the_adopting_session`.

use std::path::Path;

/// POST the adoption request and report what the daemon decided (#6497).
///
/// Why: the daemon owns the decision — it is the only process holding both the
/// delegation map and the session-record store — so this function's whole job
/// is to carry the answer back without softening it.
/// What: 200 prints the new owner; 409 prints the refusal reason and returns an
/// error so the exit status is non-zero; anything else surfaces as the HTTP
/// error it is.
/// Test: `cli_parses_session_adopt_worktree`.
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
    println!("adopted {} as {as_session}", path.display());
    Ok(())
}
