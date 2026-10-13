//! `tm doctor` row for the per-owner launch config (#8988 PR 1a).
//!
//! Why: a bad owner file never refuses a launch — it falls back to the last
//! good policy — so the only loud signal is this row.
//! What: [`check_owner_config`] reads [`crate::core::launch_policy::resolve`]
//! for the project and writes nothing. `Fail` for a bad file or a broken
//! `extends` chain, naming the last-good record in use or "no last good";
//! `Warn` when an `allow`/`deny` name matches no user-scope or builtin server;
//! `Ok` otherwise, including "none" when there is no owner or no file.
//! Test: `tests/owner_config_8988.rs` and the `tests` module below.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::launch_policy::compose::OwnerPolicy;
use crate::core::launch_policy::{Outcome, OwnerInputs, resolve};

/// The row name.
///
/// Test: `doctor_checks_match_run_doctor_names`.
pub(super) const CHECK_NAME: &str = "owner_config";

/// The row against the host's repos root, state root and managed config dir.
///
/// Test: `tests/owner_config_8988.rs`.
pub(super) fn check_owner_config(project_dir: Option<&Path>) -> DoctorCheck {
    let repos_root = crate::core::repo_paths::repos_root();
    let state_root = crate::core::launch_policy::state_root();
    let config_dir = crate::core::trusty_tools_config::managed_claude_config_dir();
    match (project_dir, state_root) {
        (Some(cwd), Some(state_root)) => check_owner_config_at(
            &OwnerInputs {
                cwd,
                repos_root: &repos_root,
                state_root: &state_root,
            },
            config_dir.as_deref(),
        ),
        (None, _) => ok("no project directory supplied — no owner layer to check"),
        (_, None) => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            "no home directory — the owner last-good store cannot be located",
        ),
    }
}

/// [`check_owner_config`] over explicit inputs.
///
/// Test: `owner_config_warns_on_a_name_no_server_carries`,
/// `owner_config_is_ok_and_names_the_chain`, `owner_config_is_ok_without_an_owner`.
pub(super) fn check_owner_config_at(
    inputs: &OwnerInputs<'_>,
    config_dir: Option<&Path>,
) -> DoctorCheck {
    let resolution = resolve(inputs);
    let (Some(owner), Some(file)) = (&resolution.owner, &resolution.file) else {
        return ok(format!(
            "none: {} belongs to no owner",
            inputs.cwd.display()
        ));
    };
    let file = file.display();
    match &resolution.outcome {
        Outcome::NoOwner => ok(format!(
            "none: {} belongs to no owner",
            inputs.cwd.display()
        )),
        Outcome::NoFile { retained, .. } => {
            let mut msg = format!("none: owner `{owner}` has no {file}; owner layer off");
            if let Some(record) = retained {
                msg.push_str(&format!(
                    " (a last good policy from {} is retained but unused)",
                    record.recorded_at
                ));
            }
            ok(msg)
        }
        Outcome::Loaded(chain) => {
            let summary = format!(
                "owner `{owner}` ({}): {}; applied at each launch via --settings; a launch that cannot write it warns (daemon log for daemon spawns) and runs unfiltered (#8988)",
                chain.chain.join(" → "),
                describe(&chain.policy)
            );
            match unknown_names(&chain.policy, config_dir) {
                Some(unknown) if !unknown.is_empty() => DoctorCheck::new(
                    CHECK_NAME,
                    CheckStatus::Warn,
                    format!(
                        "{summary}; name(s) matching no user-scope or builtin server: {}",
                        unknown.join(", ")
                    ),
                ),
                _ => ok(summary),
            }
        }
        Outcome::Fallback { error, last_good } => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            format!(
                "owner config {file} INVALID ({error}); the owner layer falls back to the last \
                 good policy from {} (sha {}): {}",
                last_good.recorded_at,
                last_good
                    .source_sha256
                    .get(..12)
                    .unwrap_or(&last_good.source_sha256),
                describe(&last_good.policy)
            ),
        ),
        Outcome::Skipped {
            error,
            last_good_error,
        } => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            format!(
                "owner config {file} INVALID ({error}); no last good policy{}, owner layer skipped",
                last_good_error
                    .as_ref()
                    .map(|r| format!(" usable ({r})"))
                    .unwrap_or_default()
            ),
        ),
    }
}

