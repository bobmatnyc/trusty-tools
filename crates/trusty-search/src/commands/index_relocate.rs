//! Handler for `trusty-search index relocate --to <new-path>` (issue #1073).
//!
//! Why: when a project directory moves on disk (rename, volume remount, machine
//! migration) the existing index registration becomes stale. Running a full
//! `trusty-search index --force` would re-embed every file even though nothing
//! has changed. This subcommand rebinds the daemon's registry to the new path
//! WITHOUT clearing the hash cache, so a subsequent incremental reindex only
//! re-embeds genuinely changed files.
//! What: resolves the current index (from `-i` flag or CWD detection), calls
//! `search.index.relocate` over the daemon socket (#9214) with
//! `{ "root_path": "<new>" }`, and approves the new path in the allowlist.
//! Test: `resolve_index_id_uses_explicit_arg`,
//! `an_unreadable_status_refuses_cwd_detection`, `approve_destination_*`.

use super::explicit_target::{flag_only_index, IndexIdSource};
use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::path::{Path, PathBuf};
use trusty_search::service::daemon_client::DaemonClient;
use trusty_search::service::rpc::writes::METHOD_INDEX_RELOCATE;

/// Entry point for `trusty-search index relocate --to <new-path>`.
///
/// Why: see module docs.
/// What: resolves the current index id, canonicalizes `new_path`, approves it,
/// and calls `search.index.relocate`; any failure withdraws the approval.
/// `TRUSTY_INDEX` alone refuses before any network call (#8737): a relocate
/// rewrites the index's root, so its target needs a real `-i` or CWD detection.
/// Test: unit tests below; the #8737 refusal by
/// `relocate_env_only_refuses_with_no_requests`.
pub async fn handle_index_relocate(
    cli_index: &Option<String>,
    index_source: Option<IndexIdSource>,
    new_path: PathBuf,
) -> Result<()> {
    let cli_index = &flag_only_index("relocate", cli_index, index_source)?;
    // #9214: the socket only; nothing answering is an error naming it.
    let client = super::daemon_rpc::connect().await?;

    // Resolve the index id: explicit `-i` wins, otherwise auto-detect from CWD.
    let cwd = std::env::current_dir().context("could not determine current directory")?;
    let index_id = resolve_index_id(&client, cli_index, &cwd).await?;

    // Canonicalize the new path early for a friendly error before hitting the
    // daemon (which will also reject non-existent paths, but the CLI message
    // is clearer here).
    let canonical_new = new_path
        .canonicalize()
        .with_context(|| format!("new path does not exist: {}", new_path.display()))?;

    // #767: approve the destination BEFORE the relocate — see
    // `approve_destination`. `newly_approved` drives the rollback below.
    let newly_approved = approve_destination(&canonical_new, None)?;

    // #767: every failure — refusal, broken exchange, unreachable socket —
    // means the relocation did not happen, so each owes the same rollback via
    // `withdraw_approval`.
    let params = serde_json::json!({
        "index_id": index_id,
        "body": { "root_path": canonical_new.to_string_lossy() },
    });
    let result = match super::daemon_rpc::call(&client, METHOD_INDEX_RELOCATE, params).await {
        Ok(result) => result,
        Err(e) => {
            withdraw_approval(newly_approved, &canonical_new);
            return Err(e).with_context(|| format!("could not relocate index '{index_id}'"));
        }
    };
    let new_root = result
        .get("new_root_path")
        .and_then(|v| v.as_str())
        .unwrap_or(canonical_new.to_str().unwrap_or("(new path)"));

    // The allowlist entry for the new path was written above, before the relocate.
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
/// caller should withdraw it if the relocate fails.
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
/// for it must not outlive the attempt. Every way the relocate can fail owes this
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
/// Why: `Relocate` needs a daemon-side index id to call `search.index.relocate`;
/// the `-i` flag (if present) provides it directly, otherwise we look up the
/// index whose `root_path` contains `cwd`.
/// What: if `cli_index` is `Some`, returns it verbatim. Otherwise reads every
/// resident index's status and returns the id of the first one whose
/// `root_path` is an ancestor of (or equal to) `cwd`. #9214: a status that
/// cannot be read refuses rather than being skipped — the skipped index could
/// be the one that owns `cwd`.
/// Test: `resolve_index_id_uses_explicit_arg`,
/// `an_unreadable_status_refuses_cwd_detection`.
async fn resolve_index_id(
    client: &DaemonClient,
    cli_index: &Option<String>,
    cwd: &Path,
) -> Result<String> {
    if let Some(id) = cli_index {
        return Ok(id.clone());
    }

    // Auto-detect from CWD.
    let canonical_cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let ids = super::daemon_rpc::registrations(client).await?.resident;
    // #9214: H1 — a failed lookup refuses; it used to `continue`.
    let read = super::daemon_rpc::resident_statuses(client, ids)
        .await
        .require_all("refusing to guess which index owns the current directory")?;
    for status in read {
        let canonical_root =
            std::fs::canonicalize(&status.root).unwrap_or_else(|_| status.root.clone());
        if canonical_cwd.starts_with(&canonical_root) {
            return Ok(status.id);
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
    #[tokio::test]
    async fn resolve_index_id_uses_explicit_arg() {
        // The explicit-id branch returns before any daemon call, so a socket
        // nothing serves is never dialled.
        let dir = tempfile::tempdir().expect("scratch dir");
        let client = DaemonClient::at(dir.path().join("absent.sock"));
        let id = resolve_index_id(&client, &Some("my-index".to_string()), dir.path())
            .await
            .expect("explicit id");
        assert_eq!(id, "my-index");
    }

    /// #9214 H1: CWD detection with one status refused must fail naming that
    /// index, not skip it and report "no index registered" or pick another.
    #[tokio::test]
    async fn an_unreadable_status_refuses_cwd_detection() {
        let owner = tempfile::tempdir().expect("owner root");
        let owner_root = owner.path().to_string_lossy().into_owned();
        let daemon = crate::commands::mock_socket::mock_daemon(move |method, params| {
            match (method, params["index_id"].as_str()) {
                ("search.indexes.list", _) => Ok(serde_json::json!({ "indexes": ["owner"] })),
                ("search.index.status", Some("owner")) => {
                    Err(trusty_common::uds::server::RpcError::internal(format!(
                        "status unavailable for {owner_root}"
                    )))
                }
                _ => Err(trusty_common::uds::server::RpcError::internal("unexpected")),
            }
        })
        .await;
        let err = resolve_index_id(&daemon.client, &None, owner.path())
            .await
            .expect_err("an unreadable status must refuse")
            .to_string();
        assert!(err.contains("\"owner\""), "{err}");
        assert!(!err.contains("no index registered"), "{err}");
    }
}
