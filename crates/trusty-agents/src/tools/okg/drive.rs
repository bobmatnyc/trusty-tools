//! `okg_ingest_drive` — pull a Drive folder into the OKG, revision-aware.
//!
//! Why: Drive differs from Gmail in exactly one way that matters, and it drives
//! the whole design here: a Drive file's CONTENT changes in place under a stable
//! id. So presence in the ledger is not enough to skip — the fingerprint tracks
//! the file's `version` (which Drive bumps on every content edit), falling back
//! to `modifiedTime`. An unchanged file therefore costs one cheap metadata row
//! and no download; a new revision re-ingests and replaces its entity.
//!
//! What: registers (or updates) a `drive` source, lists the folder through the
//! authenticated `trusty-gworkspace` client with a field set that actually
//! includes `version` and `nextPageToken` (the crate's own wrapper requests
//! neither), skips files whose revision the ledger already holds, downloads or
//! exports the rest as text, and ingests. Deletion detection is ON only for a
//! full recursive listing, which genuinely enumerates the folder's corpus.
//! Test: `super::tests::drive_item_fingerprints_on_version`,
//! `drive_item_falls_back_to_mtime`.

use std::collections::BTreeSet;

use async_trait::async_trait;
use serde_json::{Value, json};
use trusty_gworkspace::api::client::BaseClient;
use trusty_kb::okg::ingest::{IngestReport, SourceItem};
use trusty_kb::okg::ledger::Ledger;
use trusty_kb::okg::registry::{Locator, SourceSpec};

use crate::tools::okg::gapi;
use crate::tools::okg::{arg_str, now_iso, ok_json, require_str, resolve_store, with_root};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Default ceiling on files listed in one call.
const DEFAULT_MAX_FILES: usize = 500;

/// Hard ceiling, enforced regardless of what the caller asks for.
///
/// Why: `max_files` comes from the model; without a clamp a single call could
/// be told to pull an unbounded folder and hold every downloaded body at once.
const MAX_FILES_CEILING: usize = 5_000;

/// Files downloaded and committed per chunk — bounds peak memory and makes each
/// chunk a ledger commit point, so a crash mid-backfill keeps prior progress.
const CHUNK: usize = 50;

/// `okg_ingest_drive` — ingest a Google Drive folder into the OKG.
pub struct OkgIngestDriveTool;

impl OkgIngestDriveTool {
    /// Construct the tool. No credential work happens until `execute`.
    pub fn new() -> Self {
        Self
    }
}

