//! `tm repair delegation --list [DIR]` — the read-only delegation listing (#8257).
//!
//! Why: the dispatch guard denied on records nobody could enumerate; the only
//! reads of the map were POSTs that claim a directory. This prints what the
//! daemon holds for one directory and writes nothing.
//! What: canonicalizes DIR (the form `tm hook` stamps on a record), GETs
//! `/api/v1/delegations?cwd=`, and prints one line per record with the same
//! fields the dispatch deny names.
//! Test: `listing_line_names_every_field_8257`,
//! `listing_dir_is_absolute_for_a_missing_relative_dir_8257`,
//! `listing_dir_refuses_a_path_it_cannot_resolve_8257`; the route in
//! `list_route_names_the_blocking_record_8257`.

use std::path::{Path, PathBuf};

use trusty_mpm::daemon::services::delegation_records::{DelegationListing, DelegationRecordView};

/// Fetch and print the listing for `dir`.
pub(crate) async fn list(client: &reqwest::Client, url: &str, dir: &Path) -> anyhow::Result<()> {
    let dir = listing_dir(dir)?;
    let resp = client
        .get(format!("{url}/api/v1/delegations"))
        .query(&[("cwd", dir.display().to_string())])
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("listing delegations failed ({status}): {body}");
    }
    let listing: DelegationListing = serde_json::from_str(&body)
        .map_err(|e| anyhow::anyhow!("could not read the daemon's answer ({e}): {body}"))?;
    if listing.records.is_empty() {
        println!("no live delegation records in {}", dir.display());
        return Ok(());
    }
    println!("live delegation records in {}:", dir.display());
    for record in &listing.records {
        println!("{}", listing_line(record));
    }
    Ok(())
}

/// The directory to ask the daemon about, in the canonical form `tm hook`
/// stamps on a record (#8257).
///
/// Why: the daemon matches `cwd` exactly, so a relative or unresolved path
/// matches no record, and the listing would print "no live delegation
/// records" for a directory it never asked about.
/// What: the canonical path when `dir` exists. When it does not — a removed
/// worktree whose record outlives it — the deepest existing ancestor,
/// canonicalized, joined with the missing tail. `Err` when the path cannot be
/// resolved for any reason other than its absence.
/// Test: `listing_dir_is_absolute_for_a_missing_relative_dir_8257`,
/// `listing_dir_canonicalizes_the_existing_ancestor_8257`,
/// `listing_dir_refuses_a_path_it_cannot_resolve_8257`.
fn listing_dir(dir: &Path) -> anyhow::Result<PathBuf> {
    let absolute = std::path::absolute(dir)
        .map_err(|e| anyhow::anyhow!("could not resolve {}: {e}", dir.display()))?;
    let mut tail = Vec::new();
    let mut at = absolute.as_path();
    loop {
        match std::fs::canonicalize(at) {
            Ok(base) => return Ok(tail.iter().rev().fold(base, |p, c| p.join(c))),
            // #8257: only absence walks up; any other failure is not a listing.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => anyhow::bail!("could not resolve {}: {e}", dir.display()),
        }
        let (Some(parent), Some(name)) = (at.parent(), at.file_name()) else {
            return Ok(absolute);
        };
        tail.push(name.to_os_string());
        at = parent;
    }
}

/// One record as a listing line — type, ids, owner, age, clearing command.
fn listing_line(r: &DelegationRecordView) -> String {
    let status = serde_json::to_value(r.status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    format!(
        "  {} [{status}{}] agent id {} delegation {} owner {} age {}m tree {} — {}",
        r.agent,
        if r.blocks_dispatch {
            ", blocks dispatch"
        } else {
            ""
        },
        r.agent_id.as_deref().unwrap_or("none"),
        r.delegation_id,
        r.owner,
        r.age_secs.max(0) / 60,
        r.worktree_path
            .as_deref()
            .map_or("none".to_string(), |p| p.display().to_string()),
        r.repair_command
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::core::agent::DelegationStatus;

    #[test]
    fn listing_line_names_every_field_8257() {
        let line = listing_line(&DelegationRecordView {
            delegation_id: "64237d1c-0aa4-4090-bd14-d3c273da7e95".to_string(),
            agent: "version-control".to_string(),
            agent_id: None,
            owner: "session `tm-trusty-tools`".to_string(),
            status: DelegationStatus::Stale,
            age_secs: 600,
            cwd: None,
            worktree_path: None,
            blocks_dispatch: true,
            repair_command:
                "tm repair delegation --delegation-id 64237d1c-0aa4-4090-bd14-d3c273da7e95"
                    .to_string(),
        });
        for want in [
            "version-control [stale, blocks dispatch]",
            "agent id none",
            "delegation 64237d1c-0aa4-4090-bd14-d3c273da7e95",
            "owner session `tm-trusty-tools`",
            "age 10m",
            "— tm repair delegation --delegation-id 64237d1c",
        ] {
            assert!(line.contains(want), "missing {want:?}: {line}");
        }
    }

    // #8257: a relative path that no longer exists still asks the daemon about
    // the canonical absolute directory a record would carry, never about ".".
    #[test]
    fn listing_dir_is_absolute_for_a_missing_relative_dir_8257() {
        let rel = Path::new("no-such-dir-8257/removed-tree");
        let resolved = listing_dir(rel).expect("a missing dir still resolves");
        assert!(resolved.is_absolute(), "{}", resolved.display());
        assert!(resolved.ends_with(rel), "{}", resolved.display());
    }

    // #8257: the missing tail hangs off the canonical form of the deepest
    // directory that exists, the form a record's `cwd` was stamped in.
    #[test]
    fn listing_dir_canonicalizes_the_existing_ancestor_8257() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("gone").join("tree");
        let resolved = listing_dir(&missing).expect("resolves");
        let base = std::fs::canonicalize(dir.path()).expect("canonical");
        assert_eq!(resolved, base.join("gone").join("tree"));
    }

    // #8257: a path that fails for a reason other than absence is an error, not
    // an empty listing for an unresolved path.
    #[test]
    fn listing_dir_refuses_a_path_it_cannot_resolve_8257() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("a-file");
        std::fs::write(&file, b"x").expect("write");
        let through_a_file = file.join("sub");
        let err = listing_dir(&through_a_file).expect_err("a path through a file");
        assert!(err.to_string().contains("could not resolve"), "{err}");
    }
}
