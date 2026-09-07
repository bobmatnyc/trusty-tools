//! Ollama probe helpers for the REPL.
//!
//! Why: `/provider local` and `/local` slash commands need to verify a
//! running ollama instance and list its pulled models before flipping the
//! provider override. Extracting these helpers keeps `mod.rs` lean and
//! makes the probe path independently testable.
//!
//! Why they no longer speak HTTP themselves (#4490): this module used to GET
//! `{host}/api/tags` through its own client with its own 2-second timeout — a
//! second independent implementation of the liveness question
//! `trusty_common::local_probe` already answers for `chat::` and for the shared
//! `LocalAdapter`. Three copies of one probe drift: they disagreed on the
//! budget (2s here, 1s there, 500ms in `local_inference`) and on the endpoint,
//! so "is the local server up" could get two different answers in one process.
//! The REPL needs the model LIST as well as liveness, so the shared probe grew
//! [`trusty_common::local_probe::list_models`] rather than this module keeping a
//! private HTTP call — one request over the OpenAI-dialect `/v1/models`
//! endpoint answers both, and covers LM Studio and vLLM, which `/api/tags`
//! never did.
//!
//! What: `probe_ollama` delegates to `local_probe::list_models` and returns the
//! model names; `ollama_host` delegates to `local_probe::local_host`. The
//! rendering stays with the callers in `repl::commands::routing`.
//! Test: `tests::probe_reports_a_dead_host_inside_the_shared_budget`,
//! `tests::probe_error_names_the_endpoint_it_dialled`,
//! `tests::host_follows_the_shared_resolver`.

use anyhow::{Context, Result};
use trusty_common::local_probe;

/// Probe a local ollama server and return the list of model names.
///
/// Why: `/provider local` lets the user route LLM calls to a locally-running
/// ollama instance. Before flipping the override we must verify ollama is
/// running and surface its model list so the user can pick one with `/model`.
/// What: delegates to [`local_probe::list_models`], which GETs
/// `{host}/v1/models` inside the shared
/// [`local_probe::LOCAL_PROBE_TIMEOUT`] and returns the served ids. The typed
/// probe error already names the endpoint it dialled; the context added here
/// keeps the REPL's existing "connecting to …" framing.
/// Test: `tests::probe_reports_a_dead_host_inside_the_shared_budget`,
/// `tests::probe_error_names_the_endpoint_it_dialled`.
pub(crate) async fn probe_ollama(host: &str) -> Result<Vec<String>> {
    // #4490: one probe, one budget — see the module header.
    local_probe::list_models(host)
        .await
        .with_context(|| format!("probing local model server at {host}"))
}

/// Resolve the configured ollama host (env override or default).
///
/// Why (#4490): four call sites read `OLLAMA_HOST`; one resolver is what keeps
/// the probe and the request dialling the same machine.
/// What: delegates to [`local_probe::local_host`].
/// Test: `tests::host_follows_the_shared_resolver`.
pub(crate) fn ollama_host() -> String {
    local_probe::local_host()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use trusty_common::local_probe::LOCAL_PROBE_TIMEOUT;

    use super::*;

    /// Why (#4490): the REPL blocks on this probe, so the budget IS the user
    /// experience — the old bespoke call used 2s, and a wedged server (accepts,
    /// never answers) is exactly the case a connect timeout does not cover.
    /// What it pins: the REPL path returns inside the SHARED constant plus a
    /// generous margin against a black-hole listener, which is only true while
    /// it delegates to `local_probe`.
    /// Test: this test.
    #[tokio::test]
    async fn probe_reports_a_dead_host_inside_the_shared_budget() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind black hole");
        let addr = listener.local_addr().expect("addr").to_string();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });

        let started = std::time::Instant::now();
        let err = probe_ollama(&format!("http://{addr}"))
            .await
            .expect_err("a server that never answers must fail");
        let elapsed = started.elapsed();

        assert!(
            elapsed < LOCAL_PROBE_TIMEOUT + Duration::from_secs(2),
            "probe took {elapsed:?}, past the shared {LOCAL_PROBE_TIMEOUT:?} budget"
        );
        assert!(format!("{err:#}").contains(&addr), "{err:#}");
    }

    /// Why: the operator's next move after a failed probe is to check the right
    /// machine, so the endpoint must survive into the REPL's rendering.
    /// What it pins: a closed port fails naming `{host}/v1/models`.
    /// Test: this test.
    #[tokio::test]
    async fn probe_error_names_the_endpoint_it_dialled() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind to free a port");
        let addr = listener.local_addr().expect("addr").to_string();
        drop(listener);

        let err = probe_ollama(&format!("http://{addr}"))
            .await
            .expect_err("closed port must fail");
        assert!(
            format!("{err:#}").contains(&format!("http://{addr}/v1/models")),
            "{err:#}"
        );
    }

    /// Why (#4490): this function used to carry its own copy of the default and
    /// the env read; a drift here sends the REPL to a different host than the
    /// adapter that ultimately serves the turn.
    /// What it pins: the REPL's host IS the shared resolver's answer.
    /// Test: this test.
    #[test]
    fn host_follows_the_shared_resolver() {
        assert_eq!(ollama_host(), local_probe::local_host());
    }
}
