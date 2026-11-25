//! `tm wait --for check --required-only` and `--watch` (#11673 AC5).
//!
//! Why: a PM waits on the checks that gate its merge, not on every check; a
//! non-required check that runs long held the wait (#11595, #8021). And a
//! PM that wants a wake after its turn ends needs the PR in the daemon's
//! PR watch table — only when it asks, with `--watch`.
//! What: [`validate`] refuses the new flags outside `--for check`.
//! [`resolve`] reads the base branch's required contexts once per
//! invocation (`check_gate::read_required`) and refuses an unknown or empty
//! list. [`register_watch`] adds the PR through `tm pr watch`'s `register`.
//! Test: `required_flags_need_for_check`,
//! `check_condition_required_only_ignores_other_checks`.

use trusty_mpm::core::pr_watch::gh::{PR_VIEW_FIELDS, parse_pr_view};
use trusty_mpm::core::pr_watch::table::Table;

use crate::cli::{WaitArgs, WaitFor};
use crate::commands::pr::{GhRunner as _, RealGhRunner, argv, check_gate, repo_slug, watch};

/// Refuse `--required-only`, `--watch` and `--owner` outside `--for check`.
///
/// # Errors
/// One of those flags on another verb, or `--owner` without `--watch`.
///
/// Test: `required_flags_need_for_check`.
pub(crate) fn validate(args: &WaitArgs) -> anyhow::Result<()> {
    let check = args.condition == WaitFor::Check;
    anyhow::ensure!(
        check || !(args.required_only || args.watch),
        "--required-only and --watch apply only to `--for check`"
    );
    anyhow::ensure!(args.owner.is_none() || args.watch, "--owner needs --watch");
    Ok(())
}

/// The required contexts to judge, or `None` without `--required-only`.
///
/// # Errors
/// A `gh` read failed, or the base's required list is unknown or empty —
/// a required-only wait on nothing would be met at once.
pub(crate) fn resolve(args: &WaitArgs, pr: u64) -> anyhow::Result<Option<Vec<String>>> {
    if !args.required_only {
        return Ok(None);
    }
    let gh = RealGhRunner::new()?;
    let slug = repo_slug(&gh, args.repo.as_deref())?;
    let a = argv(&[
        "pr",
        "view",
        &pr.to_string(),
        "--repo",
        &slug,
        "--json",
        PR_VIEW_FIELDS,
    ]);
    let snap = parse_pr_view(&gh.run(&a)?.stdout_ok(&a)?)?;
    let required = check_gate::read_required(&gh, &slug, &snap.base)?.unwrap_or_default();
    anyhow::ensure!(
        !required.is_empty(),
        "{slug} base `{}` requires no status checks; drop --required-only",
        snap.base
    );
    Ok(Some(required))
}

/// Register the PR with the daemon's PR watch; returns the report line.
///
/// # Errors
/// As `tm pr watch add`.
pub(crate) fn register_watch(args: &WaitArgs) -> anyhow::Result<String> {
    let pr = args
        .pr
        .ok_or_else(|| anyhow::anyhow!("`tm wait --for check` needs --pr <number>"))?;
    let gh = RealGhRunner::new()?;
    watch::register(
        &gh,
        &Table::production()?,
        pr,
        args.repo.as_deref(),
        args.owner.clone(),
        &std::env::current_dir()?,
    )
}
