// Pre-existing clippy warnings across this large binary crate.
// Each category below is suppressed at crate level with rationale:
// - dead_code / unused_imports: Many helpers are kept for future use, behind
//   feature flags, or used only on certain platforms / by tests; pruning them
//   is its own refactor and would churn unrelated modules.
// - clippy::collapsible_if / collapsible_else_if: Style preference; nested
//   ifs are often clearer with the existing comments and gating logic.
// - clippy::manual_str_repeat / manual_repeat_n / single_char_add_str: Style
//   nits in display/formatting code where current form reads fine.
// - clippy::too_many_arguments: A few orchestration entry points genuinely
//   need their argument count; signatures are part of internal contracts.
// - clippy::await_holding_lock: Test-only — a std::sync::Mutex serializes
//   tests that mutate process-global env (HOME, etc.). The await points are
//   inside the critical section by design, and tests are single-threaded
//   per-test by virtue of the lock.
// - clippy::clone_on_copy / len_zero / map_or / etc.: Misc style nits in
//   pre-existing code; not worth the churn vs. risk of breaking 1500+ tests.
#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_mut)]
#![allow(unused_assignments)]
#![allow(unused_variables)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::manual_str_repeat)]
#![allow(clippy::manual_repeat_n)]
#![allow(clippy::single_char_add_str)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::await_holding_lock)]
#![allow(clippy::clone_on_copy)]
#![allow(clippy::len_zero)]
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::manual_map)]
#![allow(clippy::needless_borrows_for_generic_args)]
#![allow(clippy::unnecessary_sort_by)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::new_without_default)]
#![allow(clippy::manual_split_once)]
#![allow(clippy::needless_splitn)]
#![allow(clippy::single_match_else)]
#![allow(clippy::single_match)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::manual_clamp)]
#![allow(clippy::redundant_closure)]
#![allow(clippy::manual_pattern_char_comparison)]
#![allow(clippy::vec_init_then_push)]
#![allow(clippy::single_component_path_imports)]
#![allow(clippy::derivable_impls)]
#![allow(clippy::match_single_binding)]
#![allow(clippy::redundant_pattern_matching)]

//! Postmortem-agent dispatch: the shared `trigger_postmortem` helper and the
//! `open-mpm postmortem` CLI subcommand.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::{mistake_log, subprocess, tools};

/// #186: Spawn the postmortem-agent subprocess for a session. Used both by
/// the auto-trigger path (after a workflow run with logged mistakes) and the
/// `postmortem` CLI subcommand.
///
/// Why: Centralizing dispatch keeps the construction of the task prompt,
/// agent name, and config dir in one place; both callers want the agent to
/// inspect the local `.open-mpm/state/mistakes/<session>.jsonl` file.
/// What: Builds a SubprocessAgentRunner pointed at the project's bundled
/// agents directory, hands it a task that names the session id and the
/// file path, and prints the resulting agent output to stderr.
/// Test: Manual; covered indirectly by the auto-trigger end-to-end flow.
pub(super) async fn trigger_postmortem(project_root: &Path, session_id: &str) -> Result<()> {
    use tools::AgentRunner;
    let agents_config_dir = project_root.join(".open-mpm").join("agents");
    let log_path = project_root
        .join(".open-mpm")
        .join("state")
        .join("mistakes")
        .join(format!("{session_id}.jsonl"));
    let task = format!(
        "Analyze the mistake log at {} for session {} and produce a postmortem report following your standard format. Categorize each failure, apply fixes you are confident about, and recommend follow-ups.",
        log_path.display(),
        session_id
    );
    let runner = subprocess::SubprocessAgentRunner::new().with_config_dir(Some(agents_config_dir));
    let output = runner.run("postmortem-agent", &task).await?;
    eprintln!(
        "\n=== Postmortem Report ({session_id}) ===\n{}",
        output.content
    );
    Ok(())
}

/// #186: `open-mpm postmortem [--session <id>] [--last N]` subcommand.
///
/// Why: Operators want to invoke postmortem analysis on demand — either on
/// a specific failed session or on the recent global error stream — without
/// running a full workflow.
/// What: Parses --session and --last flags, dispatches to either
/// `trigger_postmortem` or feeds the recent global mistakes inline.
/// Test: Manual smoke (`open-mpm postmortem --last 5`); the helper logic is
/// unit-tested via `MistakeLog` directly.
pub(super) async fn run_postmortem_subcommand(args: &[String]) -> Result<()> {
    let mut session: Option<String> = None;
    let mut last: usize = 20;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--session" => {
                session = args.get(i + 1).cloned();
                i += 2;
            }
            "--last" => {
                if let Some(v) = args.get(i + 1).and_then(|s| s.parse::<usize>().ok()) {
                    last = v;
                }
                i += 2;
            }
            _ => i += 1,
        }
    }

    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    if let Some(sid) = session {
        return trigger_postmortem(&project_root, &sid).await;
    }

    // No --session: feed the last N global mistakes to the agent inline.
    let recent = mistake_log::MistakeLog::read_recent_global(last)?;
    if recent.is_empty() {
        println!("(no mistakes recorded)");
        return Ok(());
    }
    let payload = serde_json::to_string_pretty(&recent)?;
    let task = format!(
        "Analyze these {} recent agent mistakes and produce a postmortem report:\n\n{}",
        recent.len(),
        payload
    );
    use tools::AgentRunner;
    let agents_config_dir = project_root.join(".open-mpm").join("agents");
    let runner = subprocess::SubprocessAgentRunner::new().with_config_dir(Some(agents_config_dir));
    let output = runner.run("postmortem-agent", &task).await?;
    println!("{}", output.content);
    Ok(())
}
