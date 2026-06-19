//! Managed session-manager methods for [`DaemonClient`].
//!
//! Why: the `/api/v1/sessions/managed/*` route family is the convergence target
//! for every managed-session UI — the `tm` CLI today, STUI (#1272) and TELUI
//! (#1433) next. Before this module each surface hand-rolled `reqwest` calls and
//! ad-hoc response structs; gathering them as typed [`DaemonClient`] methods wires
//! each endpoint exactly once. They live in a sibling file (not `mod.rs`) so the
//! 463-SLOC client `mod.rs` stays under the 500-SLOC cap; this file can reach the
//! `base`/`http` fields because they are `pub(in crate::client::http_client)`.
//! What: one async method per managed endpoint, each building the URL from
//! [`DaemonClient::base`], sending via the shared `reqwest::Client`, checking the
//! status, and deserializing the typed wire DTO from `super::types`.
//! Test: `cargo test -p trusty-mpm` (`managed_*` round-trip tests in `tests.rs`)
//! covers wire-shape deserialization; live HTTP is exercised by the daemon's
//! `tests/session_manager_mvp.rs` integration suite.

use anyhow::Context;

use super::DaemonClient;
use super::types::{
    ManagedActivityResponse, ManagedAnswerRequest, ManagedAnswerResponse, ManagedAttachCmdResponse,
    ManagedListResponse, ManagedSendInputRequest, ManagedSendInputResponse, ManagedSessionSummary,
    ManagedSpawnRequest, ManagedSpawnResponse,
};

impl DaemonClient {
    /// List every managed session via `GET /api/v1/sessions/managed`.
    ///
    /// Why: the managed-session list view and the id-or-name resolver both read
    /// the full session list from this one call.
    /// What: GETs the managed-list endpoint and returns the `sessions` array.
    /// Test: `managed_list_response_deserializes` covers the wire shape; live
    /// HTTP via `tests/session_manager_mvp.rs`.
    pub async fn list_managed_sessions(&self) -> anyhow::Result<Vec<ManagedSessionSummary>> {
        let url = format!("{}/api/v1/sessions/managed", self.base);
        let body: ManagedListResponse = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(body.sessions)
    }

    /// Fetch one managed session via `GET /api/v1/sessions/managed/{id}`.
    ///
    /// Why: a detail view needs a single session without listing every one.
    /// What: GETs the per-session endpoint and returns the [`ManagedSessionSummary`];
    /// a 404 (and any other non-success status) surfaces as an `Err`.
    /// Test: live HTTP via `tests/session_manager_mvp.rs`.
    pub async fn get_managed_session(&self, id: &str) -> anyhow::Result<ManagedSessionSummary> {
        let url = format!("{}/api/v1/sessions/managed/{id}", self.base);
        let summary = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(summary)
    }

