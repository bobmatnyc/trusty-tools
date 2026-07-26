//! OKG builder tools — grow an assistant's knowledge graph from real sources.
//!
//! Why: the base assistant can already READ a KB tree, but nothing built one
//! except an LLM hand-transcribing emails into markdown (see the batch log in
//! `bob-kb/_state/ingest.md`). That is neither idempotent nor resumable — a
//! second pass re-reads everything and duplicates whatever it re-summarises.
//! These four tools make ingestion mechanical and convergent: the engine in
//! `trusty-kb::okg` owns the ledger and the entity writes, and the assistant
//! stays free to do the part that actually needs a model — distilling those
//! ingested items into people/organizations/events entities.
//!
//! What: `okg_ingest_docstore`, `okg_ingest_gmail`, `okg_ingest_drive` each
//! register-or-update their source, ingest it, and then feed what they wrote
//! into the agent's bound trusty-search index ([`index_feed`], #3892) so the
//! `[[stores]]` binding's "one store, tree and index" semantics are true rather
//! than aspirational; `okg_sources` reports every source with its live coverage,
//! tree AND index. Registration is folded into ingestion
//! on purpose — "point at this directory" and "reach further back in time" are
//! the same call, so widening a window can never desync from the ledger.
//! Gmail/Drive fetching goes through `trusty-gworkspace`'s authenticated client
//! ([`gapi`]); no new OAuth path is introduced.
//!
//! Test: `super::tests` covers the pure adapters, the tool schemas, and a full
//! docstore ingest round-trip against a temp-dir tree.

pub mod config;
pub mod docstore;
pub mod drive;
pub mod gapi;
pub mod gmail;
mod index_feed;
pub mod sources;

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use trusty_kb::roots::{Roots, assert_within};
use trusty_kb::schema::Profile;
use trusty_kb::store::KbStore;

use crate::tools::traits::{ToolExecutor, ToolResult};

pub use docstore::OkgIngestDocstoreTool;
pub use drive::OkgIngestDriveTool;
pub use gmail::OkgIngestGmailTool;
pub use sources::OkgSourcesTool;

/// Construct the four OKG builder tools for registration.
///
/// Why: mirrors `izzie_tools()` / `git_tools()` — one call hands a dispatch path
/// the whole family, so the registration site stays a single loop.
/// What: returns the tools in a stable order. Construction performs no I/O and
/// no network calls, so this is safe at session start; credentials are resolved
/// lazily inside each Google-backed execute.
/// Test: `okg_tools_exposes_expected_names`.
pub fn okg_tools() -> Vec<Arc<dyn ToolExecutor>> {
    vec![
        Arc::new(OkgIngestDocstoreTool::new()),
        Arc::new(OkgIngestGmailTool::new()),
        Arc::new(OkgIngestDriveTool::new()),
        Arc::new(OkgSourcesTool::new()),
    ]
}

/// The shared `agent` / `root` arguments every OKG tool accepts.
pub(super) fn root_props() -> Value {
    json!({
        "agent": {
            "type": "string",
            "description": "Assistant whose KB tree to build; resolves to <knowledge_dir>/<slug(agent)>. Omit to use the default tree."
        },
        "root": {
            "type": "string",
            "description": "Explicit KB tree root. Overrides `agent`. Must live under the knowledge directory."
        }
    })
}

/// Merge the shared root props into a tool-specific property set.
pub(super) fn with_root(mut props: Value) -> Value {
    if let (Value::Object(dst), Value::Object(src)) = (&mut props, root_props()) {
        for (k, v) in src {
            dst.insert(k, v);
        }
    }
    props
}

/// The knowledge directory holding one KB tree per assistant.
///
/// Mirrors `trusty-kb`'s own default so both binaries address the same trees:
/// `$KB_KNOWLEDGE_DIR`, else `<home>/.trusty-agents/knowledge`.
pub(super) fn knowledge_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("KB_KNOWLEDGE_DIR") {
        return PathBuf::from(dir);
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".trusty-agents")
        .join("knowledge")
}

/// Resolve and confine the KB store a tool call operates on.
///
/// Why: `root` is model-supplied input. Without the confinement check an
/// ingest could be pointed at any directory on disk and rewrite it — the same
/// hole `kb_convert_tree` was fixed for in trusty-kb. Resolution precedence
/// matches trusty-kb exactly so a tree addressed here is the same tree the KB
/// MCP server sees.
/// What: explicit `root` → `agent`-mapped tree → service default, then asserts
/// the result stays under the knowledge directory.
/// Test: `resolve_store_rejects_out_of_tree_root`.
pub(super) fn resolve_store(args: &Value) -> anyhow::Result<KbStore> {
    let kdir = knowledge_dir();
    let default_root = std::env::var_os("KB_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| kdir.join("bob-kb"));
    let roots = Roots::new(kdir.clone(), default_root.clone());
    let root = roots.resolve(arg_str(args, "root"), arg_str(args, "agent"));

    if root != default_root {
        assert_within(&kdir, &root).map_err(|_| {
            anyhow::anyhow!(
                "root {} is outside the knowledge directory {}",
                root.display(),
                kdir.display()
            )
        })?;
        // `assert_within` passes vacuously when neither path exists yet, so
        // also require a literal prefix — a not-yet-created sibling of the
        // knowledge dir must not slip through on a first run.
        if !root.starts_with(&kdir) {
            anyhow::bail!(
                "root {} is outside the knowledge directory {}",
                root.display(),
                kdir.display()
            );
        }
    }
    Ok(KbStore::new(root, Profile::default_profile()))
}

/// A non-empty string argument, or `None`.
pub(super) fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

/// A required non-empty string argument.
pub(super) fn require_str<'a>(args: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    arg_str(args, key).ok_or_else(|| anyhow::anyhow!("missing required '{key}'"))
}

/// The doc-store read-confinement policy for this process.
///
/// Why: `okg_ingest_docstore` reads a model-supplied path, so the permitted
/// roots are operator configuration (`[okg] docstore_roots`), never a model
/// argument. Resolved per call so an operator can widen the list without
/// restarting the agent.
/// Test: `config_defaults_to_home`, `docstore_tool_rejects_credential_dir`.
pub(super) fn docstore_policy() -> trusty_kb::okg::policy::DocStorePolicy {
    let home = config::home_dir().unwrap_or_else(|| PathBuf::from("."));
    config::load().policy(&home)
}

/// Current UTC timestamp in the format the KB store stamps entities with.
pub(super) fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Wrap a serialisable outcome as a successful tool result.
pub(super) fn ok_json<T: serde::Serialize>(value: &T) -> ToolResult {
    match serde_json::to_string(value) {
        Ok(text) => ToolResult::ok(text),
        Err(e) => ToolResult::err(format!("failed to serialise result: {e}")),
    }
}

#[cfg(test)]
mod tests;
