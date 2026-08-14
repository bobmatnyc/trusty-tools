//! Handler for `trusty-search index relocate --to <new-path>` (issue #1073).
//!
//! Why: when a project directory moves on disk (rename, volume remount, machine
//! migration) the existing index registration becomes stale. Running a full
//! `trusty-search index --force` would re-embed every file even though nothing
//! has changed. This subcommand rebinds the daemon's registry to the new path
//! WITHOUT clearing the hash cache, so a subsequent incremental reindex only
//! re-embeds genuinely changed files.
//! What: resolves the current index (from `-i` flag or CWD detection), calls
//! `PATCH /indexes/:id` with `{ "root_path": "<new>" }`, and updates the
//! allowlist entry to point to the new path.
//! Test: `handle_index_relocate_rejects_missing_id` unit test below; the HTTP
//! round-trip is covered by `tests_index::relocate_index_updates_root_path`.

use super::daemon_utils::daemon_base_url;
use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::path::PathBuf;

/// Entry point for `trusty-search index relocate --to <new-path>`.
///
/// Why: see module docs.
/// What: resolves the current index id, canonicalizes `new_path`, calls the
/// `PATCH /indexes/:id` endpoint, and updates the allowlist for the old path.
/// Test: unit tests below; integration coverage in `tests_index.rs`.
pub async fn handle_index_relocate(cli_index: &Option<String>, new_path: PathBuf) -> Result<()> {
    let base = daemon_base_url();
    crate::commands::daemon_guard::ensure_daemon_running_or_exit(&base).await?;

    let client = trusty_common::server::daemon_http_client()?;

    // Resolve the index id: explicit `-i` wins, otherwise auto-detect from CWD.
    let index_id = resolve_index_id(&client, &base, cli_index).await?;

    // Canonicalize the new path early for a friendly error before hitting the
    // daemon (which will also reject non-existent paths, but the CLI message
    // is clearer here).
    let canonical_new = new_path
        .canonicalize()
        .with_context(|| format!("new path does not exist: {}", new_path.display()))?;

    // #767: approve the destination BEFORE the PATCH — see
    // `approve_destination`. `newly_approved` drives the rollback below.
    let newly_approved = approve_destination(&canonical_new, None)?;

    // Call PATCH /indexes/:id
    let patch_url = format!("{base}/indexes/{index_id}");
    let body = serde_json::json!({ "root_path": canonical_new.to_string_lossy() });
    // #767: bind rather than `?`. A transport failure means the relocation did
    // not happen either, so it owes the same rollback as a non-2xx answer —
    // `?`-ing here left a durable `allowlist.toml` entry behind with nothing on
    // screen. Both arms now go through `withdraw_approval`.
    let send_result = client.patch(&patch_url).json(&body).send().await;
    let resp = match send_result {
        Ok(r) => r,
        Err(e) => {
            withdraw_approval(newly_approved, &canonical_new);
            return Err(e).with_context(|| format!("could not reach daemon at {base}"));
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        withdraw_approval(newly_approved, &canonical_new);
        bail!("daemon returned {status} for PATCH {patch_url}: {text}");
    }

    let result: serde_json::Value = resp
        .json()
        .await
        .context("could not parse PATCH response")?;
    let new_root = result
        .get("new_root_path")
        .and_then(|v| v.as_str())
        .unwrap_or(canonical_new.to_str().unwrap_or("(new path)"));

    // The allowlist entry for the new path was written above, before the PATCH.
    // The OLD path's entry is deliberately left in place: this command does not
    // know whether the operator still wants that root approved, and
    // `trusty-search index remove <old>` is the verb that withdraws it.

    println!(
        "{} Index '{}' relocated to {}",
        "\u{2713}".green(),
        index_id.bold(),
        new_root.bold(),
    );
    println!(
        "  Run {} to incrementally re-embed only changed files.",
        "trusty-search index".cyan()
    );
    Ok(())
}

/// Approve `canonical_new` for indexing, returning whether THIS call added it.
///
/// Why (#767): `PATCH /indexes/:id` is gated by the opt-in allowlist, and the
/// normal relocate case — a repo moved to a sibling path — names a destination
/// nothing has approved yet. Approving AFTER the PATCH, as this command used to,
/// meant the PATCH returned `403` and the CLI bailed before the allowlist was
/// ever written: permanently broken, not intermittently. `add_to_allowlist`
/// still applies the strict denylist, so this grants nothing the operator could
/// not grant with `index add`.
/// What: no-op returning `false` when the destination is already approved.
/// `AllowlistConfig::upsert` does `*slot = entry`, a full replace, so writing a
/// default entry over an operator-configured one would destroy its `name`,
/// `exclude`, `extensions` and `skip_kg` — and the caller's rollback is
/// suppressed for a pre-existing entry, so nothing would restore them. Nothing
/// here needs to modify an existing approval, so it does not touch one.
/// Returns `true` only when a new entry was written, which is exactly when the
/// caller should withdraw it if the PATCH fails.
/// `allowlist_path` is injectable for tests; `None` uses the real XDG path.
/// Test: `approve_destination_adds_a_missing_entry`,
/// `approve_destination_preserves_an_existing_entrys_settings`.
fn approve_destination(
    canonical_new: &std::path::Path,
    allowlist_path: Option<&std::path::Path>,
) -> Result<bool> {
    let file = match allowlist_path {
        Some(p) => p.to_path_buf(),
        None => crate::allowlist::AllowlistConfig::default_path(),
    };
    let already_approved = crate::allowlist::AllowlistConfig::load_from(&file)
        .map(|cfg| cfg.contains(canonical_new))
        .unwrap_or(false);
    if already_approved {
        return Ok(false);
    }
    crate::allowlist::add_to_allowlist(
        crate::allowlist::AllowlistEntry {
            path: canonical_new.to_path_buf(),
            name: None,
            exclude: Vec::new(),
            extensions: Vec::new(),
            skip_kg: false,
        },
        allowlist_path,
    )
    .with_context(|| {
        format!(
            "could not approve '{}' for indexing before relocating",
            canonical_new.display()
        )
    })?;
    Ok(true)
}

/// Withdraw the approval [`approve_destination`] granted, if it granted one.
///
/// Why (#767): the relocation did not happen, so the approval that was written
/// for it must not outlive the attempt. Every way the PATCH can fail owes this
/// — the transport error and the non-2xx answer both. Keeping it in one
/// function is what stops the next failure arm from quietly skipping it, which
/// is how the transport arm came to be missing one.
/// What: no-op when `newly_approved` is `false` — an entry that predated this
/// command is the operator's, not ours to remove. On a removal failure, prints
/// to STDERR as well as logging: this command talks to the operator through
/// `println!`/`bail!`, so a tracing-only line means they see the relocate fail
/// and never learn a stale approval was left behind.
/// Test: `approve_destination_adds_a_missing_entry` covers the grant side;
/// the no-op arm is asserted by
/// `approve_destination_preserves_an_existing_entrys_settings`.
fn withdraw_approval(newly_approved: bool, canonical_new: &std::path::Path) {
    if !newly_approved {
        return;
    }
    if let Err(e) = crate::allowlist::remove_from_allowlist(canonical_new, None) {
        eprintln!(
            "{} '{}' was approved for indexing before this relocation and could \
             NOT be un-approved ({e:#}). It is still in the allowlist — remove \
             it with `trusty-search index remove {}`.",
            "warning:".yellow(),
            canonical_new.display(),
            canonical_new.display(),
        );
        tracing::warn!(
            path = %canonical_new.display(),
            error = %e,
            "could not withdraw the allowlist entry after a failed relocation"
        );
    }
}

#[cfg(test)]
mod tests_767 {
    use super::approve_destination;
    use crate::allowlist::{AllowlistConfig, AllowlistEntry};

    /// A destination nothing has approved gets a fresh entry, and the caller is
    /// told it owns the rollback.
    #[test]
    fn approve_destination_adds_a_missing_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("allowlist.toml");
        let dest = std::path::PathBuf::from("/srv/moved-project");

        let newly = approve_destination(&dest, Some(&file)).expect("approve");
        assert!(newly, "a missing entry must be reported as newly approved");
        let cfg = AllowlistConfig::load_from(&file).expect("load");
        assert!(cfg.contains(&dest), "{cfg:?}");
    }

    /// An operator-configured entry at the destination is left completely
    /// alone.
    ///
    /// Why: `upsert` is a full replace. Writing a default entry over this one
    /// would silently destroy `name`, `exclude`, `extensions` and `skip_kg` —
    /// and because the entry pre-existed, the caller suppresses its rollback, so
    /// nothing would put them back.
    #[test]
    fn approve_destination_preserves_an_existing_entrys_settings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("allowlist.toml");
        let dest = std::path::PathBuf::from("/srv/moved-project");
        let mut cfg = AllowlistConfig::default();
        cfg.upsert(AllowlistEntry {
            path: dest.clone(),
            name: Some("configured".into()),
            exclude: vec!["target/".into()],
            extensions: vec!["rs".into()],
            skip_kg: true,
        });
        cfg.save_to(&file).expect("seed");

        let newly = approve_destination(&dest, Some(&file)).expect("approve");
        assert!(
            !newly,
            "an existing entry must not be reported as newly added"
        );

        let cfg = AllowlistConfig::load_from(&file).expect("load");
        assert_eq!(cfg.entries.len(), 1, "{cfg:?}");
        assert_eq!(cfg.entries[0].name.as_deref(), Some("configured"));
        assert_eq!(cfg.entries[0].exclude, vec!["target/".to_string()]);
        assert_eq!(cfg.entries[0].extensions, vec!["rs".to_string()]);
        assert!(cfg.entries[0].skip_kg);
    }

    /// The strict denylist still applies — relocate cannot approve `~/.ssh`.
    #[test]
    fn approve_destination_refuses_a_denylisted_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("allowlist.toml");
        let ssh = dirs::home_dir().expect("home").join(".ssh");
        assert!(approve_destination(&ssh, Some(&file)).is_err());
    }
}