    /// Spawn a managed session via `POST /api/v1/sessions/managed`.
    ///
    /// Why: provision an isolated workspace and start a harness in it.
    /// What: POSTs `req` and returns the daemon's [`ManagedSpawnResponse`].
    /// Test: live HTTP via `tests/session_manager_mvp.rs`.
    pub async fn spawn_managed_session(
        &self,
        req: &ManagedSpawnRequest,
    ) -> anyhow::Result<ManagedSpawnResponse> {
        let url = format!("{}/api/v1/sessions/managed", self.base);
        let resp = self
            .http
            .post(&url)
            .json(req)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    /// Inject text into a session's pane via `POST .../{id}/send`.
    ///
    /// Why: drive a harness with a prompt without attaching to tmux.
    /// What: POSTs `{ text }` and returns the [`ManagedSendInputResponse`].
    /// Test: live HTTP via `tests/session_manager_mvp.rs`.
    pub async fn send_to_managed_session(
        &self,
        id: &str,
        text: &str,
    ) -> anyhow::Result<ManagedSendInputResponse> {
        let url = format!("{}/api/v1/sessions/managed/{id}/send", self.base);
        let resp = self
            .http
            .post(&url)
            .json(&ManagedSendInputRequest {
                text: text.to_string(),
            })
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    /// Answer a pending decision via `POST .../{id}/answer`.
    ///
    /// Why: unblock a harness waiting on an operator decision.
    /// What: POSTs `{ answer }` and returns the [`ManagedAnswerResponse`].
    /// Test: live HTTP via `tests/session_manager_mvp.rs`.
    pub async fn answer_managed_session(
        &self,
        id: &str,
        answer: &str,
    ) -> anyhow::Result<ManagedAnswerResponse> {
        let url = format!("{}/api/v1/sessions/managed/{id}/answer", self.base);
        let resp = self
            .http
            .post(&url)
            .json(&ManagedAnswerRequest {
                answer: answer.to_string(),
            })
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    /// Inspect a session's activity via `GET .../{id}/activity`.
    ///
    /// Why: see what a session is doing without attaching; the raw pane is always
    /// returned so the caller can reason over it even without an LLM key.
    /// What: GETs the activity endpoint and returns the [`ManagedActivityResponse`].
    /// Test: live HTTP via `tests/session_manager_mvp.rs`.
    pub async fn managed_session_activity(
        &self,
        id: &str,
    ) -> anyhow::Result<ManagedActivityResponse> {
        let url = format!("{}/api/v1/sessions/managed/{id}/activity", self.base);
        let resp = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    /// Fetch the tmux attach command via `GET .../{id}/attach-cmd`.
    ///
    /// Why: an operator needs the exact `tmux attach` invocation to take over.
    /// What: GETs the attach-cmd endpoint and returns the [`ManagedAttachCmdResponse`].
    /// Test: live HTTP via `tests/session_manager_mvp.rs`.
    pub async fn managed_session_attach_cmd(
        &self,
        id: &str,
    ) -> anyhow::Result<ManagedAttachCmdResponse> {
        let url = format!("{}/api/v1/sessions/managed/{id}/attach-cmd", self.base);
        let resp = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    /// Stop a session's runtime via `POST .../{id}/runtime-stop`.
    ///
    /// Why: a session endures beyond its runtime; this kills only the tmux session
    /// and harness process, preserving the workspace for a later resume.
    /// What: POSTs to the runtime-stop endpoint and returns the updated summary.
    /// Test: live HTTP via `tests/session_manager_mvp.rs`.
    pub async fn runtime_stop_managed_session(
        &self,
        id: &str,
    ) -> anyhow::Result<ManagedSessionSummary> {
        let url = format!("{}/api/v1/sessions/managed/{id}/runtime-stop", self.base);
        let resp = self
            .http
            .post(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    /// Resume a stopped session via `POST .../{id}/resume`.
    ///
    /// Why: after a runtime-stop the workspace is still on disk; resume re-spawns
    /// the runtime there without re-cloning.
    /// What: POSTs to the resume endpoint and returns the updated summary. The
    /// daemon answers with typed 404/409/500 errors; rather than swallow them,
    /// a non-success status is flattened into an `anyhow` error that carries the
    /// HTTP status and the response body text so the caller can surface both.
    /// Test: live HTTP via `tests/session_manager_mvp.rs`.
    pub async fn resume_managed_session(&self, id: &str) -> anyhow::Result<ManagedSessionSummary> {
        let url = format!("{}/api/v1/sessions/managed/{id}/resume", self.base);
        let resp = self.http.post(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("resume failed (HTTP {status}): {body}");
        }
        let summary = resp.json().await.context("decoding resume response body")?;
        Ok(summary)
    }

    /// Decommission a session via `POST .../{id}/decommission`.
    ///
    /// Why: the only operation that permanently removes the workspace directory;
    /// it is terminal — no resume is possible afterward.
    /// What: POSTs to the decommission endpoint and returns the tombstone summary.
    /// Test: live HTTP via `tests/session_manager_mvp.rs`.
    pub async fn decommission_managed_session(
        &self,
        id: &str,
    ) -> anyhow::Result<ManagedSessionSummary> {
        let url = format!("{}/api/v1/sessions/managed/{id}/decommission", self.base);
        let resp = self
            .http
            .post(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }
}
