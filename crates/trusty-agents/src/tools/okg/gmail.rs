//! `okg_ingest_gmail` — pull a Gmail query window into the OKG, exactly once.
//!
//! Why: this is the source with the sharpest cost asymmetry. Listing message
//! ids is cheap; fetching each full message is not. So the ledger is consulted
//! BEFORE the body fetch, not after — an id already recorded is never fetched
//! again, which is what makes "pull exactly once, ever" literally true rather
//! than merely idempotent at the write layer. It is also what makes Bob's
//! additive time-extension cheap: widening `after:` backwards re-lists the
//! window already covered, but only the genuinely older messages cost a fetch.
//!
//! What: registers (or widens) a `gmail` source, lists ids through the
//! authenticated `trusty-gworkspace` client, filters them against the ledger,
//! fetches only the unseen ones, and hands them to the ingest engine with
//! deletion detection OFF — a windowed pull can never prove a message is gone.
//! Test: `super::tests::gmail_query_*`, `message_to_item_*`.

use async_trait::async_trait;
use serde_json::{Value, json};
use trusty_gworkspace::api::client::BaseClient;
use trusty_kb::okg::ingest::{IngestReport, SourceItem};
use trusty_kb::okg::ledger::Ledger;
use trusty_kb::okg::registry::{Locator, SourceSpec};

use crate::tools::okg::gapi;
use crate::tools::okg::{arg_str, now_iso, ok_json, require_str, resolve_store, with_root};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Default ceiling on messages listed in one call.
const DEFAULT_MAX_MESSAGES: usize = 500;

/// Hard ceiling, enforced regardless of what the caller asks for.
///
/// Why: `max_messages` comes from the model. Without a clamp a single call
/// could be told to pull 10,000,000 messages, and the fetch loop would hold
/// every decoded body in memory at once.
const MAX_MESSAGES_CEILING: usize = 5_000;

/// Messages fetched and committed per chunk.
///
/// Why: bounds peak memory to one chunk of message bodies, and — because the
/// ledger is written inside `ingest_items` — makes each chunk a commit point.
/// A crash during a long backfill loses at most this many fetches, not all of
/// them.
const CHUNK: usize = 100;

/// `okg_ingest_gmail` — ingest a Gmail query window into the OKG.
pub struct OkgIngestGmailTool;

impl OkgIngestGmailTool {
    /// Construct the tool. No credential work happens until `execute`.
    pub fn new() -> Self {
        Self
    }
}

