//! `tm doctor --fix` — the per-repair dispatch, `--only` scoping and the
//! fail-closed dry run (#10425).
//!
//! Why: `--fix` ran every repair class as one block, so an operator could not
//! apply one check's repair without the others, and a repair skipped for a
//! missing prerequisite (no cwd, no home, no managed config dir) vanished from
//! the output — a dry run then read as "no change" for a repair it never
//! evaluated.
//! What: [`FIX_REPAIR_IDS`] names every repair by the doctor check it answers,
//! in run order; [`resolve_scope`] turns `--only` values into the ids to run
//! and refuses an unknown one; [`run_repairs`] runs each selected repair and
//! prints its steps, then one `NOT EVALUATED` line per repair it could not run.
//! Test: `doctor_fix_run_tests.rs`; the repairs themselves are tested in their
//! own modules.

use std::cell::OnceCell;
use std::path::{Path, PathBuf};

use trusty_mpm::core::doctor_repair::{
    RepairMode, RepairStep, refuse_legacy_sources, repair_build_tree_binary,
    repair_hooks_contamination, repair_missing_hook_group, repair_push_guard, repair_statusline,
};
use trusty_mpm::core::doctor_repair_scope::repair_session_scope;
use trusty_mpm::core::stray_mcp::quarantine_strays;

use super::doctor_repair::{
    machine_wide_settings_files, output_style_steps, print_steps, skill_steps,
    statusline_settings_files,
};

/// Every repair `tm doctor --fix` runs, named by the check it answers, in run
/// order.
///
/// Why (#10425): `--only <check>` takes these values, and clap lists them in
/// `--help`, so the scope an operator can name is the scope that exists.
/// What: the order is the dependency order the repairs need — the build-tree
/// repoint before the contamination strip (#7262), the strip before the
/// lifecycle-group re-merge (#7490), the repoint before the statusline seed
/// (#7617).
/// Test: `every_repair_id_dispatches_and_no_other_does`.
pub(crate) const FIX_REPAIR_IDS: &[&str] = &[
    "skill_staleness",
    "hooks_build_tree_binary",
    "hook-interpreter",
    "statusline",
    "hooks_contamination",
    "hooks_missing_tm_group",
    "push_guard",
    "session_scope",
    "ancestor_claude_md",
    "auto_memory",
    "search_project_duplicate_index",
    "rust_build_env",
    "output_style_staleness",
    "legacy_sources",
    "stray_mcp_json",
    "launchd_secrets",
    "launchd_process_type",
    "claude_json_secrets",
    "notification_hook",
    "worktree_markers",
];

/// What one repair produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RepairRun {
    /// The repair ran; an empty list means it found nothing to change.
    Evaluated(Vec<RepairStep>),
    /// The repair could not run, so its effect is unknown; the string says why.
    NotEvaluated(String),
}

/// The paths and mode every repair reads.
pub(crate) struct FixContext {
    pub(crate) mode: RepairMode,
    pub(crate) include_frozen: bool,
    pub(crate) project: Option<PathBuf>,
    pub(crate) home: Option<PathBuf>,
    pub(crate) managed_config: Option<PathBuf>,
    machine_files: OnceCell<Vec<PathBuf>>,
}

impl FixContext {
    /// Build a context from explicit paths, so a test supplies a fake root.
    pub(crate) fn new(
        mode: RepairMode,
        include_frozen: bool,
        project: Option<PathBuf>,
        home: Option<PathBuf>,
        managed_config: Option<PathBuf>,
    ) -> Self {
        Self {
            mode,
            include_frozen,
            project,
            home,
            managed_config,
            machine_files: OnceCell::new(),
        }
    }

    /// The machine-wide settings list, walked once and only when a selected
    /// repair needs it.
    fn machine_files(&self) -> &[PathBuf] {
        self.machine_files.get_or_init(|| {
            machine_wide_settings_files(self.project.as_deref(), self.home.as_deref())
        })
    }

