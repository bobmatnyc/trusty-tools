//! Doctor probe: agent files in the shared deploy dir the bundle dropped (#8992).
//!
//! Why: the deploy path never deletes, so an agent removed from the bundle
//! stays in the one dir every PM reads and keeps reaching every agent listing.
//! `deploy_validate` checks that bundled agents are present; nothing reported
//! the reverse until this row.
//! What: [`check_agent_orphans`] runs the read-only
//! [`scan_orphan_agents`] — the classifier `tm doctor --fix-agents` deletes
//! by — and names each orphan as removable or kept, with the reason. It never
//! writes.
//! Test: `doctor_agent_orphans_tests` below.

use std::collections::BTreeSet;
use std::path::Path;

use crate::core::agent_orphans::{
    AgentRemoval, CHECK, OrphanAgent, resolve_live_agent_names, scan_orphan_agents,
};
use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::paths::FrameworkPaths;

/// The command that removes what this row reports.
const FIX_HINT: &str = "tm doctor --fix-agents --yes";

/// Report orphan agent files in [`FrameworkPaths::agent_deploy_dir`].
///
/// What: resolves the live agent set, then delegates to [`orphan_verdict`].
/// An unresolvable roster is a `Warn`, never a clean pass.
/// Test: `orphan_verdict` carries the coverage.
pub(super) fn check_agent_orphans(paths: &FrameworkPaths) -> DoctorCheck {
    match resolve_live_agent_names(paths) {
        Ok(live) => orphan_verdict(&paths.agent_deploy_dir(), &live),
        Err(why) => DoctorCheck::new(
            CHECK,
            CheckStatus::Warn,
            format!("cannot classify the shared agent dir: {why}"),
        ),
    }
}

/// The row for `dir` against `live`.
///
/// Why: split from [`check_agent_orphans`] so tests drive a temp dir and a
/// literal live set rather than the host's real deploy dir.
/// What: `Warn` when a removable orphan exists (naming the fix command) or the
/// scan fails closed; `Ok` otherwise, still naming every kept orphan and why.
/// Test: `a_removable_orphan_warns_and_names_the_fix`,
/// `kept_orphans_are_reported_without_warning`, `a_corrupt_ledger_warns`.
fn orphan_verdict(dir: &Path, live: &BTreeSet<String>) -> DoctorCheck {
    let orphans = match scan_orphan_agents(dir, live) {
        Ok(orphans) => orphans,
        Err(why) => return DoctorCheck::new(CHECK, CheckStatus::Warn, why),
    };
    if orphans.is_empty() {
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Ok,
            format!("no orphan agent files in {}", dir.display()),
        );
    }
    let (removable, kept): (Vec<&OrphanAgent>, Vec<&OrphanAgent>) = orphans
        .iter()
        .partition(|o| o.verdict == AgentRemoval::Removable);
    let mut lines = Vec::new();
    if !removable.is_empty() {
        let names: Vec<&str> = removable.iter().map(|o| o.file_name.as_str()).collect();
        lines.push(format!(
            "{} tm-deployed agent file(s) the bundle no longer ships: {} — run `{FIX_HINT}`",
            removable.len(),
            names.join(", ")
        ));
    }
    for orphan in &kept {
        if let AgentRemoval::Kept(why) = &orphan.verdict {
            lines.push(format!("skipped {}: {why}", orphan.file_name));
        }
    }
    let status = if removable.is_empty() {
        CheckStatus::Ok
    } else {
        CheckStatus::Warn
    };
    DoctorCheck::new(CHECK, status, lines.join("; "))
}

#[cfg(test)]
mod doctor_agent_orphans_tests {
    use super::*;
    use tempfile::TempDir;
    use trusty_agents_common::agents::manifest::{
        AgentManifest, MANIFEST_FILE, ManifestEntry, Origin, checksum,
    };

    fn live() -> BTreeSet<String> {
        BTreeSet::from(["engineer".to_owned()])
    }

    fn deploy(dir: &Path, file_name: &str, body: &str, recorded: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(file_name), body).unwrap();
        let mut manifest = AgentManifest::load(dir);
        manifest.managed.insert(
            file_name.to_owned(),
            ManifestEntry {
                source_chain: vec![],
                checksum: checksum(recorded),
                deployed_at: "2026-08-01T14:36:49Z".to_owned(),
                origin: Origin::Bundled,
            },
        );
        manifest.save(dir).unwrap();
    }

    #[test]
    fn a_removable_orphan_warns_and_names_the_fix() {
        let root = TempDir::new().unwrap();
        deploy(root.path(), "engineer.md", "e", "e");
        deploy(root.path(), "ops.md", "o", "o");

        let check = orphan_verdict(root.path(), &live());

        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        assert!(check.message.contains("ops.md") && check.message.contains(FIX_HINT));
        assert!(root.path().join("ops.md").is_file(), "doctor never deletes");
    }

    #[test]
    fn kept_orphans_are_reported_without_warning() {
        let root = TempDir::new().unwrap();
        deploy(root.path(), "engineer.md", "e", "e");
        deploy(root.path(), "ops.md", "edited", "o");

        let check = orphan_verdict(root.path(), &live());

        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
        assert!(
            check.message.contains("skipped ops.md"),
            "{}",
            check.message
        );
        assert!(check.message.contains("edited"), "{}", check.message);
    }

    #[test]
    fn a_corrupt_ledger_warns() {
        let root = TempDir::new().unwrap();
        deploy(root.path(), "ops.md", "o", "o");
        std::fs::write(root.path().join(MANIFEST_FILE), "nope").unwrap();

        let check = orphan_verdict(root.path(), &live());

        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    }
}
