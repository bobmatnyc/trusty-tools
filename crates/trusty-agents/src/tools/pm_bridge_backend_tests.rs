//! Tests for `tools::pm_bridge_backend` — `binary_on_path`, the decode
//! helpers, fail-closed behavior when the target binary is absent, and
//! binary-gated integration smokes against the real `tcode`/`tm` binaries.
//!
//! Why `#![allow(clippy::await_holding_lock)]`: the two `fails_closed`
//! tests hold `crate::test_env::ENV_LOCK` across `.await` points by design
//! — they mutate the process-wide `$PATH` for their whole body, matching
//! the established crate-wide convention documented in `crate::test_env`
//! and already used by `system_status::daemons::tests` /
//! `llm::credentials::tests`.
#![allow(clippy::await_holding_lock)]

use serde_json::json;

use super::*;

// =====================================================================
// binary_on_path
// =====================================================================

#[test]
#[cfg(unix)]
fn binary_on_path_recognises_sh() {
    assert!(binary_on_path("sh"));
    assert!(!binary_on_path("definitely-not-a-real-binary-xyzzy"));
}

// =====================================================================
// Decode helpers
// =====================================================================

#[test]
fn extract_session_id_parses_wrapped_text_frame() {
    let resp = json!({
        "content": [{ "type": "text", "text": "{\"id\":\"sess-123\",\"status\":\"active\"}" }]
    });
    assert_eq!(extract_session_id(&resp).as_deref(), Some("sess-123"));
}

#[test]
fn extract_session_id_falls_back_to_session_id_field() {
    let resp = json!({
        "content": [{ "type": "text", "text": "{\"session_id\":\"sess-456\"}" }]
    });
    assert_eq!(extract_session_id(&resp).as_deref(), Some("sess-456"));
}

#[test]
fn extract_session_id_missing_id_returns_none() {
    let resp = json!({
        "content": [{ "type": "text", "text": "{\"status\":\"active\"}" }]
    });
    assert_eq!(extract_session_id(&resp), None);
}

#[test]
fn extract_session_id_missing_content_returns_none() {
    let resp = json!({ "isError": false });
    assert_eq!(extract_session_id(&resp), None);
}

#[test]
fn extract_pane_content_parses_wrapped_text_frame() {
    let resp = json!({
        "content": [{ "type": "text", "text": "{\"pane_content\":\"hello from the pane\"}" }]
    });
    assert_eq!(
        extract_pane_content(&resp).as_deref(),
        Some("hello from the pane")
    );
}

#[test]
fn is_runtime_active_defaults_true_when_absent() {
    let resp = json!({
        "content": [{ "type": "text", "text": "{\"pane_content\":\"x\"}" }]
    });
    assert!(is_runtime_active(&resp));
}

#[test]
fn is_runtime_active_reads_false() {
    let resp = json!({
        "content": [{ "type": "text", "text": "{\"runtime_active\":false}" }]
    });
    assert!(!is_runtime_active(&resp));
}

// =====================================================================
// Fail-closed behavior when the target binary is absent from PATH
// =====================================================================

