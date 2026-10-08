//! Handler for `trusty-search convert` — migrate mcp-vector-search projects.
//!
//! Why: the convert flow has two distinct sub-cases (single project / all
//! projects) plus dry-run handling and bounded-concurrency fan-out, plus the
//! mcp-vector-search config discovery + parsing helpers. Keeping it all in one
//! module co-locates the (de)serialization, the per-project register-and-
//! reindex calls, and the render layer.
//! What: `handle_convert` is the entry point; everything else is private. The
//! daemon is reached over its socket only (#9214).
//! Test: `convert_one_registers_then_reindexes_over_the_socket`,
//! `convert_one_stops_at_a_refused_create`,
//! `convert_one_contacts_nothing_on_a_dry_run`.

use anyhow::Result;
use clap::ValueEnum;
use colored::Colorize;
use serde_json::json;
use trusty_search::service::daemon_client::{DaemonCallError, DaemonClient};
use trusty_search::service::rpc::writes::{METHOD_INDEX_CREATE, METHOD_INDEX_REINDEX};

/// Why: `convert` accepts a discrete operating mode, so model it as an enum
/// rather than a free-form string. Validated at parse time by clap.
/// What: `Project` operates on the CWD; `All` walks the user's home tree
/// looking for `.mcp-vector-search/config.json` files.
/// Test: `cargo run -- convert bogus` → clap rejects with usage hint.
#[derive(Debug, Clone, ValueEnum)]
pub enum ConvertTarget {
    /// Convert the project in the current directory (or any parent)
    Project,
    /// Convert every mcp-vector-search project on this machine
    All,
}

/// Subset of mcp-vector-search's `config.json` we care about.
///
/// Why: only `project_root` is needed to derive an index name and reindex
/// path. Every other field (file_extensions, embedding_model, ...) is
/// re-derived from the project tree at index time.
/// What: serde-deserialized from the JSON config.
/// Test: parse a config containing extra unknown fields → succeeds.
#[derive(Debug, serde::Deserialize)]
struct MvsConfig {
    project_root: std::path::PathBuf,
}

/// Walk up from `start` looking for `.mcp-vector-search/config.json`.
fn find_mvs_config(start: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join(".mcp-vector-search").join("config.json");
        if candidate.exists() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Find every `*/.mcp-vector-search/config.json` under the user's home dir.
///
/// Why: shared by both `convert all` and `migrate mcp-vector-search` so the
/// discovery logic (home walk + noise-dir skipping) lives in exactly one
/// place.
/// What: walks `$HOME` (max depth 6) and returns every config path.
/// Test: covered indirectly by `convert all --dry-run` enumerating projects.
pub(crate) fn find_all_mvs_configs() -> Vec<std::path::PathBuf> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Vec::new(),
    };
    let mut configs = Vec::new();
    for entry in walkdir::WalkDir::new(&home)
        .max_depth(6)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Skip obvious noise that can't contain user projects but bloats
            // the walk: hidden caches, language toolchains, OS junk.
            let name = e.file_name().to_string_lossy();
            !matches!(
                name.as_ref(),
                "node_modules"
                    | ".git"
                    | "target"
                    | "Library"
                    | ".cache"
                    | ".cargo"
                    | ".rustup"
                    | ".npm"
                    | ".pnpm"
                    | ".pyenv"
                    | ".nvm"
                    | "venv"
                    | ".venv"
                    | "__pycache__"
            )
        })
        .filter_map(|e| e.ok())
    {
        if entry.file_name() == "config.json"
            && entry
                .path()
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n == ".mcp-vector-search")
                .unwrap_or(false)
        {
            configs.push(entry.path().to_path_buf());
        }
    }
    configs
}

/// Parse a mcp-vector-search config and derive `(project_root, index_name)`.
///
/// Why: shared by `convert` and `migrate` — both need the project root plus
/// a daemon-safe index name derived from the directory basename.
/// What: deserializes the JSON config and lowercases/de-spaces the basename.
/// Test: `parse_mvs_config` on a config with extra fields still succeeds.
pub(crate) fn parse_mvs_config(
    config_path: &std::path::Path,
) -> Result<(std::path::PathBuf, String)> {
    let content = std::fs::read_to_string(config_path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", config_path.display()))?;
    let config: MvsConfig = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("parse {}: {e}", config_path.display()))?;
    let name = config
        .project_root
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase().replace(' ', "-"))
        .unwrap_or_else(|| "project".to_string());
    Ok((config.project_root, name))
}