    /// Run `f` on the project dir, or report the repair unevaluated.
    fn with_project(&self, f: impl FnOnce(&Path) -> Vec<RepairStep>) -> RepairRun {
        match &self.project {
            Some(p) => RepairRun::Evaluated(f(p)),
            None => RepairRun::NotEvaluated("could not resolve the current directory".into()),
        }
    }

    /// Run `f` on the home dir, or report the repair unevaluated.
    fn with_home(&self, f: impl FnOnce(&Path) -> Vec<RepairStep>) -> RepairRun {
        match &self.home {
            Some(h) => RepairRun::Evaluated(f(h)),
            None => RepairRun::NotEvaluated("could not resolve the home directory".into()),
        }
    }

    /// Run `f` on the managed `CLAUDE_CONFIG_DIR`, or report it unevaluated.
    fn with_managed(&self, f: impl FnOnce(&Path) -> Vec<RepairStep>) -> RepairRun {
        match &self.managed_config {
            Some(d) => RepairRun::Evaluated(f(d)),
            None => RepairRun::NotEvaluated("no managed CLAUDE_CONFIG_DIR resolved".into()),
        }
    }
}

/// One repair, as a function of the context.
type RepairFn = fn(&FixContext) -> RepairRun;

/// The repair registered for `id`, or `None` for an id `--fix` does not run.
///
/// Why: a match on the id is the one place a repair is bound to its name, so
/// a test can prove every [`FIX_REPAIR_IDS`] entry reaches a repair without
/// running any.
/// What: one arm per id; each arm calls the repair the pre-#10425 block ran.
/// Test: `every_repair_id_dispatches_and_no_other_does`.
pub(crate) fn dispatch(id: &str) -> Option<RepairFn> {
    let f: RepairFn = match id {
        "skill_staleness" => |c| RepairRun::Evaluated(skill_steps(c.include_frozen, c.mode)),
        "hooks_build_tree_binary" => {
            |c| RepairRun::Evaluated(repair_build_tree_binary(c.machine_files(), c.mode))
        }
        "hook-interpreter" => |c| {
            RepairRun::Evaluated(
                trusty_mpm::core::hook_interpreter_doctor::repair_hook_interpreter(
                    c.machine_files(),
                    c.mode,
                ),
            )
        },
        "statusline" => |c| {
            let files = statusline_settings_files(c.project.as_deref(), c.home.as_deref());
            RepairRun::Evaluated(repair_statusline(&files, c.mode))
        },
        "hooks_contamination" => |c| c.with_project(|p| repair_hooks_contamination(p, c.mode)),
        "hooks_missing_tm_group" => |c| c.with_project(|p| repair_missing_hook_group(p, c.mode)),
        "push_guard" => |c| c.with_project(|p| repair_push_guard(p, c.mode)),
        "session_scope" => |c| {
            c.with_project(|p| {
                repair_session_scope(
                    p,
                    c.managed_config.as_deref(),
                    trusty_mpm::core::session_mcp_scope::session_mcp_path(p).as_deref(),
                    c.mode,
                )
            })
        },
        "ancestor_claude_md" => |c| {
            c.with_project(|p| {
                trusty_mpm::core::ancestor_claude_md_repair::repair_ancestor_claude_md(
                    p,
                    c.home.as_deref(),
                    c.managed_config.as_deref(),
                    &chrono::Utc::now().format("%Y%m%d").to_string(),
                    c.mode,
                )
            })
        },
        "auto_memory" => |c| {
            c.with_project(|p| {
                trusty_mpm::daemon::doctor_auto_memory::repair_auto_memory(p, c.mode)
            })
        },
        "search_project_duplicate_index" => {
            |c| c.with_project(|p| trusty_mpm::daemon::doctor::repair_duplicate_indexes(p, c.mode))
        }
        "rust_build_env" => rust_build_env_run,
        "output_style_staleness" => |c| {
            c.with_home(|h| {
                // #9396: a doctor repair never fetches the content release.
                let content = trusty_mpm::core::content_source::framework_content_local(None);
                output_style_steps(content, h, c.managed_config.as_deref(), c.mode)
            })
        },
        "legacy_sources" => |c| c.with_home(refuse_legacy_sources),
        "stray_mcp_json" => |c| {
            c.with_home(|h| {
                let root = trusty_mpm::core::mcp_provenance::default_framework_root();
                quarantine_strays(&root, c.project.as_deref(), h, c.mode)
            })
        },
        "launchd_secrets" => |c| {
            c.with_home(|h| {
                trusty_mpm::daemon::doctor_launchd_secrets_repair::repair_launchd_plist_secrets(
                    h, c.mode,
                )
            })
        },
        "launchd_process_type" => |c| {
            c.with_home(|h| {
                trusty_mpm::daemon::doctor_launchd_process_type::repair::repair_launchd_process_type(
                    h, c.mode,
                )
            })
        },
        "claude_json_secrets" => |c| {
            c.with_managed(|d| {
                trusty_mpm::daemon::doctor_claude_json_secrets_repair::repair_claude_json_secrets(
                    d, c.mode,
                )
            })
        },
        "notification_hook" => |c| {
            c.with_managed(|d| {
                trusty_mpm::core::standalone::hooks::notification::repair_notification_hook(
                    &d.join("settings.json"),
                    None,
                    trusty_mpm::core::config::MpmConfig::load_default()
                        .notification_hook
                        .enabled,
                    c.mode,
                )
            })
        },
        "worktree_markers" => |c| {
            RepairRun::Evaluated(
                trusty_mpm::session_manager::worktree_marker_migration::repair_worktree_markers(
                    &trusty_mpm::daemon::managed_routes::inproject::repos_root(),
                    &trusty_mpm::project::default_adopted_anchors(),
                    c.mode,
                ),
            )
        },
        _ => return None,
    };
    Some(f)
}

