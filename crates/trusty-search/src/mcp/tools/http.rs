//! HTTP transport helpers for the MCP tool dispatcher.
//!
//! Why: the four HTTP verbs (`GET`, `GET text/plain`, `POST`, `DELETE`) used
//! by the tool arms all share identical error-mapping and status-code logic.
//! Centralising them here means each tool arm is a thin wrapper that only
//! describes *what* to send, not *how* to handle the response.
//! What: `get`, `get_text`, `post`, `delete` — all implemented as inherent
//! methods on `McpServer` that forward to the daemon and map HTTP errors to
//! `DispatchError` variants.
//! Test: indirectly covered by the tool-dispatch tests in `tests.rs` and
//! `tests_lane.rs` (every test that spins up a mock daemon exercises these
//! paths).

use serde_json::Value;

use super::{types::DispatchError, McpServer};

impl McpServer {
    /// GET an endpoint that returns JSON.
    ///
    /// Why: most read-only endpoints (list_indexes, index_status, health)
    /// return JSON bodies; one shared helper avoids copy-pasting the
    /// response-decoding and error-mapping logic.
    /// What: GETs `{base_url}{path}`, decodes the JSON body, returns
    /// `DispatchError::Transport` on network or decode failure.
    /// Test: `search_health` and `index_status` arms exercise this.
    pub(super) async fn get(&self, path: &str) -> Result<Value, DispatchError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| DispatchError::Transport(format!("GET {url}: {e}")))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| DispatchError::Transport(format!("decode {url}: {e}")))?;
        if !status.is_success() {
            return Err(DispatchError::Transport(format!(
                "GET {url} returned {status}: {body}"
            )));
        }
        Ok(body)
    }

    /// GET an endpoint that returns `text/plain`.
    ///
    /// Why: `get_call_chain` (issue #76) returns prose intended for direct LLM
    /// consumption; it cannot share the JSON `get` helper.
    /// What: GETs `{base_url}{path}?{query}`, reads the body as a `String`,
    /// maps HTTP 400 to `InvalidParams` and other failures to `Transport`.
    /// Test: `get_call_chain` arm exercises this path.
    pub(super) async fn get_text(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<String, DispatchError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .get(&url)
            .query(query)
            .send()
            .await
            .map_err(|e| DispatchError::Transport(format!("GET {url}: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| DispatchError::Transport(format!("decode {url}: {e}")))?;
        if !status.is_success() {
            // 400 from the daemon means invalid params; surface that to the
            // caller as an INVALID_PARAMS error rather than INTERNAL_ERROR.
            if status == reqwest::StatusCode::BAD_REQUEST {
                return Err(DispatchError::InvalidParams(body));
            }
            return Err(DispatchError::Transport(format!(
                "GET {url} returned {status}: {body}"
            )));
        }
        Ok(body)
    }

    /// POST a JSON body to an endpoint and decode the JSON response.
    ///
    /// Why: the majority of mutating endpoints (index_file, reindex, search,
    /// grep, …) are POSTs; one helper maps the full response lifecycle.
    /// What: POSTs `body` as `application/json` to `{base_url}{path}`, decodes
    /// the response, maps HTTP 400 to `InvalidParams` (issue #882) and other
    /// failures to `Transport`.
    /// Test: most tool arms in `search.rs`, `index.rs`, and `misc.rs` exercise
    /// this path.
    pub(super) async fn post(&self, path: &str, body: &Value) -> Result<Value, DispatchError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(|e| DispatchError::Transport(format!("POST {url}: {e}")))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| DispatchError::Transport(format!("decode {url}: {e}")))?;
        if !status.is_success() {
            // Issue #882: 400 means invalid input — surface as InvalidParams.
            if status == reqwest::StatusCode::BAD_REQUEST {
                let msg = body
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("bad request")
                    .to_owned();
                return Err(DispatchError::InvalidParams(msg));
            }
            return Err(DispatchError::Transport(format!(
                "POST {url} returned {status}: {body}"
            )));
        }
        Ok(body)
    }

    /// DELETE an endpoint and decode the JSON response.
    ///
    /// Why: `delete_index` is the only DELETE endpoint; a dedicated helper
    /// keeps the tool arm simple.
    /// What: sends DELETE to `{base_url}{path}`, decodes JSON response, maps
    /// failure to `DispatchError::Transport`.
    /// Test: `delete_index` arm exercises this path.
    pub(super) async fn delete(&self, path: &str) -> Result<Value, DispatchError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .delete(&url)
            .send()
            .await
            .map_err(|e| DispatchError::Transport(format!("DELETE {url}: {e}")))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| DispatchError::Transport(format!("decode {url}: {e}")))?;
        if !status.is_success() {
            return Err(DispatchError::Transport(format!(
                "DELETE {url} returned {status}: {body}"
            )));
        }
        Ok(body)
    }
}
