//! Handler for `trusty-search watch`.

use super::daemon_utils::daemon_base_url;
use super::index_resolve::{print_index_header, resolve_index};
use crate::detect::detect_project;
use anyhow::Result;
use colored::Colorize;

/// Why: issue #1621 wired the `FileWatcher` directly into the daemon — every
/// registered (allowlisted) index is watched automatically and saves are
/// incrementally indexed within the 500ms debounce window. There is therefore
/// no separate "watch" product to run; this command is now an informational
/// status check that confirms the daemon-managed watcher covers the resolved
/// index, rather than the old "not yet implemented" stub.
/// What: resolves the index id, ensures the daemon is up, prints the watched
/// root and a note that watching is automatic. Honours `TRUSTY_DISABLE_WATCHER`
/// in the message so operators who opted out understand why saves are not
/// auto-indexed.
/// Test: `cargo run -- watch` prints the daemon-managed message without
/// panicking; the no-op nature means there is no long-running loop to assert on.
pub async fn handle_watch(
    explicit_index: &Option<String>,
    path: Option<std::path::PathBuf>,
) -> Result<()> {
    let (index_id, warned) = resolve_index(explicit_index);
    print_index_header(&index_id, warned);
    crate::commands::daemon_guard::ensure_daemon_running_or_exit(&daemon_base_url()).await?;
    let watch_path = path.unwrap_or_else(|| {
        let cwd = std::env::current_dir().unwrap_or_default();
        detect_project(&cwd).root_path
    });

    // Issue #1621: the daemon watches every registered index automatically.
    let disabled = std::env::var("TRUSTY_DISABLE_WATCHER").as_deref() == Ok("1");
    if disabled {
        println!(
            "{} File watching is {} ({} is set).",
            "⚠".yellow(),
            "disabled".red(),
            "TRUSTY_DISABLE_WATCHER=1".cyan()
        );
        println!(
            "  Re-index manually with {} or unset the variable and restart the daemon.",
            format!("trusty-search reindex {}", watch_path.display()).cyan()
        );
        return Ok(());
    }

    println!(
        "{} {} is watched automatically by the daemon — saves under {} are indexed within ~500ms.",
        "◉".green(),
        format!("'{}'", index_id).bold(),
        watch_path.display().to_string().cyan(),
    );
    println!(
        "  No separate watch process is needed (issue #1621). Register a path with {} to have the daemon watch it.",
        "trusty-search index <path>".cyan()
    );
    Ok(())
}
