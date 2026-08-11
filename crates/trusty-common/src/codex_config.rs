//! Codex CLI MCP-server registration (`~/.codex/config.toml`).
//!
//! Why: Codex Desktop lists a registered stdio server as "enabled" whether or
//! not the process it launches ever speaks MCP. `trusty-search` was registered
//! with `args = []`, so Codex exec'd the bare binary, which printed its
//! top-level help and exited before initialization — the connection showed
//! green and the model got no tools (#5264). The same argument-boundary defect
//! was filed for `trusty-memory` (#5265), so the repair lives here rather than
//! in either binary.
//!
//! What: [`codex_config_path`] resolves the file, and [`patch_mcp_server`]
//! idempotently upserts one `[mcp_servers.<key>]` table with a real argument
//! vector, repairing a registration whose `args` is empty, a single joined
//! string, or a JSON-looking string such as `["[\"serve\"]"]`. Edits go through
//! `toml_edit`, so every other table, comment, and blank line in the operator's
//! config survives byte-for-byte.
//!
//! Test: `codex_config_path_is_under_dot_codex`,
//! `patch_mcp_server_creates_missing_file`, `patch_mcp_server_is_idempotent`,
//! `patch_mcp_server_repairs_empty_args`,
//! `patch_mcp_server_repairs_nested_json_string_args`,
//! `patch_mcp_server_preserves_other_servers_and_comments`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml_edit::{Array, DocumentMut, Item, Table, value};

/// Table the Codex CLI reads stdio MCP-server registrations from.
const MCP_SERVERS_TABLE: &str = "mcp_servers";

/// Path to the Codex CLI config file under `home`.
///
/// Why: the location is a Codex convention, not something the caller should
/// re-derive; taking `home` as an argument keeps the function testable without
/// touching the real `$HOME`.
/// What: `<home>/.codex/config.toml`.
/// Test: `codex_config_path_is_under_dot_codex`.
pub fn codex_config_path(home: &Path) -> PathBuf {
    home.join(".codex").join("config.toml")
}

/// Idempotently register one stdio MCP server in a Codex config file.
///
/// Why: re-running a setup command must be safe, and must REPAIR a broken
/// registration rather than leave it — #5264's reporter had already hand-edited
/// their config into `args = ["[\"serve\"]"]`, one literal argument that
/// launches `trusty-search '["serve"]'` and still never initializes MCP. A
/// writer that only fills in an absent key would have left that in place.
///
/// What: parses `path` (a missing file is an empty document), then sets
/// `[mcp_servers.<server_key>]`'s `command` and `args` to exactly the supplied
/// values. Returns `Ok(false)` without writing when both already match — so a
/// second run touches nothing — and `Ok(true)` after a write. Unrelated tables,
/// comments, and formatting are preserved because the document is edited in
/// place rather than re-serialized from a value tree. Parent directories are
/// created; the write goes to a temp file and is renamed, so an interrupted run
/// cannot truncate the operator's config.
///
/// Errors: an unreadable file or a TOML parse failure. Both are surfaced rather
/// than swallowed — silently rewriting a config we could not parse would
/// destroy it.
///
/// Test: `patch_mcp_server_creates_missing_file`,
/// `patch_mcp_server_is_idempotent`, `patch_mcp_server_repairs_empty_args`,
/// `patch_mcp_server_repairs_nested_json_string_args`,
/// `patch_mcp_server_preserves_other_servers_and_comments`.
pub fn patch_mcp_server(
    path: &Path,
    server_key: &str,
    command: &str,
    args: &[&str],
) -> Result<bool> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(e).with_context(|| format!("read Codex config {}", path.display()));
        }
    };
    let mut doc: DocumentMut = existing
        .parse()
        .with_context(|| format!("parse Codex config {}", path.display()))?;

    if server_entry_matches(&doc, server_key, command, args) {
        return Ok(false);
    }

    let servers = doc
        .entry(MCP_SERVERS_TABLE)
        .or_insert_with(|| Item::Table(implicit_table()));
    if servers.as_table().is_none() {
        // A non-table `mcp_servers` is a broken config, not something to merge
        // into — replace it so the registration is at least reachable.
        *servers = Item::Table(implicit_table());
    }
    let servers = servers
        .as_table_mut()
        .expect("mcp_servers coerced to a table above");

    let entry = servers
        .entry(server_key)
        .or_insert_with(|| Item::Table(Table::new()));
    if entry.as_table().is_none() {
        *entry = Item::Table(Table::new());
    }
    let entry = entry
        .as_table_mut()
        .expect("server entry coerced to a table above");

    entry["command"] = value(command);
    let mut arg_array = Array::new();
    for a in args {
        arg_array.push(*a);
    }
    entry["args"] = value(arg_array);

    write_atomic(path, &doc.to_string())?;
    Ok(true)
}

/// Whether the config already registers exactly this command and argument vector.
///
/// Why: the idempotency check has to compare the parsed ARGUMENT VECTOR, not the
/// rendered text — `args = ["serve"]` and `args = ["[\"serve\"]"]` differ by one
/// element's contents, and only the second is broken.
/// What: reads `[mcp_servers.<key>]`, returning `false` for any shape that is
/// not a table with a string `command` and a string array equal to `args`.
/// Test: covered through `patch_mcp_server`'s tests.
fn server_entry_matches(doc: &DocumentMut, server_key: &str, command: &str, args: &[&str]) -> bool {
    let Some(entry) = doc
        .get(MCP_SERVERS_TABLE)
        .and_then(Item::as_table)
        .and_then(|t| t.get(server_key))
        .and_then(Item::as_table)
    else {
        return false;
    };
    if entry.get("command").and_then(Item::as_str) != Some(command) {
        return false;
    }
    let Some(existing) = entry.get("args").and_then(Item::as_array) else {
        return false;
    };
    existing.len() == args.len()
        && existing
            .iter()
            .zip(args)
            .all(|(got, want)| got.as_str() == Some(*want))
}

