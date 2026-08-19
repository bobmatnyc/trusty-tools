//! Committed snapshot of the `rpc.discover` document (#5331).
//!
//! Why: this crate's OpenRPC document is what an MCP client discovers — 62
//! tools and the OAuth scope set each one needs. Nothing downstream fails to
//! BUILD when a field changes shape; the symptom is tools going missing at
//! dispatch. #3577 already shipped that failure once (a wire-format mismatch
//! deserialized to zero tools instead of erroring, because the mismatched
//! fields all carry `#[serde(default)]`). A byte-level snapshot turns any
//! change to the emitted document into a reviewable diff instead of a silent
//! behavior change.
//!
//! Two pending changes make this snapshot load-bearing right now: #5331
//! proposes routing this document through `trusty_mcp::openrpc`'s
//! shared builder, and the crate is scheduled to move into a new
//! `trusty-mcp-services` crate. Neither may alter what a client discovers.
//! Re-run this test after either one; the golden is deliberately independent
//! of the crate's version and of its location in the workspace, so a
//! relocated crate can be checked against the same file.
//!
//! What: `openrpc-discover.golden.json`, compared byte-for-byte against
//! `discover_response()` with `info.version` normalized to a placeholder so a
//! version bump does not churn the snapshot. Two structural assertions guard
//! the specific fields a migration to the shared builder would drop — see
//! their doc comments for why each one has teeth.
//!
//! Test: regenerate with
//! `UPDATE_GOLDEN=1 cargo test -p trusty-gworkspace --test openrpc_golden`.
//! Review the resulting `git diff` before committing it; that diff IS the
//! deliverable.

use serde_json::Value;
use std::path::{Path, PathBuf};

/// Placeholder substituted for `info.version`.
///
/// The document embeds `env!("CARGO_PKG_VERSION")`, which changes on every
/// release and would otherwise make this snapshot fail for a reason that has
/// nothing to do with the tool surface.
const VERSION_PLACEHOLDER: &str = "<CARGO_PKG_VERSION>";

const REGEN: &str = "UPDATE_GOLDEN=1 cargo test -p trusty-gworkspace --test openrpc_golden";

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("testdata")
        .join("openrpc-discover.golden.json")
}

/// The emitted document with `info.version` normalized.
fn normalized_document() -> Value {
    let mut doc = trusty_gworkspace::openrpc::discover_response();
    doc["info"]["version"] = Value::String(VERSION_PLACEHOLDER.to_string());
    doc
}

#[test]
fn discover_document_matches_golden() {
    let actual = format!(
        "{}\n",
        serde_json::to_string_pretty(&normalized_document()).expect("serialize document")
    );
    let path = golden_path();

    if std::env::var("UPDATE_GOLDEN").is_ok_and(|v| !v.is_empty() && v != "0") {
        std::fs::write(&path, &actual).expect("write golden");
        return;
    }

    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}\nRegenerate with: {REGEN}", path.display()));

    assert_eq!(
        expected, actual,
        "the rpc.discover document changed.\n\
         If the change is intended, regenerate with: {REGEN}\n\
         and review the diff — it is what every MCP client will discover."
    );
}

/// Every method must carry `x-google-scopes`, not the generic `x-scopes`.
///
/// This is not a stylistic preference. `trusty-agents`'s discovery parser
/// (`tools/registry/discovery.rs`) checks `x-scopes` FIRST and uses its
/// entries verbatim as an already-dotted scope string; it only maps OAuth
/// URLs through `google_scope::dotted_scope_for_google_scopes` when it falls
/// through to `x-google-scopes`. Emitting this crate's OAuth URLs under
/// `x-scopes` therefore resolves 61 of 62 tools to a raw
/// `https://www.googleapis.com/auth/...` string, which matches no
/// `ScopePattern` and silently scope-denies them at dispatch — the #3987 /
/// #3938 failure mode. The shared `OpenRpcBuilder` hardcodes `x-scopes`, so
/// this assertion is what stops #5331's migration from landing that
/// regression unnoticed.
#[test]
fn every_method_uses_the_google_scope_extension() {
    let doc = normalized_document();
    let methods = doc["methods"].as_array().expect("methods array");
    assert!(!methods.is_empty(), "methods must not be empty");

    for m in methods {
        let name = m["name"].as_str().unwrap_or("<unnamed>");
        assert!(
            m.get("x-google-scopes").is_some_and(Value::is_array),
            "method {name} must carry an x-google-scopes array",
        );
        assert!(
            m.get("x-scopes").is_none(),
            "method {name} carries x-scopes; trusty-agents would read these OAuth \
             URLs as dotted scopes and scope-deny the tool",
        );
    }
}

/// `info.license` must survive.
///
/// `OpenRpcBuilder` has no slot for it, so a migration to the shared builder
/// drops it silently — nothing fails to compile and no other test notices.
#[test]
fn info_carries_license_and_description() {
    let doc = normalized_document();
    assert_eq!(doc["info"]["license"]["name"], "Elastic-2.0");
    assert!(
        doc["info"]["license"]["url"].is_string(),
        "info.license.url must be present",
    );
    assert!(
        doc["info"]["description"]
            .as_str()
            .is_some_and(|d| !d.is_empty()),
        "info.description must be present and non-empty",
    );
}
