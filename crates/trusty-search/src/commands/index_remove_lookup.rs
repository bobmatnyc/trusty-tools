//! Resolve `index remove`'s target against the daemon (split from
//! `index_remove.rs`, #8687).
//!
//! Why: `GET /indexes` lists resident indexes only, and `GET /indexes/:id/status`
//! answers `503 index_not_resident` for a cold-parked one. Resolving through
//! those alone made removing index X depend on residency: a parked X could not
//! be found by PATH or read by id, and a PATH whose registration was already
//! deleted aborted before its stale `allowlist.toml` row was cleaned.
//! What: [`find_index_by_id`] and [`find_index_by_path`], both reading the
//! `parked` rows `GET /indexes` now carries (#8727), plus
//! [`clear_stale_path_rows`] for a PATH the daemon no longer registers.
//! Test: `tests/index_remove_residency_8687.rs`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::config::GlobalConfig;

/// What `find_index_by_path` learned about PATH.
#[derive(Debug)]
pub(crate) enum PathLookup {
    /// A registration — resident or parked — owns PATH.
    Found(String, PathBuf),
    /// Every registration was read, and none owns PATH.
    NotRegistered,
}

/// `GET /indexes`: the resident ids and the parked `(id, root)` rows.
async fn list_registrations(
    client: &reqwest::Client,
    base: &str,
) -> Result<(Vec<String>, Vec<(String, PathBuf)>)> {
    let list_url = format!("{base}/indexes");
    let body: Value = client
        .get(&list_url)
        .send()
        .await
        .with_context(|| format!("could not reach daemon at {base}"))?
        .error_for_status()
        .with_context(|| format!("daemon error for {list_url}"))?
        .json()
        .await
        .context("could not parse /indexes response")?;
    let strings = |v: Option<&Value>| v.and_then(Value::as_array).cloned().unwrap_or_default();
    let ids = strings(body.get("indexes"))
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let parked = strings(body.get("parked"))
        .iter()
        .filter_map(|row| {
            let id = row.get("id")?.as_str()?.to_string();
            Some((id, PathBuf::from(row.get("root_path")?.as_str()?)))
        })
        .collect();
    Ok((ids, parked))
}

/// Fetch the registered `root_path` for a known index id.
///
/// Why (#1087): `-i <id>` names the id; the root is still needed for the
/// post-delete cleanup. #8687: a cold-parked id answers its status with `503`,
/// so its root is read from the `parked` rows instead.
/// What: `GET /indexes/:id/status`; on a non-success status, the id's parked
/// row; otherwise the status error.
/// Test: `removing_a_cold_parked_index_beside_a_cold_neighbour_succeeds`.
pub(crate) async fn find_index_by_id(
    client: &reqwest::Client,
    base: &str,
    id: &str,
) -> Result<(String, PathBuf)> {
    let url = format!("{base}/indexes/{id}/status");
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("could not reach daemon at {base}"))?;
    if let Err(status_err) = resp.error_for_status_ref() {
        let (_, parked) = list_registrations(client, base).await?;
        if let Some((_, root)) = parked.into_iter().find(|(p, _)| p == id) {
            return Ok((id.to_string(), root));
        }
        return Err(status_err).with_context(|| format!("daemon returned an error for {url}"));
    }
    let body: Value = resp
        .json()
        .await
        .context("could not parse status response")?;
    let root = body
        .get("root_path")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .with_context(|| format!("status response for '{id}' is missing root_path"))?;
    Ok((id.to_string(), root))
}

/// Find the registration — resident or parked — whose root is `target`.
///
/// Why: the CLI takes a path, the daemon is keyed by id. #8687: a parked
/// registration is matched from its `parked` row with no status call, and a
/// status lookup that fails for an unrelated index is only fatal when nothing
/// matched — then it is fail-closed: PATH is not reported unregistered while
/// some registration could not be read.
/// What: `Found` on a canonical-path match; `NotRegistered` when every
/// registration was read and none matched; `Err` naming the unreadable ids.
/// A `404` status means the id was deleted since the list, so it is skipped.
/// Test: `removing_a_cold_parked_index_beside_a_cold_neighbour_succeeds`,
/// `removing_an_already_deleted_index_clears_its_stale_allowlist_row`.
pub(crate) async fn find_index_by_path(
    client: &reqwest::Client,
    base: &str,
    target: &Path,
) -> Result<PathLookup> {
    let (ids, parked) = list_registrations(client, base).await?;
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let canonical_target = canonical(target);
    if let Some((id, root)) = parked
        .into_iter()
        .find(|(_, root)| canonical(root) == canonical_target)
    {
        return Ok(PathLookup::Found(id, root));
    }
    let mut unreadable = Vec::new();
    for id in ids {
        let url = format!("{base}/indexes/{id}/status");
        let body: Value = match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => match r.json().await {
                Ok(b) => b,
                Err(_) => {
                    unreadable.push(id);
                    continue;
                }
            },
            Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => continue,
            _ => {
                unreadable.push(id);
                continue;
            }
        };
        let Some(root) = body.get("root_path").and_then(|v| v.as_str()) else {
            unreadable.push(id);
            continue;
        };
        let root = PathBuf::from(root);
        if canonical(&root) == canonical_target {
            return Ok(PathLookup::Found(id, root));
        }
    }
    if !unreadable.is_empty() {
        bail!(
            "no readable index is registered for path {}, and the status of [{}] could not \
             be read, so it cannot be ruled out; retry once the daemon answers for them",
            target.display(),
            unreadable.join(", ")
        );
    }
    Ok(PathLookup::NotRegistered)
}

/// Drop PATH's rows from the allowlist and the global config when the daemon
/// no longer registers it (#8687).
///
/// Why: an index deleted through another surface (MCP `delete_index`) left its
/// `allowlist.toml` row behind, and `index remove <PATH>` aborted before the
/// cleanup that would clear it.
/// What: removes each file's row for PATH, saving only a file that changed.
/// Returns how many rows went; `0` means there was nothing to clean.
/// Test: `removing_an_already_deleted_index_clears_its_stale_allowlist_row`.
pub(crate) fn clear_stale_path_rows(path: &Path) -> Result<usize> {
    let mut cleared = 0;
    let allowlist_path = crate::allowlist::AllowlistConfig::default_path();
    let mut allowlist = crate::allowlist::AllowlistConfig::load_from(&allowlist_path)?;
    if allowlist.remove(path).is_some() {
        allowlist.save_to(&allowlist_path)?;
        cleared += 1;
    }
    let mut cfg = GlobalConfig::load()?;
    if cfg.remove_collection_by_path(path).is_some() {
        cfg.save()?;
        cleared += 1;
    }
    Ok(cleared)
}
