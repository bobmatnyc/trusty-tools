//! `okg_ingest_docstore` — point the KB at a directory of documents.
//!
//! Why: the simplest additive source. "Here is another folder of notes" must
//! append to the store without touching what any other source built, and
//! re-running over an unchanged folder must write nothing at all.
//! What: registers (or updates) a `doc_store` source under `source_id`, then
//! runs the in-crate scanner + ingest engine. Change detection is a SHA-256 of
//! each file's bytes, so an edit re-ingests and a moved-but-identical file is
//! still a new item (its path is its identity). Deletions are surfaced, and
//! optionally tombstoned, never silently dropped.
//! Test: `super::tests::docstore_tool_ingests_and_is_idempotent`.

use async_trait::async_trait;
use serde_json::{Value, json};
use trusty_kb::okg::registry::{Locator, SourceSpec};

use crate::tools::okg::{arg_str, now_iso, ok_json, require_str, resolve_store, with_root};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// `okg_ingest_docstore` — ingest a directory of text documents into the OKG.
pub struct OkgIngestDocstoreTool;

impl OkgIngestDocstoreTool {
    /// Construct the tool. Performs no I/O.
    pub fn new() -> Self {
        Self
    }
}

impl Default for OkgIngestDocstoreTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for OkgIngestDocstoreTool {
    fn name(&self) -> &str {
        "okg_ingest_docstore"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "okg_ingest_docstore",
                "description": "Ingest a directory of text documents (md/txt/rst/csv/json/…) into the assistant's knowledge graph. Idempotent: unchanged files are skipped by content hash, edited files replace their prior entry, and deleted files are reported (or tombstoned). Additive: registering a new source_id never disturbs sources already ingested. Binary files are skipped and reported. Readable directories are limited to an operator-configured allow-list ([okg] docstore_roots, default $HOME); hidden paths such as ~/.ssh and ~/.aws are always refused.",
                "parameters": {
                    "type": "object",
                    "properties": with_root(json!({
                        "source_id": { "type": "string", "description": "Stable name for this doc store, e.g. 'ssd1-personal'. Re-using it updates that source in place; a new one appends." },
                        "path": { "type": "string", "description": "Directory to ingest. Required on first registration; omit to re-run an already-registered source." },
                        "collection": { "type": "string", "description": "KB collection to write into. Defaults to 'sources'." },
                        "extensions": { "type": "array", "items": { "type": "string" }, "description": "Extensions to accept (without dots). Omit for the built-in text set." },
                        "recursive": { "type": "boolean", "description": "Walk subdirectories. Default true." },
                        "tombstone_deleted": { "type": "boolean", "description": "Mark entities whose file disappeared with source_status=deleted (content preserved). Default false — deletions are reported instead." }
                    })),
                    "required": ["source_id"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        match run(&args) {
            Ok(result) => result,
            Err(e) => ToolResult::err(format!("okg_ingest_docstore failed: {e}")),
        }
    }
}

/// The fallible body — any `Err` becomes an error result.
fn run(args: &Value) -> anyhow::Result<ToolResult> {
    let store = resolve_store(args)?;
    let source_id = require_str(args, "source_id")?;
    let now = now_iso();

    // A path is required to register a NEW source, but omitting it on a re-run
    // is the normal "ingest whatever changed" call — so fall back to the
    // already-registered locator rather than erroring.
    let existing = store.okg_source(source_id).ok();
    let path = match (arg_str(args, "path"), &existing) {
        (Some(p), _) => p.to_string(),
        (None, Some(spec)) => match &spec.locator {
            Locator::DocStore { path, .. } => path.clone(),
            other => anyhow::bail!(
                "source {source_id} is a {} source, not a doc store",
                other.kind()
            ),
        },
        (None, None) => {
            anyhow::bail!("'path' is required when registering the new source {source_id:?}")
        }
    };
    // Confine the READ side before anything is registered, so a rejected path
    // never even reaches `registry.toml`. The engine re-checks this same policy
    // at scan time on every later run, so this is a fast, clear failure rather
    // than the only line of defence.
    let policy = crate::tools::okg::docstore_policy();
    let path = policy
        .permit(std::path::Path::new(&path))?
        .to_string_lossy()
        .to_string();

    let extensions: Vec<String> = args
        .get("extensions")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim_start_matches('.').to_lowercase())
                .collect()
        })
        .unwrap_or_default();
    // Unspecified booleans inherit the registered source rather than silently
    // reverting to a default — re-running must not change a source's behaviour.
    let recursive = args
        .get("recursive")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| match existing.as_ref().map(|s| &s.locator) {
            Some(Locator::DocStore { recursive, .. }) => *recursive,
            _ => true,
        });
    let tombstone = args
        .get("tombstone_deleted")
        .and_then(Value::as_bool)
        .or_else(|| existing.as_ref().map(|s| s.tombstone_deleted))
        .unwrap_or(false);

    let mut spec = SourceSpec::new(
        source_id,
        arg_str(args, "collection").or_else(|| existing.as_ref().map(|s| s.collection.as_str())),
        Locator::DocStore {
            path,
            extensions,
            recursive,
        },
        &now,
    );
    spec.tombstone_deleted = tombstone;
    let registered = store.okg_register_source(spec)?;
    let report = store.okg_ingest_docstore(&registered.id, &policy, &now)?;

    Ok(ok_json(&json!({
        "source": registered,
        "ingest": report,
    })))
}