/// Outcome of attempting to convert a single project.
///
/// Why: shared with `migrate` so the index-migration phase can render the
/// same status set without re-deriving it.
/// What: enumerates the four terminal states of `convert_one`.
/// Test: exercised by `convert all` integration runs.
#[derive(Debug)]
pub(crate) enum ConvertStatus {
    Queued,
    AlreadyRegistered,
    DryRun,
    Failed(String),
}

/// Result of converting one project (name + path + terminal status).
///
/// Why: shared return type for `convert_one`, consumed by both `convert` and
/// `migrate` render paths.
/// What: pairs the derived index name and root path with a `ConvertStatus`.
/// Test: exercised by `convert all` integration runs.
#[derive(Debug)]
pub(crate) struct ConvertResult {
    pub(crate) name: String,
    pub(crate) path: std::path::PathBuf,
    pub(crate) status: ConvertStatus,
}

/// Convert one project: register it with the daemon (idempotent) and trigger
/// a reindex.
///
/// Why: the per-project register-then-reindex pair is reused verbatim by
/// `migrate mcp-vector-search`, so it is exposed crate-wide.
/// What: `search.index.create` then `search.index.reindex` over the daemon
/// socket (#9214) — the twins of `POST /indexes` and
/// `POST /indexes/:id/reindex`, with the same bodies. `client: None` is a dry
/// run: it contacts nothing and reports `DryRun`.
/// Test: `convert_one_registers_then_reindexes_over_the_socket`,
/// `convert_one_stops_at_a_refused_create`,
/// `convert_one_contacts_nothing_on_a_dry_run`.
pub(crate) async fn convert_one(
    project_root: std::path::PathBuf,
    index_name: String,
    client: Option<&DaemonClient>,
) -> ConvertResult {
    let status = match client {
        None => ConvertStatus::DryRun,
        Some(client) => register_and_reindex(client, &project_root, &index_name).await,
    };
    ConvertResult {
        name: index_name,
        path: project_root,
        status,
    }
}

/// Register `name` at `root` (idempotent), then queue its reindex.
async fn register_and_reindex(
    client: &DaemonClient,
    root: &std::path::Path,
    name: &str,
) -> ConvertStatus {
    // 1. Register the index. `created: false` means it already existed —
    //    still proceed to reindex so the user gets a fresh build.
    let create = json!({ "id": name, "root_path": root });
    let already_existed = match client.call(METHOD_INDEX_CREATE, create).await {
        Ok(body) => !body
            .get("created")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        Err(e) => return step_failed("create", e),
    };

    // 2. Kick off reindex (fire-and-forget — we don't follow the progress
    //    stream here because `convert all` may have many parallel migrations).
    let reindex = json!({ "index_id": name, "body": { "root_path": root } });
    match client.call(METHOD_INDEX_REINDEX, reindex).await {
        Ok(_) if already_existed => ConvertStatus::AlreadyRegistered,
        Ok(_) => ConvertStatus::Queued,
        Err(e) => step_failed("reindex", e),
    }
}

/// A failed socket call as a `Failed` row, worded as `daemon_rpc` words it.
fn step_failed(step: &str, e: DaemonCallError) -> ConvertStatus {
    ConvertStatus::Failed(format!("{step}: {}", super::daemon_rpc::rpc_error(e)))
}

/// Render one ConvertResult line for the `convert all` table.
fn print_convert_line(idx: usize, total: usize, r: &ConvertResult) {
    let prefix = format!("[{}/{}]", idx, total);
    let path = r.path.display().to_string();
    match &r.status {
        ConvertStatus::Queued => {
            println!(
                "  {} {} {:<24} → {}",
                prefix.dimmed(),
                "✓".green(),
                r.name,
                path.dimmed()
            );
        }
        ConvertStatus::AlreadyRegistered => {
            println!(
                "  {} {} {:<24} → {} {}",
                prefix.dimmed(),
                "↻".cyan(),
                r.name,
                path.dimmed(),
                "(already registered, reindexing)".dimmed()
            );
        }
        ConvertStatus::DryRun => {
            println!("  {} {:<24} {}", prefix.dimmed(), r.name, path.dimmed());
        }
        ConvertStatus::Failed(msg) => {
            println!(
                "  {} {} {:<24} → {} {}",
                prefix.dimmed(),
                "✗".red(),
                r.name,
                path.dimmed(),
                format!("({})", msg).red()
            );
        }
    }
}

