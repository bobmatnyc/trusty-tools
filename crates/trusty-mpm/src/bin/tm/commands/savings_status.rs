//! `tm savings status` runner (#11033).
//!
//! Why: the clap surface lives in `cli/actions/savings.rs`; this binds it to
//! the core fold and prints text or JSON.
//! What: [`run`] folds `~/.trusty-mpm/usage` (or `TRUSTY_MPM_ROOT`) and prints.
//! Test: the fold is covered in `core/savings_status_tests.rs`; parsing by
//! `cli_parses_savings_status`.

use anyhow::Context;
use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::core::savings_status::{GroupBy, fold_status, render_text};

use crate::cli::{SavingsAction, SavingsGroupArg};

/// Run one `tm savings` action.
pub(crate) fn run(action: SavingsAction) -> anyhow::Result<()> {
    let SavingsAction::Status { by, days, json } = action;
    let by = match by {
        SavingsGroupArg::Tool => GroupBy::Tool,
        SavingsGroupArg::Subagent => GroupBy::Subagent,
    };
    let root = FrameworkPaths::default().root;
    let report = fold_status(&root, by, days, chrono::Utc::now().date_naive());
    if json {
        let text = serde_json::to_string_pretty(&report).context("serialise savings report")?;
        println!("{text}");
    } else {
        print!("{}", render_text(&report));
    }
    Ok(())
}