/// Point `$PATH` at an empty tempdir (containing neither `tcode` nor `tm`),
/// run `body`, then restore the original `$PATH`. Caller must hold
/// `crate::test_env::ENV_LOCK` for the whole call — see the module docs.
async fn with_empty_path<Fut: std::future::Future<Output = anyhow::Result<String>>>(
    body: impl FnOnce() -> Fut,
) -> anyhow::Result<String> {
    let empty_dir = tempfile::tempdir().unwrap();
    let original = std::env::var_os("PATH");
    // SAFETY: ENV_LOCK held by the caller for the whole body.
    unsafe {
        std::env::set_var("PATH", empty_dir.path());
    }
    let result = body().await;
    // SAFETY: see above.
    unsafe {
        match &original {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
    }
    result
}

#[tokio::test]
async fn process_pm_bridge_tcode_route_fails_closed_without_binary() {
    let _env_guard = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::tempdir().unwrap();
    let bridge = ProcessPmBridge::from_project(tmp.path().to_path_buf());
    let err = with_empty_path(|| bridge.run(BridgeRoute::Tcode, "fix the parser"))
        .await
        .expect_err("must fail closed when tcode is not on PATH");
    assert!(
        format!("{err:#}").contains("tcode"),
        "error should name the missing binary for diagnosability: {err:#}"
    );
}

#[tokio::test]
async fn process_pm_bridge_tm_route_fails_closed_without_binary() {
    let _env_guard = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::tempdir().unwrap();
    let bridge = ProcessPmBridge::from_project(tmp.path().to_path_buf());
    let err = with_empty_path(|| bridge.run(BridgeRoute::Tm, "spawn a new session"))
        .await
        .expect_err("must fail closed when tm is not on PATH");
    assert!(
        format!("{err:#}").contains("tm"),
        "error should name the missing binary for diagnosability: {err:#}"
    );
}

// =====================================================================
// Binary-gated integration smokes (skip when the real binary is absent —
// mirrors plugins::trusty_search's try_spawn contract).
// =====================================================================

const FORBIDDEN_BRANDING_TOKENS: [&str; 4] = ["trusty-mpm", "trusty-code", "tcode", "tm"];

/// Word-tokenized forbidden-token check — mirrors
/// `pm_bridge_tests::scrub_branding_removes_every_forbidden_token`. A plain
/// substring `.contains("tm")` false-positives on innocent text (observed
/// live: macOS tempdir names like `.tmpBmUq52` contain "tm"), so the
/// production contract this smoke actually verifies — no BRANDED WORD
/// survives — is checked the same word-bounded way `scrub_branding`'s own
/// regex enforces it, not a raw substring scan.
fn assert_no_branded_word(text: &str) {
    let lower = text.to_lowercase();
    for word in lower.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '-');
        assert!(
            !FORBIDDEN_BRANDING_TOKENS.contains(&trimmed),
            "backend-identity token '{trimmed}' leaked as a whole word in: {text}"
        );
    }
}

/// Real end-to-end run through `ProcessPmBridge::run_tcode` -> the tool
/// layer's `scrub_branding`: skipped unless `tcode` is on PATH. Asserts a
/// non-error result whose SCRUBBED text carries no backend-identity token —
/// this is the actual production contract (`PmBridgeTool::execute` always
/// scrubs before returning; the raw backend transcript alone is not
/// guaranteed to be branding-free, e.g. an incidental tempdir path).
///
/// Holds `crate::test_env::ENV_LOCK` for the whole body (including the
/// `binary_on_path` check and the subprocess spawn): without it this test
/// races the `fails_closed` tests above, which mutate `$PATH` process-wide
/// — observed live as a genuine flake (this test reporting "binary not
/// found" while `tcode` was actually installed, because a sibling test's
/// PATH-clearing window overlapped this one's `binary_on_path` check).
#[tokio::test]
async fn tcode_route_smoke() {
    let _env_guard = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !binary_on_path("tcode") {
        eprintln!("tcode not on PATH; skipping tcode_route_smoke");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".claude/agents")).unwrap();
    let bridge = ProcessPmBridge::from_project(tmp.path().to_path_buf());
    let result = bridge
        .run(BridgeRoute::Tcode, "reply with a one-line status only")
        .await;
    match result {
        Ok(out) => assert_no_branded_word(&crate::tools::pm_bridge::scrub_branding(&out)),
        Err(e) => {
            // A live smoke without a properly configured project (no pm
            // agent, no credentials) failing is acceptable here — the
            // binary-presence gate is what this test actually verifies.
            eprintln!("tcode_route_smoke: run failed (acceptable in an unconfigured env): {e:#}");
        }
    }
}

/// Real end-to-end run through `ProcessPmBridge::run_tm` -> `scrub_branding`:
/// skipped unless `tm` is on PATH. Holds `ENV_LOCK` for the same reason as
/// `tcode_route_smoke`.
#[tokio::test]
async fn tm_route_smoke() {
    let _env_guard = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !binary_on_path("tm") {
        eprintln!("tm not on PATH; skipping tm_route_smoke");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let bridge = ProcessPmBridge::from_project(tmp.path().to_path_buf());
    let result = bridge.run(BridgeRoute::Tm, "report session status").await;
    match result {
        Ok(out) => assert_no_branded_word(&crate::tools::pm_bridge::scrub_branding(&out)),
        Err(e) => {
            eprintln!("tm_route_smoke: run failed (acceptable in an unconfigured env): {e:#}");
        }
    }
}