/// A `[mcp_servers]` header that renders as `[mcp_servers.<key>]` sub-tables.
fn implicit_table() -> Table {
    let mut t = Table::new();
    t.set_implicit(true);
    t
}

/// Write `contents` to `path` via a temp file and rename.
///
/// Why: Codex's config holds the operator's whole CLI configuration. A crash
/// partway through a direct write would leave it truncated and unparseable —
/// the same hazard `claude_config::write_json_atomic` exists to avoid.
/// What: creates the parent directory, writes `<path>.tmp`, renames it onto
/// `path`.
/// Test: exercised by every filesystem test in this module.
fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir {}", parent.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, contents.as_bytes())
        .with_context(|| format!("write temp file {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} onto {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the path is the contract with the Codex CLI; getting it wrong makes
    /// every other function in this module write to a file nobody reads.
    #[test]
    fn codex_config_path_is_under_dot_codex() {
        let p = codex_config_path(Path::new("/Users/x"));
        assert_eq!(p, PathBuf::from("/Users/x/.codex/config.toml"));
    }

    /// Why (#5264): a machine with no Codex config yet must still end up with a
    /// working registration rather than an error.
    #[test]
    fn patch_mcp_server_creates_missing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = codex_config_path(tmp.path());

        let wrote = patch_mcp_server(&path, "trusty-search", "trusty-search", &["serve"])
            .expect("patch a missing config");
        assert!(wrote, "a fresh registration is a write");

        let doc: DocumentMut = std::fs::read_to_string(&path)
            .expect("config written")
            .parse()
            .expect("valid TOML");
        let entry = doc["mcp_servers"]["trusty-search"]
            .as_table()
            .expect("server table");
        assert_eq!(entry["command"].as_str(), Some("trusty-search"));
        let args: Vec<&str> = entry["args"]
            .as_array()
            .expect("args array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(args, vec!["serve"], "the MCP entrypoint is `serve`");
    }

    /// Why: setup commands are re-run constantly; a second run must not rewrite
    /// the file (and so must not churn its mtime or backups).
    #[test]
    fn patch_mcp_server_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = codex_config_path(tmp.path());

        assert!(patch_mcp_server(&path, "trusty-search", "trusty-search", &["serve"]).unwrap());
        let first = std::fs::read_to_string(&path).expect("read");
        assert!(
            !patch_mcp_server(&path, "trusty-search", "trusty-search", &["serve"]).unwrap(),
            "an unchanged registration must not report a write"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
    }

    /// Why (#5264): this is the exact registration the reporter had — Codex
    /// exec'd the bare binary, which printed help and exited before MCP
    /// initialization while the connection still showed as enabled.
    #[test]
    fn patch_mcp_server_repairs_empty_args() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = codex_config_path(tmp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[mcp_servers.trusty-search]\ncommand = \"trusty-search\"\nargs = []\n",
        )
        .unwrap();

        assert!(
            patch_mcp_server(&path, "trusty-search", "trusty-search", &["serve"]).unwrap(),
            "an empty argument vector must be repaired, not left alone"
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("\"serve\"")
        );
    }

    /// Why (#5264 follow-up): after hand-editing, the reporter's config held
    /// `args = ["[\"serve\"]"]` — one literal argument whose text merely LOOKS
    /// like an argument vector. The process receives `["serve"]` as a single
    /// token and still cannot initialize, so a repair that only checks
    /// "is `args` non-empty?" would pass this straight through.
    #[test]
    fn patch_mcp_server_repairs_nested_json_string_args() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = codex_config_path(tmp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[mcp_servers.trusty-search]\ncommand = \"trusty-search\"\nargs = [\"[\\\"serve\\\"]\"]\n",
        )
        .unwrap();

        assert!(
            patch_mcp_server(&path, "trusty-search", "trusty-search", &["serve"]).unwrap(),
            "a JSON-looking single argument must be repaired"
        );

        let doc: DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        let args: Vec<&str> = doc["mcp_servers"]["trusty-search"]["args"]
            .as_array()
            .expect("args array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(args, vec!["serve"], "got {args:?}");
    }

    /// Why: this writes into a file the operator owns and maintains by hand.
    /// Clobbering their other servers, comments, or unrelated settings would be
    /// a far worse defect than the one being fixed.
    #[test]
    fn patch_mcp_server_preserves_other_servers_and_comments() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = codex_config_path(tmp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "# my codex config\nmodel = \"gpt-5\"\n\n\
             [mcp_servers.other]\ncommand = \"other-server\"\nargs = [\"run\"]\n",
        )
        .unwrap();

        assert!(patch_mcp_server(&path, "trusty-search", "trusty-search", &["serve"]).unwrap());

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my codex config"), "comment lost:\n{text}");
        assert!(text.contains("model = \"gpt-5\""), "setting lost:\n{text}");
        let doc: DocumentMut = text.parse().unwrap();
        assert_eq!(
            doc["mcp_servers"]["other"]["command"].as_str(),
            Some("other-server"),
            "another server's registration was clobbered"
        );
        assert_eq!(
            doc["mcp_servers"]["trusty-search"]["command"].as_str(),
            Some("trusty-search")
        );
    }
}