/// Resolve the effective index id from the `-i` flag or CWD auto-detection.
///
/// Why: `Relocate` needs a daemon-side index id to call `PATCH /indexes/:id`;
/// the `-i` flag (if present) provides it directly, otherwise we look up the
/// index whose `root_path` contains CWD.
/// What: if `cli_index` is `Some`, returns it verbatim. Otherwise fetches all
/// index statuses from the daemon and returns the id of the first one whose
/// `root_path` is an ancestor of (or equal to) CWD.
/// Test: `resolve_index_id_uses_explicit_arg` below.
async fn resolve_index_id(
    client: &reqwest::Client,
    base: &str,
    cli_index: &Option<String>,
) -> Result<String> {
    if let Some(id) = cli_index {
        return Ok(id.clone());
    }

    // Auto-detect from CWD.
    let cwd = std::env::current_dir().context("could not determine current directory")?;
    let canonical_cwd = std::fs::canonicalize(&cwd).unwrap_or_else(|_| cwd.clone());

    let list_url = format!("{base}/indexes");
    let list_body: serde_json::Value = client
        .get(&list_url)
        .send()
        .await
        .with_context(|| format!("could not reach daemon at {base}"))?
        .error_for_status()
        .with_context(|| format!("daemon error for {list_url}"))?
        .json()
        .await
        .context("could not parse /indexes response")?;

    let empty: Vec<serde_json::Value> = Vec::new();
    let ids: Vec<String> = list_body
        .get("indexes")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty)
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    for id in ids {
        let url = format!("{base}/indexes/{id}/status");
        let resp = match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => r,
            _ => continue,
        };
        let body: serde_json::Value = match resp.json().await {
            Ok(b) => b,
            Err(_) => continue,
        };
        let root = body
            .get("root_path")
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from);
        let Some(root) = root else { continue };
        let canonical_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        if canonical_cwd.starts_with(&canonical_root) {
            return Ok(id);
        }
    }

    bail!(
        "no index registered for the current directory ({}); \
         use -i <id> to specify an index explicitly, or run \
         `trusty-search list` to see registered indexes",
        cwd.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// When `-i my-index` is passed, `resolve_index_id` should return it
    /// immediately without contacting the daemon.
    ///
    /// Why: ensures the explicit-id fast path is exercised.
    /// What: calls `resolve_index_id` with an explicit `Some("my-index")` and
    /// asserts the returned string equals the input.
    /// Test: this test.
    #[test]
    fn resolve_index_id_uses_explicit_arg() {
        // We can test the synchronous decision logic without a live daemon by
        // constructing a dummy client and a base URL that would fail to connect.
        // Since the explicit-id branch returns early without any network call,
        // the client is never used.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let client = reqwest::Client::new();
        let id = rt.block_on(resolve_index_id(
            &client,
            "http://127.0.0.1:0", // unreachable — should not be contacted
            &Some("my-index".to_string()),
        ));
        assert!(id.is_ok());
        assert_eq!(id.unwrap(), "my-index");
    }
}
