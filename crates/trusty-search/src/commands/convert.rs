//! Handler for `trusty-search convert` — migrate mcp-vector-search projects.
//!
//! Why: the convert flow has two distinct sub-cases (single project / all
//! projects) plus dry-run handling and bounded-concurrency fan-out, plus the
//! mcp-vector-search config discovery + parsing helpers. Keeping it all in one
//! module co-locates the (de)serialization, the per-project HTTP dance, and
//! the render layer.
//! What: `handle_convert` is the entry point; everything else is private.
//! Test: `cargo run -- convert project --dry-run` from inside an
//! mcp-vector-search repo prints the would-convert line; `convert all
//! --dry-run` enumerates every detected project.

use anyhow::Result;
use clap::ValueEnum;
use colored::Colorize;

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
/// Why: the per-project HTTP dance (register → reindex) is reused verbatim by
/// `migrate mcp-vector-search`, so it is exposed crate-wide.
/// What: POSTs `/indexes` then `/indexes/:id/reindex`; returns a `ConvertResult`.
/// Test: `convert project` against a running daemon yields a `Queued` status.
pub(crate) async fn convert_one(
    project_root: std::path::PathBuf,
    index_name: String,
    base_url: &str,
    dry_run: bool,
) -> ConvertResult {
    if dry_run {
        return ConvertResult {
            name: index_name,
            path: project_root,
            status: ConvertStatus::DryRun,
        };
    }

    let client = match trusty_common::server::daemon_http_client() {
        Ok(c) => c,
        Err(e) => {
            return ConvertResult {
                name: index_name,
                path: project_root,
                status: ConvertStatus::Failed(format!("failed to build HTTP client: {e}")),
            };
        }
    };

    // 1. Register the index. 200 with body.created=false means it already
    //    existed — still proceed to reindex so the user gets a fresh build.
    let create_url = format!("{base_url}/indexes");
    let create_resp = client
        .post(&create_url)
        .json(&serde_json::json!({
            "id": index_name,
            "root_path": project_root,
        }))
        .send()
        .await;

    let already_existed = match create_resp {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value =
                resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
            !body
                .get("created")
                .and_then(|v| v.as_bool())
                .unwrap_or(true)
        }
        Ok(resp) => {
            return ConvertResult {
                name: index_name,
                path: project_root,
                status: ConvertStatus::Failed(format!("create returned {}", resp.status())),
            };
        }
        Err(e) => {
            return ConvertResult {
                name: index_name,
                path: project_root,
                status: ConvertStatus::Failed(format!("create error: {e}")),
            };
        }
    };

    // 2. Kick off reindex (fire-and-forget — we don't follow the SSE stream
    //    here because `convert all` may have many parallel migrations).
    let reindex_url = format!("{base_url}/indexes/{index_name}/reindex");
    let reindex_resp = client
        .post(&reindex_url)
        .json(&serde_json::json!({ "root_path": project_root }))
        .send()
        .await;

    match reindex_resp {
        Ok(resp) if resp.status().is_success() => ConvertResult {
            name: index_name,
            path: project_root,
            status: if already_existed {
                ConvertStatus::AlreadyRegistered
            } else {
                ConvertStatus::Queued
            },
        },
        Ok(resp) => ConvertResult {
            name: index_name,
            path: project_root,
            status: ConvertStatus::Failed(format!("reindex returned {}", resp.status())),
        },
        Err(e) => ConvertResult {
            name: index_name,
            path: project_root,
            status: ConvertStatus::Failed(format!("reindex error: {e}")),
        },
    }
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
    // #9214: start the daemon over its socket, then resolve its HTTP base.
    let base = super::daemon_http::ensure_daemon_http_base().await?;

    match target {
        ConvertTarget::Project => handle_convert_project(dry_run, &base).await,
        ConvertTarget::All => handle_convert_all(dry_run, concurrency, base).await,
    }
}

/// Convert the mcp-vector-search project rooted at (or above) the cwd.
async fn handle_convert_project(dry_run: bool, base: &str) -> Result<()> {
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
    let result = convert_one(root, name, base, false).await;
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
async fn handle_convert_all(dry_run: bool, concurrency: usize, base: String) -> Result<()> {
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
    let base = std::sync::Arc::new(base);
    let mut tasks = tokio::task::JoinSet::new();

    for (i, config_path) in configs.into_iter().enumerate() {
        let sem = sem.clone();
        let base = base.clone();
        tasks.spawn(async move {
            // Acquire permit inside the task so JoinSet limits concurrency
            // cleanly without us pre-allocating futures that all immediately
            // try to fire.
            let _permit = sem.acquire_owned().await.ok();
            let parsed = parse_mvs_config(&config_path);
            let result = match parsed {
                Ok((root, name)) => convert_one(root, name, &base, dry_run).await,
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
