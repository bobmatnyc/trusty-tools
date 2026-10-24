//! The `session_target_dir` row of `tm doctor` (#10215).
//!
//! Why: agent sessions in tm worktrees built into the worktree's own `target/`
//! — a cold 3.6-6.7 GB build each — while the `rust_build_env` row stayed
//! green, because it checks only that the shared directory exists. Nothing
//! reported the directory a session's builds actually land in.
//! What: reads THIS process's `CARGO_TARGET_DIR` (the session shell that runs
//! `tm doctor`), resolves it the way `tm build-lease` does
//! ([`plan_with_worktree`]), and lists every linked worktree of the project
//! that holds a `target/` of its own. WARN, with the fix, when the effective
//! directory is cargo's `./target` or lies inside a checkout of this repo, or
//! when any worktree holds a local `target/`. A probe that fails is UNKNOWN,
//! never OK; a non-Rust project is a skip.
//! Test: `doctor_session_target_dir_tests.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;

use trusty_mpm::core::build_lease::target_dir::{
    SharedDirs, TargetPlan, plan_with_worktree, resolve_pool,
};
use trusty_mpm::core::builders::BuildersConfig;
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};

/// This check's name.
pub(crate) const CHECK: &str = "session_target_dir";

/// The most worktree paths the row names before it summarizes the rest.
const MAX_LISTED: usize = 5;

/// Where this session's leased builds land.
///
/// Test: every test in `doctor_session_target_dir_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionTarget {
    /// A pool slot or the shared directory: the cache is reused.
    Pooled,
    /// A private directory outside every checkout of this repo.
    Private(String),
    /// A directory inside a checkout of this repo.
    CheckoutLocal(String),
    /// Unset with no slot to take: cargo builds in `<checkout>/target`.
    Unset(PathBuf),
    /// The lease refuses the build; the string says why.
    Refused(String),
}

/// Everything the verdict is folded from.
///
/// Test: `doctor_session_target_dir_tests.rs`.
#[derive(Debug, Clone)]
pub(crate) enum Facts {
    /// No project, or a project with no Rust in it.
    NotApplicable(String),
    /// A probe failed; the string says which and why.
    Undetermined(String),
    /// The session's target and the worktrees holding a local `target/`.
    Probed {
        /// Where this session's builds land.
        session: SessionTarget,
        /// Linked worktrees with a `target/` directory of their own.
        local_targets: Vec<PathBuf>,
    },
}

/// Fold the facts into the row (pure).
///
/// What: `Ok` for a skip or a pooled/private session with no worktree-local
/// `target/`; `Unknown` for an undetermined probe; otherwise `Warn`, naming
/// each problem and the fix.
/// Test: `a_pooled_session_with_no_local_targets_is_ok`,
/// `an_unset_session_target_warns_with_the_fix`,
/// `a_checkout_local_target_dir_warns`,
/// `worktrees_holding_a_local_target_warn`,
/// `an_undetermined_probe_is_unknown_not_ok`.
pub(crate) fn build_check(facts: &Facts) -> DoctorCheck {
    let (session, local_targets) = match facts {
        Facts::NotApplicable(why) => {
            return DoctorCheck::new(CHECK, CheckStatus::Ok, format!("{why} — not applicable"));
        }
        Facts::Undetermined(why) => {
            return DoctorCheck::new(
                CHECK,
                CheckStatus::Unknown,
                format!(
                    "where this session's builds land is UNDETERMINED: {why} (#10215). Builds \
                     may be landing in a worktree-local target/"
                ),
            );
        }
        Facts::Probed {
            session,
            local_targets,
        } => (session, local_targets),
    };
    let mut problems = Vec::new();
    match session {
        SessionTarget::Pooled | SessionTarget::Private(_) => {}
        SessionTarget::Unset(checkout) => problems.push(format!(
            "CARGO_TARGET_DIR is unset and no pool slot applies, so builds land in {}",
            checkout.join("target").display()
        )),
        SessionTarget::CheckoutLocal(dir) => problems.push(format!(
            "CARGO_TARGET_DIR={dir} lies inside a checkout of this repo"
        )),
        SessionTarget::Refused(why) => {
            problems.push(format!(
                "the build lease refuses this session's builds: {why}"
            ));
        }
    }
    if !local_targets.is_empty() {
        let listed: Vec<String> = local_targets
            .iter()
            .take(MAX_LISTED)
            .map(|p| p.join("target").display().to_string())
            .collect();
        let more = local_targets.len().saturating_sub(MAX_LISTED);
        problems.push(format!(
            "{} worktree(s) hold a worktree-local target/: {}{}",
            local_targets.len(),
            listed.join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        ));
    }
    if problems.is_empty() {
        let detail = match session {
            SessionTarget::Private(dir) => format!("a private CARGO_TARGET_DIR={dir}"),
            _ => "a build-lease pool slot or the shared target directory".to_string(),
        };
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Ok,
            format!("this session's builds land in {detail}; no worktree holds a local target/"),
        );
    }
    DoctorCheck::new(
        CHECK,
        CheckStatus::Warn,
        format!(
            "{} (#10215). Fix: run cargo directly in the tm session so the build-lease hook \
             gives a linked worktree a pool slot, or export the CARGO_TARGET_DIR the \
             rust_build_env row prints; once no build runs in a listed target/, remove it or \
             reclaim merged worktrees with `tm session prune-worktrees --merged-prs`",
            problems.join("; ")
        ),
    )
}

