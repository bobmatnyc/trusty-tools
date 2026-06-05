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

/// Typed error returned by `add_to_allowlist_checked`, replacing the previous
/// string-match heuristic.
///
/// Why (issue #795): the old `handle_allowlist_add` distinguished denylist
/// from I/O errors by inspecting the error message string for "sensitive
/// pattern" or "sensitive home".  String-matching is fragile — a message
/// wording change silently broke the branch.  A typed enum makes the
/// distinction reliable and lets callers handle each case without parsing.
///
/// What: two variants — `Denied` carries the human-readable reason string from
/// `is_denied`; `Io` wraps the underlying `anyhow::Error` for all other
/// failures (file I/O, TOML parse, rename).
///
/// Test: `handle_allowlist_add` exercises the `Denied` path when called with a
/// sensitive path; the `Io` path is exercised when the config directory is
/// unwriteable.
#[derive(Debug)]
pub enum AllowlistAddError {
    /// The path matched a hard-denylist pattern.
    Denied(String),
    /// Any other failure (I/O, TOML parse, atomic rename).
    Io(anyhow::Error),
}

impl std::fmt::Display for AllowlistAddError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AllowlistAddError::Denied(reason) => write!(f, "denied: {reason}"),
            AllowlistAddError::Io(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for AllowlistAddError {}

/// Try to add `entry` to the allowlist, returning a typed error.
///
/// Why: extracted from `handle_allowlist_add` to provide a typed error that
/// callers can match without inspecting message strings (issue #795).
/// What: checks `is_denied` first; on a match returns `AllowlistAddError::Denied`.
/// On any other failure from `add_to_allowlist`, wraps the error as
/// `AllowlistAddError::Io`.
/// Test: `handle_allowlist_add` is the primary call site; the `Denied` branch
/// is exercised when a sensitive path is supplied.
fn add_to_allowlist_checked(entry: AllowlistEntry) -> std::result::Result<(), AllowlistAddError> {
    // Pre-check the denylist so we get a typed Denied variant rather than an
    // anyhow error whose message we'd have to string-match.
    if let Some(reason) = crate::allowlist::is_denied(&entry.path) {
        return Err(AllowlistAddError::Denied(reason));
    }
    add_to_allowlist(entry, None).map_err(AllowlistAddError::Io)
}

/// Handle `trusty-search index add <path>`.
///
/// Why: gives users a clear, auditable way to approve a path for indexing.
/// Validates against the hard denylist before writing to avoid silently
/// persisting a sensitive path. After writing, informs the user of the next
/// step (`trusty-search index <path>`).
/// What: resolves the path to an absolute form, calls `add_to_allowlist_checked`
/// (which uses the typed `AllowlistAddError` instead of string-matching),
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

    // `add_to_allowlist_checked` validates against the denylist before writing
    // and returns a typed error (issue #795: replaces string-match heuristic).
    match add_to_allowlist_checked(entry) {
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
        Err(AllowlistAddError::Denied(reason)) => {
            anyhow::bail!("{} {}", "denied:".red(), reason);
        }
        Err(AllowlistAddError::Io(e)) => {
            return Err(e).context("could not write to allowlist");
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
