//! Auto-seed the R3 memory prompt-fact identity pointer at workspace provision
//! (DOC-28 §5 / §7 Phase 2, epic #1855).
//!
//! Why: `get_prompt_context()` (`crates/trusty-memory/src/prompt_facts.rs`)
//! surfaces `is_fact` triples from every registered trusty-memory palace, but
//! the identity fact disambiguating `trusty-mpm` (binary `tm`, the Rust
//! Meta-Harness) from the unrelated Python `claude-mpm` project only appears
//! once someone runs the manual `kg_assert` step documented in
//! `crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md`'s "Memory seeding" section.
//! DOC-28 §10 asks whether that seed should run automatically; this module
//! implements the chosen policy: first-run/provision only, idempotency-guarded,
//! fail-open. It fires from [`crate::provisioner::workspace::WorkspaceProvisioner::provision_in`]
//! — the one place a brand-new managed-session workspace is created — rather
//! than from the general `prepare_session` path that also runs on every
//! resume/connect, so the seed attempt happens once per provisioned workspace
//! instead of on every launch.
//! What: [`seed_identity_prompt_fact_blocking`] is the synchronous entry point
//! [`WorkspaceProvisioner::provision_in`] calls. It spawns a dedicated OS
//! thread running a fresh `current_thread` Tokio runtime (mirroring
//! `trusty_common::catchup::run_catchup_blocking`) so it never conflicts with
//! whatever async runtime the caller may already be inside, then runs
//! [`seed_identity_prompt_fact`]: a `kg_query(subject: "trusty-mpm")` call
//! against the session's palace as an idempotency guard, followed by a
//! `kg_assert` only when no `is_fact` triple already exists.
//! Test: `identity_fact_present_detects_existing_fact`,
//! `identity_fact_present_false_when_absent`, `seed_is_fail_open_on_unreachable_daemon`.

use std::time::Duration;

use serde::Deserialize;

/// Subject the identity prompt-fact is asserted against.
///
/// Why: must match the literal subject `get_prompt_context()`'s
/// `gather_hot_triples` surfaces and the one documented in
/// `WHAT-IS-TRUSTY-MPM.md`'s manual seeding step, so the automatic and manual
/// seed paths are interchangeable/idempotent with each other.
const IDENTITY_SUBJECT: &str = "trusty-mpm";

/// Predicate used for the identity prompt-fact.
///
/// Why: `is_fact` is already a member of trusty-memory's `HOT_PREDICATES`
/// (`prompt_facts.rs`), so no trusty-memory code change is required for the
/// seeded triple to surface in `get_prompt_context()`.
const IDENTITY_PREDICATE: &str = "is_fact";

/// Object text for the seeded identity prompt-fact (DOC-28 §5 acceptance
/// criteria: must contain the R1 doc's canonical deployed path and the
/// disambiguation clause).
const IDENTITY_OBJECT: &str = "trusty-mpm (binary tm) is the Rust Meta-Harness / control plane, NOT the Python claude-mpm project; see crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md or ~/.trusty-mpm/framework/docs/WHAT-IS-TRUSTY-MPM.md";

/// Provenance tag for the seeded triple, matching the manual step documented
/// in `WHAT-IS-TRUSTY-MPM.md`.
const IDENTITY_PROVENANCE: &str = "DOC-28 self-awareness seed";

/// Minimal shape of a triple as returned by `GET /api/v1/palaces/{id}/kg`.
///
/// Why: mirrors the fail-open, minimal-deserialization pattern already used by
/// `trusty_common::catchup::palace::RawDrawer` — only the two fields the
/// idempotency check needs are read; unknown fields are ignored.
/// What: `subject`/`predicate` pair; missing fields default to empty string so
/// a malformed/partial record never panics the parse.
/// Test: `identity_fact_present_detects_existing_fact`.
#[derive(Debug, Deserialize)]
struct RawTriple {
    #[serde(default)]
    subject: String,
    #[serde(default)]
    predicate: String,
}

/// True when `triples` already contains the seeded identity `is_fact` triple.
///
/// Why: isolates the pure idempotency decision from the HTTP layer so it is
/// unit-testable without a live trusty-memory daemon.
/// What: returns true iff any triple has `subject == "trusty-mpm"` and
/// `predicate == "is_fact"`.
/// Test: `identity_fact_present_detects_existing_fact`,
/// `identity_fact_present_false_when_absent`.
fn identity_fact_present(triples: &[RawTriple]) -> bool {
    triples
        .iter()
        .any(|t| t.subject == IDENTITY_SUBJECT && t.predicate == IDENTITY_PREDICATE)
}