impl Default for OkgIngestGmailTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for OkgIngestGmailTool {
    fn name(&self) -> &str {
        "okg_ingest_gmail"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "okg_ingest_gmail",
                "description": "Ingest Gmail messages matching a query and date window into the assistant's knowledge graph. Each message is pulled exactly once, ever — ids already in the source ledger are never re-fetched. Widening `after` further back in time is additive: only the older messages are fetched. Uses the existing Google Workspace credentials; never prompts for auth.",
                "parameters": {
                    "type": "object",
                    "properties": with_root(json!({
                        "source_id": { "type": "string", "description": "Stable name for this mail source, e.g. 'sent-mail'. Re-using it widens that source; a new one appends." },
                        "query": { "type": "string", "description": "Gmail search DSL WITHOUT date operators, e.g. 'in:sent' or 'from:board@example.com'. Required on first registration." },
                        "after": { "type": "string", "description": "Inclusive lower bound, YYYY/MM/DD. Move this earlier to reach further back in time — already-ingested messages are not re-fetched." },
                        "before": { "type": "string", "description": "Exclusive upper bound, YYYY/MM/DD." },
                        "collection": { "type": "string", "description": "KB collection to write into. Defaults to 'sources'." },
                        "account": { "type": "string", "description": "Google Workspace profile name. Omit for the default account." },
                        "max_messages": { "type": "integer", "description": "Ceiling on messages listed in one call. Default 500." }
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
            Err(e) => ToolResult::err(format!("okg_ingest_gmail failed: {e}")),
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
        Some(Locator::Gmail {
            query,
            after,
            before,
        }) => Some((query.clone(), after.clone(), before.clone())),
        Some(other) => anyhow::bail!(
            "source {source_id} is a {} source, not a Gmail source",
            other.kind()
        ),
        None => None,
    };

    let query = match (arg_str(args, "query"), &prior) {
        (Some(q), _) => q.to_string(),
        (None, Some((q, _, _))) => q.clone(),
        (None, None) => {
            anyhow::bail!("'query' is required when registering the new source {source_id:?}")
        }
    };
    // An omitted bound keeps the registered one; an explicitly widened `after`
    // replaces it. Either way the ledger — not the window — decides what gets
    // fetched, so narrowing can never drop already-ingested messages.
    let after = arg_str(args, "after")
        .map(str::to_string)
        .or_else(|| prior.as_ref().and_then(|(_, a, _)| a.clone()));
    let before = arg_str(args, "before")
        .map(str::to_string)
        .or_else(|| prior.as_ref().and_then(|(_, _, b)| b.clone()));

    let spec = SourceSpec::new(
        source_id,
        arg_str(args, "collection").or_else(|| existing.as_ref().map(|s| s.collection.as_str())),
        Locator::Gmail {
            query: query.clone(),
            after: after.clone(),
            before: before.clone(),
        },
        &now,
    );
    let registered = store.okg_register_source(spec)?;
    let spec = store.okg_source(&registered.id)?;

    let account = arg_str(args, "account");
    // Clamp, never trust: a model-supplied ceiling only ever narrows the pull.
    let max_messages = args
        .get("max_messages")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_MAX_MESSAGES)
        .clamp(1, MAX_MESSAGES_CEILING);

    let client = BaseClient::new()?;
    // Fail loudly on a broken credential instead of letting the client fall
    // through to a stale token and report an empty, "successful" pull.
    client
        .get_access_token(account)
        .await
        .map_err(|e| anyhow::anyhow!("Google Workspace credentials unavailable: {e}"))?;

    let effective_query = gapi::gmail_query(&query, after.as_deref(), before.as_deref());
    let ids = gapi::gmail_list_ids(&client, account, &effective_query, max_messages).await?;

    // The exactly-once gate: skip the expensive body fetch for anything the
    // ledger has already recorded, at any version.
    let ledger = Ledger::load(&store.root, &spec.id)?;
    let unseen: Vec<String> = ids
        .iter()
        .filter(|id| !ledger.contains(id))
        .cloned()
        .collect();
    drop(ledger);

    // Fetch and COMMIT in chunks. Accumulating every body before a single
    // ingest would hold the whole backfill in memory and lose all of it to a
    // crash mid-fetch; committing per chunk bounds both.
    let mut report = IngestReport::default();
    let mut fetched = 0usize;
    for batch in unseen.chunks(CHUNK) {
        let mut items: Vec<SourceItem> = Vec::with_capacity(batch.len());
        for id in batch {
            match gapi::gmail_message(&client, account, id).await {
                Ok(msg) => match gapi::message_to_item(&msg) {
                    Some(item) => items.push(item),
                    None => report
                        .errors
                        .push(format!("{id}: message payload had no usable id")),
                },
                Err(e) => report.errors.push(format!("{id}: {e}")),
            }
        }
        fetched += batch.len();
        // detect_deletions = false: a windowed pull sees only its window, so an
        // absent message means "outside the window", never "deleted". There is
        // deliberately no deletion sweep for Gmail at all.
        let chunk = store.ingest_items(&spec, &items, false, &now)?;
        report.merge(chunk);
    }

    // `missing` here would list every message outside this window — noise, not
    // signal, for a source that never enumerates its full corpus.
    report.missing.clear();
    report.source_id = spec.id.clone();
    report.kind = spec.kind().to_string();
    report.collection = spec.collection.clone();
    // Re-frame the counts around what was LISTED, folding in the ids the ledger
    // let us skip before ever paying for a fetch.
    report.skipped += ids.len() - unseen.len();
    report.scanned = ids.len();

    Ok(ok_json(&json!({
        "source": registered,
        "query": effective_query,
        "listed": ids.len(),
        "fetched": fetched,
        "max_messages": max_messages,
        "ingest": report,
    })))
}
