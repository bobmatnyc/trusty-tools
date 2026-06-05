//! Handlers for `trusty-search index add` and `trusty-search index list`.
//!
//! Why: issue #767 makes the opt-in allowlist first-class. These commands let
//! users explicitly approve a path for indexing (`add`) and inspect what is
//! currently approved (`list`), forming the only sanctioned path to registering
//! a new index under the default-deny model.
//!
//! What: thin wrappers around `crate::allowlist` helpers that validate against
//! the hard sensitive-path denylist, write/read
//! `~/.config/trusty-search/indexes.toml`, and print human-readable output.
//!
//! Test: `cargo run -- index add /srv/my-project` writes the allowlist then
//! prints a confirmation; `cargo run -- index list` shows what is in the file.

use anyhow::{Context, Result};
use colored::Colorize;
use std::path::PathBuf;

use crate::allowlist::{add_to_allowlist, AllowlistConfig, AllowlistEntry};

/// Handle `trusty-search index add <path>`.
///
/// Why: gives users a clear, auditable way to approve a path for indexing.
/// Validates against the hard denylist before writing to avoid silently
/// persisting a sensitive path. After writing, informs the user of the next
/// step (`trusty-search index <path>`).
/// What: resolves the path to an absolute form, calls `add_to_allowlist`,
/// prints a confirmation to stdout.
/// Test: run `trusty-search index add /tmp/my-project` and verify a denial;
/// run with a safe path and verify the file is updated and a confirmation is
/// printed.
pub async fn handle_allowlist_add(path: PathBuf, name: Option<String>) -> Result<()> {
    // Resolve relative paths against CWD so "." or "src" work as expected.
    let absolute = if path.is_absolute() {
        path.clone()
    } else {
        std::env::current_dir()
            .context("could not determine current directory")?
            .join(&path)
    };

    // Canonicalise (resolve symlinks) so the stored path is stable.
    let canonical = std::fs::canonicalize(&absolute)
        .with_context(|| format!("could not resolve path '{}'", absolute.display()))?;

    let entry = AllowlistEntry {
        path: canonical.clone(),
        name: name.clone(),
        exclude: vec![],
        extensions: vec![],
        skip_kg: false,
    };

    // `add_to_allowlist` validates against the denylist before writing.
    match add_to_allowlist(entry, None) {
        Ok(()) => {
            let basename = canonical
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "<unnamed>".to_string());
            let display_name = name.as_deref().unwrap_or(&basename);
            println!(
                "{} '{}' added to allowlist: {}",
                "✓".green(),
                display_name.bold(),
                canonical.display()
            );
            println!(
                "  Run {} to register and index it.",
                format!("trusty-search index {}", canonical.display()).cyan()
            );
        }
        Err(e) => {
            // Check if this is a denylist error (contains the telltale phrase)
            // vs. a file-system I/O error; both are fatal but we want a clear
            // prefix so the user knows who's responsible.
            let msg = format!("{e:#}");
            if msg.contains("sensitive pattern") || msg.contains("sensitive home") {
                anyhow::bail!("{} {}", "denied:".red(), msg);
            } else {
                return Err(e).context("could not write to allowlist");
            }
        }
    }
    Ok(())
}

/// Handle `trusty-search index list [--json]`.
///
/// Why: the allowlist is the single source of truth for "what may be indexed";
/// `index list` makes that truth inspectable from the command line.
/// What: loads `~/.config/trusty-search/indexes.toml` and prints every entry.
/// With `--json`, emits a JSON array of objects matching the TOML schema.
/// Test: run after `index add`; assert the path appears in the output.
pub async fn handle_allowlist_list(json: bool) -> Result<()> {
    let cfg = AllowlistConfig::load().context("could not load allowlist")?;
    let allowlist_path = AllowlistConfig::default_path();

    if json {
        let entries: Vec<serde_json::Value> = cfg
            .entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "path": e.path,
                    "name": e.name,
                    "exclude": e.exclude,
                    "extensions": e.extensions,
                    "skip_kg": e.skip_kg,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    if cfg.entries.is_empty() {
        println!(
            "{} Allowlist is empty — nothing can be indexed (default-deny).",
            "ℹ".yellow()
        );
        println!(
            "  Use {} to approve a path.",
            "trusty-search index add <path>".cyan()
        );
        println!("  Config: {}", allowlist_path.display());
        return Ok(());
    }

    println!(
        "{} {} path{} in allowlist ({})",
        "✓".green(),
        cfg.entries.len(),
        if cfg.entries.len() == 1 { "" } else { "s" },
        allowlist_path.display()
    );
    for entry in &cfg.entries {
        let name_part = match &entry.name {
            Some(n) => format!(" ({})", n.bold()),
            None => String::new(),
        };
        let extras: Vec<String> = {
            let mut v = Vec::new();
            if entry.skip_kg {
                v.push("skip_kg".into());
            }
            if !entry.exclude.is_empty() {
                v.push(format!("exclude: {}", entry.exclude.join(",")));
            }
            if !entry.extensions.is_empty() {
                v.push(format!("ext: {}", entry.extensions.join(",")));
            }
            v
        };
        let extras_part = if extras.is_empty() {
            String::new()
        } else {
            format!(" [{}]", extras.join(", "))
        };
        println!("  {}{}{}", entry.path.display(), name_part, extras_part);
    }
    Ok(())
}