/// Entry point for `trusty-search convert`.
pub async fn handle_convert(
    target: ConvertTarget,
    dry_run: bool,
    concurrency: usize,
) -> Result<()> {
    // #9214: start the daemon over its socket and stay on it.
    let client = DaemonClient::resolve()?;
    super::daemon_guard::ensure_daemon_up(&client).await?;

    match target {
        ConvertTarget::Project => handle_convert_project(dry_run, &client).await,
        ConvertTarget::All => handle_convert_all(dry_run, concurrency, client).await,
    }
}

/// Convert the mcp-vector-search project rooted at (or above) the cwd.
async fn handle_convert_project(dry_run: bool, client: &DaemonClient) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let config_path = find_mvs_config(&cwd).ok_or_else(|| {
        anyhow::anyhow!(
            "No .mcp-vector-search/config.json found in {} or any parent directory",
            cwd.display()
        )
    })?;
    let (root, name) = parse_mvs_config(&config_path)?;
    if dry_run {
        println!(
            "{} Dry run — would convert '{}' ({})",
            "·".dimmed(),
            name.bold(),
            root.display()
        );
        return Ok(());
    }

    println!(
        "{} Converting '{}' ({})…",
        "⟳".cyan(),
        name.bold(),
        root.display()
    );
    let result = convert_one(root, name, Some(client)).await;
    match &result.status {
        ConvertStatus::Queued => {
            println!(
                "{} Queued for reindex — watch progress with: {}",
                "✓".green(),
                "trusty-search status".cyan()
            );
        }
        ConvertStatus::AlreadyRegistered => {
            println!("{} Already registered — reindex queued", "↻".cyan());
        }
        ConvertStatus::Failed(msg) => {
            anyhow::bail!("Conversion failed: {}", msg);
        }
        ConvertStatus::DryRun => unreachable!(),
    }
    Ok(())
}

