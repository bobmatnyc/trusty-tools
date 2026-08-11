//! `tm doctor` stray-`.mcp.json` probe.
//!
//! Why: Claude Code discovers `.mcp.json` by walking UP from a session's cwd,
//! so one written above real projects silently configures the MCP servers of
//! every session started beneath it — with no error, no log line, and nothing
//! in the project to point at. The file that motivated this probe sat at
//! `/private/tmp/.mcp.json` and was therefore read by every session whose cwd
//! was anywhere under `/tmp`, agent scratchpads included. Nothing surfaced it;
//! it was found by looking.
//!
//! What: [`check_stray_mcp_json`] reports every `.mcp.json` in the bounded scan
//! set (see [`crate::core::stray_mcp`]), each with what tm can prove about who
//! wrote it. `Warn`, never `Fail`: a stray file is a configuration the operator
//! may well want, and the probe cannot tell an accident from a deliberate
//! placement — which is the same uncertainty that stops the repair deleting
//! anything. Read-only; the remediation is the opt-in
//! `tm doctor --fix` / `--quarantine-mcp` path.
//! Test: the `tests` module below.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::mcp_provenance::{self, Provenance};
use crate::core::stray_mcp::{self, CHECK_NAME};

/// Probe the workspace's ancestors and the temp roots for stray `.mcp.json`s.
///
/// Why: see the module doc. It shares [`stray_mcp::scan`] with the repair so a
/// reported finding and a repaired one can never be different sets.
/// What: `Ok` when the scan set holds none. Otherwise `Warn`, naming every path,
/// the servers it declares, and its provenance verdict — so the operator can
/// see at a glance which findings the repair will act on and which it will
/// refuse.
/// Test: `stray_mcp_ok_when_clean`, `stray_mcp_warns_and_names_the_file`,
/// `stray_mcp_reports_provenance_per_file`,
/// `stray_mcp_ignores_the_workspaces_own_file`.
pub(super) fn check_stray_mcp_json(
    framework_root: &Path,
    project_dir: Option<&Path>,
    home: &Path,
) -> DoctorCheck {
    let ledger = mcp_provenance::load(framework_root);
    let found = stray_mcp::scan(project_dir, home, &ledger);
    if found.is_empty() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            "no stray .mcp.json above the workspace or in the temp roots",
        );
    }

    let detail: Vec<String> = found
        .iter()
        .map(|f| {
            let servers = if f.servers.is_empty() {
                "unreadable".to_string()
            } else {
                f.servers.join(", ")
            };
            format!(
                "{} ({}) [{}]",
                f.path.display(),
                servers,
                describe(&f.provenance)
            )
        })
        .collect();

    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Warn,
        format!(
            "{} .mcp.json file(s) above this workspace are read by every session whose cwd is \
             beneath them: {} — `tm doctor --fix` quarantines the ones tm can prove it wrote; \
             the rest need `tm doctor --quarantine-mcp <path>` after you have checked them",
            detail.len(),
            detail.join("; ")
        ),
    )
}

/// One-word provenance verdict for the report line.
///
/// Why: the operator's next action differs per verdict, so the report says
/// which it is rather than leaving them to run the repair to find out.
/// Test: `stray_mcp_reports_provenance_per_file`.
fn describe(p: &Provenance) -> &'static str {
    match p {
        Provenance::TmWritten => "written by tm — repairable",
        Provenance::TmWrittenThenEdited => "written by tm, edited since — refused",
        Provenance::Unattributed => "no tm record — refused",
        Provenance::Unknown(_) => "provenance undetermined — refused",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::mcp_config::MCP_JSON;

    /// A `.mcp.json` declaring `names`, written at `dir`.
    fn write_mcp(dir: &Path, names: &[&str]) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let servers: serde_json::Map<String, serde_json::Value> = names
            .iter()
            .map(|n| {
                (
                    (*n).to_string(),
                    serde_json::json!({"type": "stdio", "command": n}),
                )
            })
            .collect();
        let path = dir.join(MCP_JSON);
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&serde_json::json!({"mcpServers": servers})).unwrap(),
        )
        .unwrap();
        path
    }

    /// A home/workspace pair where the temp roots hold no `.mcp.json`.
    ///
    /// The temp roots are always in the scan set, and on a developer machine
    /// `/tmp/.mcp.json` may genuinely exist — these tests must not depend on
    /// that, so every assertion below is scoped to paths under the tempdir.
    fn strays_under(root: &Path, checks: &DoctorCheck) -> bool {
        checks.message.contains(&root.display().to_string())
    }

    #[test]
    fn stray_mcp_ok_when_clean() {
        // A workspace whose ancestors carry no `.mcp.json` must never nag.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let workspace = home.join("projects").join("thing");
        std::fs::create_dir_all(&workspace).unwrap();
        let check = check_stray_mcp_json(&tmp.path().join(".trusty-mpm"), Some(&workspace), &home);
        assert!(
            !strays_under(tmp.path(), &check),
            "clean ancestors must produce no finding under the test root: {}",
            check.message
        );
    }

    #[test]
    fn stray_mcp_warns_and_names_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let workspace = home.join("projects").join("thing");
        std::fs::create_dir_all(&workspace).unwrap();
        let stray = write_mcp(&home.join("projects"), &["trusty-search"]);

        let check = check_stray_mcp_json(&tmp.path().join(".trusty-mpm"), Some(&workspace), &home);
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(
            check.message.contains(&stray.display().to_string()),
            "the report must name the exact path: {}",
            check.message
        );
        assert!(
            check.message.contains("trusty-search"),
            "the report must name the servers so the operator can recognise the file: {}",
            check.message
        );
    }

    #[test]
    fn stray_mcp_reports_provenance_per_file() {
        // Without a ledger record the verdict must be the REFUSING one — the
        // operator must not read the report as "tm will clean this up".
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let workspace = home.join("projects").join("thing");
        std::fs::create_dir_all(&workspace).unwrap();
        write_mcp(&home.join("projects"), &["apex"]);

        let check = check_stray_mcp_json(&tmp.path().join(".trusty-mpm"), Some(&workspace), &home);
        assert!(
            check.message.contains("no tm record — refused"),
            "an unattributed file must be reported as refused: {}",
            check.message
        );
    }

    #[test]
    fn stray_mcp_ignores_the_workspaces_own_file() {
        // The project's own managed `.mcp.json` is not a stray, and reporting
        // it would train the operator to ignore this check.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let workspace = home.join("projects").join("thing");
        let own = write_mcp(&workspace, &["trusty-mpm"]);

        let check = check_stray_mcp_json(&tmp.path().join(".trusty-mpm"), Some(&workspace), &home);
        assert!(
            !check.message.contains(&own.display().to_string()),
            "the workspace's own .mcp.json must never be reported: {}",
            check.message
        );
    }
}