/// Seed the R3 identity prompt-fact into `palace_id`, fail-open.
///
/// Why: the async body of the auto-seed step; kept separate from the blocking
/// wrapper so the HTTP round-trips can be tested directly with `#[tokio::test]`.
/// What: builds a short-timeout `reqwest::Client`, issues
/// `GET {memory_url}/api/v1/palaces/{palace_id}/kg?subject=trusty-mpm` as the
/// idempotency guard, and — only when [`identity_fact_present`] returns false —
/// issues `POST {memory_url}/api/v1/palaces/{palace_id}/kg` with the identity
/// triple. Any client-build failure, connection error, non-2xx response, or
/// JSON-parse failure at any step is logged to stderr via `tracing::warn!` and
/// swallowed: this function never returns an error and never panics, so it can
/// never block or fail workspace provisioning.
/// Test: `seed_is_fail_open_on_unreachable_daemon`.
async fn seed_identity_prompt_fact(memory_url: &str, palace_id: &str) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("identity-seed: could not build HTTP client: {e}");
            return;
        }
    };

    let base = memory_url.trim_end_matches('/');
    let query_url = format!("{base}/api/v1/palaces/{palace_id}/kg");
    let resp = match client
        .get(&query_url)
        .query(&[("subject", IDENTITY_SUBJECT)])
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("identity-seed: could not reach trusty-memory at {memory_url}: {e}");
            return;
        }
    };
    if !resp.status().is_success() {
        tracing::warn!(
            "identity-seed: trusty-memory returned {} for identity kg_query",
            resp.status()
        );
        return;
    }
    let triples: Vec<RawTriple> = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("identity-seed: could not parse kg_query response: {e}");
            return;
        }
    };

    if identity_fact_present(&triples) {
        tracing::debug!("identity-seed: identity prompt-fact already present, skipping assert");
        return;
    }

    let assert_url = format!("{base}/api/v1/palaces/{palace_id}/kg");
    let body = serde_json::json!({
        "subject": IDENTITY_SUBJECT,
        "predicate": IDENTITY_PREDICATE,
        "object": IDENTITY_OBJECT,
        "provenance": IDENTITY_PROVENANCE,
    });
    match client.post(&assert_url).json(&body).send().await {
        Ok(r) if r.status().is_success() => {
            tracing::info!(
                "identity-seed: seeded trusty-mpm identity prompt-fact into palace {palace_id}"
            );
        }
        Ok(r) => {
            tracing::warn!(
                "identity-seed: trusty-memory returned {} for identity kg_assert",
                r.status()
            );
        }
        Err(e) => {
            tracing::warn!("identity-seed: could not reach trusty-memory at {memory_url}: {e}");
        }
    }
}

/// Synchronous, fail-open entry point for [`super::workspace::WorkspaceProvisioner::provision_in`].
///
/// Why: `provision_in` is a synchronous function that may itself be called
/// from inside an async handler (`daemon/managed_routes/lifecycle.rs`), so this
/// cannot simply call `tokio::runtime::Handle::block_on` (that would panic
/// with "Cannot start a runtime from within a runtime" when already inside
/// one). Spawning a dedicated OS thread with its own `current_thread` runtime
/// — the same technique `trusty_common::catchup::run_catchup_blocking` uses —
/// sidesteps that entirely.
/// What: spawns a thread, builds a `current_thread` Tokio runtime on it, and
/// blocks on [`seed_identity_prompt_fact`]. A runtime-build failure or a
/// panicked/join-failed thread is logged and swallowed — never propagated to
/// the caller.
/// Test: `seed_is_fail_open_on_unreachable_daemon` (exercises the async body
/// directly under `#[tokio::test]`); the thread-spawn wrapper itself has no
/// independent behaviour to assert beyond "does not panic", which every test
/// in this module already exercises by construction.
pub fn seed_identity_prompt_fact_blocking(memory_url: &str, palace_id: &str) {
    let memory_url = memory_url.to_string();
    let palace_id = palace_id.to_string();
    let joined = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        match rt {
            Ok(rt) => rt.block_on(seed_identity_prompt_fact(&memory_url, &palace_id)),
            Err(e) => {
                tracing::warn!("identity-seed: could not build tokio runtime: {e}");
            }
        }
    })
    .join();
    if joined.is_err() {
        tracing::warn!("identity-seed: seed thread panicked; skipping");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_triple(subject: &str, predicate: &str) -> RawTriple {
        RawTriple {
            subject: subject.to_string(),
            predicate: predicate.to_string(),
        }
    }

    #[test]
    fn identity_fact_present_detects_existing_fact() {
        let triples = vec![
            make_triple("some-other-subject", "is_fact"),
            make_triple(IDENTITY_SUBJECT, IDENTITY_PREDICATE),
        ];
        assert!(identity_fact_present(&triples));
    }

    #[test]
    fn identity_fact_present_false_when_absent() {
        let triples = vec![
            make_triple(IDENTITY_SUBJECT, "some_other_predicate"),
            make_triple("unrelated", IDENTITY_PREDICATE),
        ];
        assert!(!identity_fact_present(&triples));
    }

    #[test]
    fn identity_fact_present_false_for_empty_response() {
        let triples: Vec<RawTriple> = vec![];
        assert!(!identity_fact_present(&triples));
    }

    /// Idempotency guard, exercised without a live daemon: a kg_query response
    /// that already contains the identity fact must short-circuit before the
    /// (unreachable) assert step would ever be attempted, mirroring what
    /// `seed_identity_prompt_fact` does after parsing a successful kg_query.
    #[test]
    fn identity_fact_present_skips_when_already_seeded() {
        let already_seeded = vec![make_triple(IDENTITY_SUBJECT, IDENTITY_PREDICATE)];
        assert!(
            identity_fact_present(&already_seeded),
            "a kg_query response containing the identity triple must be treated as already seeded"
        );
    }

    /// Fail-open contract: an unreachable trusty-memory daemon must not panic
    /// or hang the caller — the async body just logs and returns.
    #[tokio::test]
    async fn seed_is_fail_open_on_unreachable_daemon() {
        // Port 1 is a reserved/unassigned port that nothing listens on, so the
        // connection fails fast without needing a real daemon or a mock server.
        seed_identity_prompt_fact("http://127.0.0.1:1", "test-palace").await;
        // Reaching this line without panicking/hanging proves fail-open.
    }

    /// Fail-open contract for the synchronous wrapper used by production code:
    /// must return promptly without panicking even when the daemon is unreachable.
    #[test]
    fn seed_blocking_is_fail_open_on_unreachable_daemon() {
        seed_identity_prompt_fact_blocking("http://127.0.0.1:1", "test-palace");
    }
}
