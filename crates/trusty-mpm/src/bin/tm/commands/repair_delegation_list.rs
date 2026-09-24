//! `tm repair delegation --list [DIR]` — the read-only delegation listing (#8257).
//!
//! Why: the dispatch guard denied on records nobody could enumerate; the only
//! reads of the map were POSTs that claim a directory. This prints what the
//! daemon holds for one directory and writes nothing.
//! What: canonicalizes DIR (the form `tm hook` stamps on a record), GETs
//! `/api/v1/delegations?cwd=`, and prints one line per record with the same
//! fields the dispatch deny names.
//! Test: `listing_line_names_every_field_8257`; the route in
//! `list_route_names_the_blocking_record_8257`.

use std::path::Path;

use trusty_mpm::daemon::services::delegation_records::{DelegationListing, DelegationRecordView};

/// Fetch and print the listing for `dir`.
pub(crate) async fn list(client: &reqwest::Client, url: &str, dir: &Path) -> anyhow::Result<()> {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
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
}