impl Default for OkgIngestDriveTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for OkgIngestDriveTool {
    fn name(&self) -> &str {
        "okg_ingest_drive"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "okg_ingest_drive",
                "description": "Ingest a Google Drive folder into the assistant's knowledge graph. Idempotent and revision-aware: unchanged files are skipped without downloading, files with a new revision re-ingest and replace their prior entry. Google Docs/Sheets/Slides are exported as text; binary files are skipped and reported. Uses the existing Google Workspace credentials; never prompts for auth.",
                "parameters": {
                    "type": "object",
                    "properties": with_root(json!({
                        "source_id": { "type": "string", "description": "Stable name for this Drive source, e.g. 'board-docs'. Re-using it updates that source; a new one appends." },
                        "folder_id": { "type": "string", "description": "Drive folder id ('root' for My Drive). Required on first registration." },
                        "recursive": { "type": "boolean", "description": "Descend into subfolders. Default false." },
                        "collection": { "type": "string", "description": "KB collection to write into. Defaults to 'sources'." },
                        "account": { "type": "string", "description": "Google Workspace profile name. Omit for the default account." },
                        "tombstone_deleted": { "type": "boolean", "description": "Mark entities whose Drive file disappeared with source_status=deleted (content preserved). Default false — deletions are reported instead." },
                        "max_files": { "type": "integer", "description": "Ceiling on files listed in one call. Default 500." }
                    })),
                    "required": ["source_id"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        match run(&args).await {
            Ok(result) => result,
            Err(e) => ToolResult::err(format!("okg_ingest_drive failed: {e}")),
        }
    }
}

/// The fallible body — any `Err` becomes an error result.
async fn run(args: &Value) -> anyhow::Result<ToolResult> {
    let store = resolve_store(args)?;
    let source_id = require_str(args, "source_id")?;
    let now = now_iso();
    let existing = store.okg_source(source_id).ok();
    let prior = match existing.as_ref().map(|s| &s.locator) {
        Some(Locator::Drive {
            folder_id,
            recursive,
        }) => Some((folder_id.clone(), *recursive)),
        Some(other) => anyhow::bail!(
            "source {source_id} is a {} source, not a Drive source",
            other.kind()
        ),
        None => None,
    };

    let folder_id = match (arg_str(args, "folder_id"), &prior) {
        (Some(f), _) => f.to_string(),
        (None, Some((f, _))) => f.clone(),
        (None, None) => {
            anyhow::bail!("'folder_id' is required when registering the new source {source_id:?}")
        }
    };
    let recursive = args
        .get("recursive")
        .and_then(Value::as_bool)
        .or_else(|| prior.as_ref().map(|(_, r)| *r))
        .unwrap_or(false);
    let tombstone = args
        .get("tombstone_deleted")
        .and_then(Value::as_bool)
        .or_else(|| existing.as_ref().map(|s| s.tombstone_deleted))
        .unwrap_or(false);

    let mut spec = SourceSpec::new(
        source_id,
        arg_str(args, "collection").or_else(|| existing.as_ref().map(|s| s.collection.as_str())),
        Locator::Drive {
            folder_id: folder_id.clone(),
            recursive,
        },
        &now,
    );
    spec.tombstone_deleted = tombstone;
    let registered = store.okg_register_source(spec)?;
    let spec = store.okg_source(&registered.id)?;

    let account = arg_str(args, "account");
    // Clamp, never trust: a model-supplied ceiling only ever narrows the pull.
    let max_files = args
        .get("max_files")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_MAX_FILES)
        .clamp(1, MAX_FILES_CEILING);

    let client = BaseClient::new()?;
    client
        .get_access_token(account)
        .await
        .map_err(|e| anyhow::anyhow!("Google Workspace credentials unavailable: {e}"))?;

    let files = gapi::drive_list_files(&client, account, &folder_id, recursive, max_files).await?;

    // Metadata carries the revision, so the skip decision is made before any
    // download — an unchanged folder costs one list call and nothing else.
    let ledger = Ledger::load(&store.root, &spec.id)?;
    let mut needs_fetch: Vec<Value> = Vec::new();
    let mut present_ids: BTreeSet<String> = BTreeSet::new();
    let mut skipped_unchanged = 0usize;
    for file in &files {
        let Some(probe) = gapi::drive_item(file, String::new()) else {
            continue;
        };
        present_ids.insert(probe.item_id.clone());
        // `volatile` files have no usable revision signal, so they must always
        // be re-fetched — consulting the ledger for them would freeze them.
        if !probe.volatile && ledger.is_current(&probe.item_id, &probe.fingerprint) {
            skipped_unchanged += 1;
        } else {
            needs_fetch.push(file.clone());
        }
    }
    drop(ledger);

    // Download and COMMIT in chunks so peak memory is one chunk of file bodies
    // and a crash mid-backfill keeps everything already committed.
    let mut report = IngestReport::default();
    let mut skipped_binary = 0usize;
    for batch in needs_fetch.chunks(CHUNK) {
        let mut items: Vec<SourceItem> = Vec::with_capacity(batch.len());
        for file in batch {
            let id = file.get("id").and_then(Value::as_str).unwrap_or_default();
            match gapi::drive_content(&client, account, id).await {
                Ok(Some(content)) => match gapi::drive_item(file, content) {
                    Some(item) => items.push(item),
                    None => report
                        .errors
                        .push(format!("{id}: file metadata had no usable id")),
                },
                Ok(None) => {
                    skipped_binary += 1;
                    report
                        .errors
                        .push(format!("{id}: skipped (binary or non-exportable content)"));
                }
                Err(e) => report.errors.push(format!("{id}: {e}")),
            }
        }
        // Detection is OFF per chunk — a single page cannot tell which files
        // exist folder-wide, and treating it as authoritative would tombstone
        // every file in the other chunks. The sweep runs once, below.
        report.merge(store.ingest_items(&spec, &items, false, &now)?);
    }

    // A recursive listing that did not hit the cap enumerates the whole folder
    // tree, so an absent file is genuinely absent. A shallow or truncated
    // listing does not — tombstoning from one would be a lie.
    let report_deletions = recursive && files.len() < max_files;
    if report_deletions {
        report.merge(store.okg_sweep_deleted(&spec, &present_ids, &now)?);
    }

    report.source_id = spec.id.clone();
    report.kind = spec.kind().to_string();
    report.collection = spec.collection.clone();
    report.skipped += skipped_unchanged;
    report.scanned = files.len();

    Ok(ok_json(&json!({
        "source": registered,
        "folder_id": folder_id,
        "listed": files.len(),
        "downloaded": needs_fetch.len(),
        "skipped_binary": skipped_binary,
        "deletion_detection": report_deletions,
        "max_files": max_files,
        "ingest": report,
    })))
}