/// The ids `--only` selects, in run order; every id when `only` is empty.
///
/// Why (#10425): an unknown id must be an error, never a silent no-op — a
/// typo that ran nothing would print a clean summary.
/// What: `Err` naming the unknown id and the valid ones; otherwise the
/// [`FIX_REPAIR_IDS`] entries `only` names, deduplicated, in run order.
/// Test: `an_unknown_only_id_is_an_error`, `only_selects_in_run_order`.
pub(crate) fn resolve_scope(only: &[String]) -> anyhow::Result<Vec<&'static str>> {
    if let Some(bad) = only
        .iter()
        .find(|id| !FIX_REPAIR_IDS.contains(&id.as_str()))
    {
        anyhow::bail!(
            "`--only {bad}` names no `tm doctor --fix` repair; valid ids: {}",
            FIX_REPAIR_IDS.join(", ")
        );
    }
    Ok(FIX_REPAIR_IDS
        .iter()
        .copied()
        .filter(|id| only.is_empty() || only.iter().any(|o| o == id))
        .collect())
}

/// Run each id in `scope` against `ctx`, in order.
///
/// What: the steps of every evaluated repair, concatenated, and one
/// `(id, reason)` per repair that could not run.
/// Test: `a_dry_run_changes_nothing_under_a_fake_root`,
/// `a_repair_without_its_prerequisite_is_reported_not_evaluated`.
pub(crate) fn collect(
    ctx: &FixContext,
    scope: &[&'static str],
) -> (Vec<RepairStep>, Vec<(&'static str, String)>) {
    let mut steps = Vec::new();
    let mut skipped = Vec::new();
    for id in scope {
        match dispatch(id).map(|f| f(ctx)) {
            Some(RepairRun::Evaluated(s)) => steps.extend(s),
            Some(RepairRun::NotEvaluated(why)) => skipped.push((*id, why)),
            None => skipped.push((*id, "no repair is registered for this id".to_string())),
        }
    }
    (steps, skipped)
}

/// `--fix` action — preview, or apply, the selected repairs.
///
/// Why: see the module doc and #4948. `--yes` is the deliberate second act
/// that writes; `--dry-run` (#10425) states the preview explicitly.
/// What: resolves the scope (`Err` before any repair runs on an unknown id),
/// prints the header, the steps through [`print_steps`], and one
/// `NOT EVALUATED` line per repair whose effect is unknown. With repairs
/// unevaluated and no steps it never prints "nothing to repair".
/// Test: `doctor_fix_run_tests.rs`; `tm_doctor_fix_dry_run` drives the binary.
pub(crate) fn run_repairs(
    apply: bool,
    include_frozen: bool,
    only: &[String],
) -> anyhow::Result<()> {
    let scope = resolve_scope(only)?;
    let mode = RepairMode::from_apply_flag(apply);
    let header = if apply {
        "applying"
    } else {
        "dry run — nothing will be written"
    };
    let scoped = if only.is_empty() {
        String::new()
    } else {
        format!("; only: {}", scope.join(", "))
    };
    println!("\nfix ({header}{scoped})");

    let ctx = FixContext::new(
        mode,
        include_frozen,
        std::env::current_dir().ok(),
        dirs::home_dir(),
        trusty_mpm::core::trusty_tools_config::managed_claude_config_dir(),
    );
    let (steps, skipped) = collect(&ctx, &scope);
    if steps.is_empty() && !skipped.is_empty() {
        println!("  nothing to repair among the repairs that ran");
    } else {
        print_steps(&steps, apply, &apply_hint(&scope, only.is_empty()));
    }
    for (id, why) in &skipped {
        println!("  ? [{id}] NOT EVALUATED: {why} — its effect is unknown, not \"no change\"");
    }
    Ok(())
}

/// The command that applies this run's preview, keeping its `--only` scope.
///
/// Test: `the_apply_hint_keeps_the_only_scope`.
pub(crate) fn apply_hint(scope: &[&str], unscoped: bool) -> String {
    if unscoped {
        return super::doctor_repair::FIX_APPLY_HINT.to_string();
    }
    let only: String = scope.iter().map(|id| format!(" --only {id}")).collect();
    format!("tm doctor --fix{only} --yes")
}

/// The `rust_build_env` repair, resolved against the context's paths (#6868).
///
/// Why: the repair is hermetic and takes resolved settings; this turns the
/// home and project into them. #10425: a project whose Rust-ness or repo
/// identity cannot be determined is reported unevaluated, never "no change".
/// What: a non-Rust project evaluates to no steps; an undetermined scan or an
/// unresolvable build env is [`RepairRun::NotEvaluated`].
/// Test: `core::build_env_repair`'s tests cover the repair.
fn rust_build_env_run(c: &FixContext) -> RepairRun {
    let Some(home) = c.home.as_deref() else {
        return RepairRun::NotEvaluated("could not resolve the home directory".into());
    };
    let project = c.project.as_deref();
    match project.and_then(trusty_mpm::core::build_env::project_is_rust) {
        Some(true) => {}
        Some(false) => return RepairRun::Evaluated(Vec::new()),
        None => {
            return RepairRun::NotEvaluated(
                "could not determine whether the project is a Rust checkout".into(),
            );
        }
    }
    let config_path = trusty_common::crate_config::crate_config_path_at(
        home,
        trusty_mpm::core::trusty_tools_config::CRATE_NAME,
    );
    let config = trusty_common::crate_config::load_at::<
        trusty_mpm::core::trusty_tools_config::TrustyToolsConfig,
    >(&config_path)
    .ok()
    .flatten();
    let identity = project.and_then(trusty_common::github_path::derive_github_path);
    match trusty_mpm::core::build_env::resolve_build_env(
        config.as_ref().and_then(|c| c.build.as_ref()),
        home,
        identity.as_ref(),
        trusty_mpm::core::build_env::host_cores(),
    ) {
        Ok(env) => RepairRun::Evaluated(trusty_mpm::core::build_env_repair::repair_rust_build_env(
            &config_path,
            &env,
            c.mode,
        )),
        Err(e) => RepairRun::NotEvaluated(format!("the build environment is undetermined: {e}")),
    }
}

#[cfg(test)]
#[path = "doctor_fix_run_tests.rs"]
mod tests;