/// Convert every mcp-vector-search project found under `$HOME`, fanning out
/// with `tokio::task::JoinSet` and bounding concurrency by `concurrency`.
async fn handle_convert_all(dry_run: bool, concurrency: usize, client: DaemonClient) -> Result<()> {
    let home_display = dirs::home_dir()
        .map(|h| h.display().to_string())
        .unwrap_or_else(|| "$HOME".to_string());
    println!(
        "🔍 Scanning for mcp-vector-search projects under {}…",
        home_display
    );
    let configs = find_all_mvs_configs();
    if configs.is_empty() {
        println!("{} No mcp-vector-search projects found.", "·".dimmed());
        return Ok(());
    }

    if dry_run {
        println!(
            "{} Dry run — would convert {} projects:\n",
            "·".dimmed(),
            configs.len()
        );
    } else {
        println!(
            "{} Found {} projects. Converting (max {} concurrent)…\n",
            "·".dimmed(),
            configs.len(),
            concurrency
        );
    }

    let total = configs.len();
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
    let mut tasks = tokio::task::JoinSet::new();

    for (i, config_path) in configs.into_iter().enumerate() {
        let sem = sem.clone();
        // A dry run hands `convert_one` no client, so it contacts nothing.
        let client = (!dry_run).then(|| client.clone());
        tasks.spawn(async move {
            // Acquire permit inside the task so JoinSet limits concurrency
            // cleanly without us pre-allocating futures that all immediately
            // try to fire.
            let _permit = sem.acquire_owned().await.ok();
            let parsed = parse_mvs_config(&config_path);
            let result = match parsed {
                Ok((root, name)) => convert_one(root, name, client.as_ref()).await,
                Err(e) => ConvertResult {
                    name: config_path.display().to_string(),
                    path: config_path.clone(),
                    status: ConvertStatus::Failed(format!("parse: {e}")),
                },
            };
            (i + 1, result)
        });
    }

    let mut queued = 0usize;
    let mut already = 0usize;
    let mut dry = 0usize;
    let mut failed = 0usize;

    // Collect-then-sort so output is deterministic instead of racy. For
    // 69 projects this is trivially small.
    let mut results: Vec<(usize, ConvertResult)> = Vec::with_capacity(total);
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((i, r)) => results.push((i, r)),
            Err(e) => eprintln!("{} task panicked: {e}", "✗".red()),
        }
    }
    results.sort_by_key(|(i, _)| *i);

    for (i, r) in &results {
        print_convert_line(*i, total, r);
        match r.status {
            ConvertStatus::Queued => queued += 1,
            ConvertStatus::AlreadyRegistered => already += 1,
            ConvertStatus::DryRun => dry += 1,
            ConvertStatus::Failed(_) => failed += 1,
        }
    }

    println!();
    if dry_run {
        println!("{} Dry run complete: {} projects", "·".dimmed(), dry);
    } else {
        println!(
            "{} Summary: {} queued, {} already registered (reindexing), {} failed",
            "✓".green(),
            queued,
            already,
            failed
        );
        println!(
            "  Reindexing in background. Run {} to see progress.",
            "trusty-search list".cyan()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::mock_socket::mock_daemon;
    use serde_json::Value;
    use std::sync::{Arc, Mutex};
    use trusty_common::uds::server::RpcError;
    use trusty_search::service::rpc::error::CODE_CONFLICT;

    type CallLog = Arc<Mutex<Vec<(String, Value)>>>;

    /// #9214: a convert registers with `search.index.create` and then queues
    /// `search.index.reindex`, each carrying the HTTP body it replaced;
    /// `created: false` reports the index as already registered.
    #[tokio::test]
    async fn convert_one_registers_then_reindexes_over_the_socket() {
        for (created, want_existing) in [(true, false), (false, true)] {
            let calls: CallLog = Arc::default();
            let log = Arc::clone(&calls);
            let daemon = mock_daemon(move |method, params| {
                log.lock().expect("log").push((method.to_string(), params));
                Ok(json!({ "id": "proj", "created": created, "queued": true }))
            })
            .await;

            let result = convert_one("/tmp/proj".into(), "proj".into(), Some(&daemon.client)).await;

            assert_eq!(
                *calls.lock().expect("log"),
                vec![
                    (
                        METHOD_INDEX_CREATE.to_string(),
                        json!({ "id": "proj", "root_path": "/tmp/proj" })
                    ),
                    (
                        METHOD_INDEX_REINDEX.to_string(),
                        json!({ "index_id": "proj", "body": { "root_path": "/tmp/proj" } })
                    ),
                ]
            );
            let existing = matches!(result.status, ConvertStatus::AlreadyRegistered);
            let queued = matches!(result.status, ConvertStatus::Queued);
            assert_eq!(
                (existing, queued),
                (want_existing, !want_existing),
                "{result:?}"
            );
        }
    }

    /// #9214: a refused create is a failed row carrying the daemon's reason,
    /// and no reindex is queued behind it.
    #[tokio::test]
    async fn convert_one_stops_at_a_refused_create() {
        let calls: CallLog = Arc::default();
        let log = Arc::clone(&calls);
        let daemon = mock_daemon(move |method, params| {
            log.lock().expect("log").push((method.to_string(), params));
            Err(RpcError::new(CODE_CONFLICT, "index_root_overlap"))
        })
        .await;

        let result = convert_one("/tmp/proj".into(), "proj".into(), Some(&daemon.client)).await;

        let ConvertStatus::Failed(why) = &result.status else {
            panic!("a refused create must fail the row: {result:?}");
        };
        assert!(why.starts_with("create: daemon returned"), "{why}");
        assert!(why.contains("index_root_overlap"), "{why}");
        assert_eq!(
            calls.lock().expect("log").len(),
            1,
            "no reindex after a refusal"
        );
    }

    /// #9214: a dry run takes no client and reports what it would convert.
    #[tokio::test]
    async fn convert_one_contacts_nothing_on_a_dry_run() {
        let result = convert_one("/tmp/proj".into(), "proj".into(), None).await;
        assert!(matches!(result.status, ConvertStatus::DryRun), "{result:?}");
    }
}