/// The live row for `project_dir`.
///
/// Test: `tm_doctor_reports_every_local_check_with_no_daemon` runs it.
pub(crate) fn session_target_dir_row(project_dir: Option<&Path>) -> DoctorCheck {
    let ambient = match std::env::var("CARGO_TARGET_DIR") {
        Ok(v) => Ok(Some(v)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(e) => Err(e.to_string()),
    };
    let builders = trusty_mpm::core::config::MpmConfig::load_default().builders;
    let Some(home) = dirs::home_dir() else {
        return build_check(&Facts::Undetermined("no home directory".to_string()));
    };
    build_check(&gather(project_dir, ambient, &builders, &home))
}

/// Gather the facts for `project_dir` with `ambient` as `CARGO_TARGET_DIR`.
///
/// What: skips a non-Rust project; `Undetermined` when the stack scan, the
/// environment value, `git worktree list` or a `target/` probe fails.
/// Test: `gather_*` in `doctor_session_target_dir_tests.rs`.
pub(crate) fn gather(
    project_dir: Option<&Path>,
    ambient: Result<Option<String>, String>,
    builders: &BuildersConfig,
    home: &Path,
) -> Facts {
    let Some(project_dir) = project_dir else {
        return Facts::NotApplicable("no project directory supplied".to_string());
    };
    match trusty_mpm::core::build_env::project_is_rust(project_dir) {
        Some(true) => {}
        Some(false) => {
            return Facts::NotApplicable("this project's stack does not include Rust".into());
        }
        None => return Facts::Undetermined("stack detection was cut short".to_string()),
    }
    let ambient = match ambient {
        Ok(v) => v,
        Err(e) => return Facts::Undetermined(format!("CARGO_TARGET_DIR is unreadable: {e}")),
    };
    let checkouts = match worktree_roots(project_dir) {
        Ok(roots) => roots,
        Err(e) => return Facts::Undetermined(format!("`git worktree list` failed: {e}")),
    };
    let linked = trusty_mpm::core::harness_root::linked_worktree_owner(project_dir).is_some();
    let plan = plan_with_worktree(
        ambient.as_deref(),
        &SharedDirs::resolve(builders, home),
        resolve_pool(builders, home, project_dir),
        linked,
    );
    let session = match plan {
        TargetPlan::Keep(dir) => {
            // #10215: compare resolved paths, so a symlink or `..` cannot hide a checkout.
            let abs = resolved(&project_dir.join(&dir));
            if checkouts.iter().any(|c| abs.starts_with(resolved(c))) {
                SessionTarget::CheckoutLocal(dir)
            } else {
                SessionTarget::Private(dir)
            }
        }
        TargetPlan::Unset => SessionTarget::Unset(project_dir.to_path_buf()),
        TargetPlan::Refuse(why) => SessionTarget::Refused(why),
        _ => SessionTarget::Pooled,
    };
    // The first entry is the main checkout; one `./target` there is not a pileup.
    let mut local_targets = Vec::new();
    for wt in checkouts.iter().skip(1) {
        match std::fs::symlink_metadata(wt.join("target")) {
            Ok(m) if m.is_dir() => local_targets.push(wt.clone()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Facts::Undetermined(format!(
                    "{} could not be read: {e}",
                    wt.join("target").display()
                ));
            }
        }
    }
    Facts::Probed {
        session,
        local_targets,
    }
}

/// `path` with symlinks and `..` resolved as far as it exists (#10215).
///
/// Why: a lexical `starts_with` reads a symlinked or `..` target directory
/// inside a checkout as private — a false OK on the condition this row exists
/// for.
/// What: canonicalizes the deepest existing ancestor and appends the rest;
/// `path` unchanged when no ancestor canonicalizes.
/// Test: `gather_sees_a_symlinked_or_dotdot_target_dir_inside_a_checkout`.
fn resolved(path: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut cur = path;
    loop {
        if let Ok(canon) = cur.canonicalize() {
            return tail.iter().rev().fold(canon, |acc, c| acc.join(c));
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                cur = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Every checkout of `dir`'s repository, main checkout first.
///
/// What: the `worktree <path>` lines of `git worktree list --porcelain`.
/// `Err` when git cannot run or exits non-zero.
/// Test: `gather_lists_a_worktree_holding_a_local_target`,
/// `gather_is_undetermined_outside_a_git_repository`.
fn worktree_roots(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect())
}

#[cfg(test)]
#[path = "doctor_session_target_dir_tests.rs"]
mod tests;