fn ok(message: impl Into<String>) -> DoctorCheck {
    DoctorCheck::new(CHECK_NAME, CheckStatus::Ok, message)
}

/// One line naming a policy's keys.
fn describe(policy: &OwnerPolicy) -> String {
    let list = |set: &std::collections::BTreeSet<String>| {
        set.iter().cloned().collect::<Vec<_>>().join(", ")
    };
    let allow = match &policy.allow {
        None => "all".to_string(),
        Some(set) => format!("[{}]", list(set)),
    };
    let connectors = match policy.claude_ai_connectors {
        Some(false) => "off",
        _ => "default",
    };
    format!(
        "allow {allow}, deny [{}], claude.ai connectors {connectors}",
        list(&policy.deny)
    )
}

/// `allow`/`deny` names that match no user-scope or builtin server; `None` when
/// the user scope cannot be read.
fn unknown_names(policy: &OwnerPolicy, config_dir: Option<&Path>) -> Option<Vec<String>> {
    let known = crate::core::session_mcp_scope::user_scope_servers(config_dir?).ok()?;
    let names = policy.allow.iter().flatten().chain(&policy.deny);
    let mut unknown: Vec<String> = names
        .filter(|n| !known.contains(n) && !crate::core::launch_policy::floor::is_floor(n))
        .cloned()
        .collect();
    unknown.sort();
    unknown.dedup();
    Some(unknown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct Fx {
        _tmp: TempDir,
        repos: std::path::PathBuf,
        state: std::path::PathBuf,
        cfg: std::path::PathBuf,
        project: std::path::PathBuf,
    }

    fn fx(owner_toml: Option<&str>) -> Fx {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let repos = base.join("repos");
        let project = repos.join("acme").join("widget");
        std::fs::create_dir_all(&project).unwrap();
        if let Some(body) = owner_toml {
            std::fs::write(repos.join("acme").join(".trusty-mpm-owner.toml"), body).unwrap();
        }
        let cfg = base.join("cfg");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(
            cfg.join(".claude.json"),
            r#"{"mcpServers":{"apex":{},"heygen":{}}}"#,
        )
        .unwrap();
        Fx {
            state: base.join("state"),
            _tmp: tmp,
            repos,
            cfg,
            project,
        }
    }

    fn run(f: &Fx) -> DoctorCheck {
        check_owner_config_at(
            &OwnerInputs {
                cwd: &f.project,
                repos_root: &f.repos,
                state_root: &f.state,
            },
            Some(&f.cfg),
        )
    }

    #[test]
    fn owner_config_is_ok_and_names_the_chain() {
        let f = fx(Some("[mcp]\nallow = [\"apex\"]\ndeny = [\"heygen\"]\n"));
        let row = run(&f);
        assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
        assert!(
            row.message.contains("owner `acme` (acme)"),
            "{}",
            row.message
        );
        assert!(row.message.contains("allow [apex]"), "{}", row.message);
    }

    #[test]
    fn owner_config_warns_on_a_name_no_server_carries() {
        let f = fx(Some("[mcp]\ndeny = [\"notion\"]\n"));
        let row = run(&f);
        assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
        assert!(row.message.contains("notion"), "{}", row.message);
    }

    #[test]
    fn owner_config_is_ok_without_an_owner() {
        let f = fx(None);
        let row = check_owner_config_at(
            &OwnerInputs {
                cwd: &f.repos,
                repos_root: &f.repos,
                state_root: &f.state,
            },
            Some(&f.cfg),
        );
        assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
        assert!(
            row.message.contains("belongs to no owner"),
            "{}",
            row.message
        );
    }
}
