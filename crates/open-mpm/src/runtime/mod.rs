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

//! open-mpm entry point (PM orchestrator + sub-agent runner + direct/workflow modes).
//!
//! Why: A single binary hosts all execution modes so we don't have to build
//! or distribute separate crates. The binary inspects argv and dispatches.
//! What:
//!   - No args  -> PM mode: reads a line from stdin, calls OpenRouter with
//!     the `delegate_to_agent` tool, spawns the chosen sub-agent subprocess,
//!     forwards the task via NDJSON, prints the result to stdout.
//!   - `--agent <name>` -> sub-agent mode: reads one NDJSON task line from
//!     stdin, runs a chat completion (with tool support when the agent's
//!     config enables it) using the agent's config, writes a single NDJSON
//!     result line to stdout, exits.
//!   - `--direct <name> [--task-file <path>] [--out-dir <dir>]` -> direct mode:
//!     bypasses the PM LLM, sends stdin/file contents straight to the named
//!     sub-agent and optionally extracts file sections from the output.
//!   - `--workflow <name> --task-file <path> --out-dir <dir>` -> workflow mode:
//!     loads `.open-mpm/workflows/<name>.json` and runs each phase sequentially.
//! Test: `cargo run -- --agent python-engineer` with a Task JSON piped on
//! stdin returns a JSON Result line on stdout.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

use crate::default_bundled_config_dir;
use crate::{perf, skills};

// Submodule declarations (issue #171: runtime.rs split into focused modules).
mod cli_def;
mod direct_mode;
mod indexer;
mod mode_dispatch;
mod pm_mode;
mod postmortem;
mod predispatch;
mod registry_cmds;
mod service_cmds;
mod session_cmds;
mod startup;
mod subagent_exec;
mod subagent_mode;
mod subcommands;
mod tool_registry;
mod workflow_mode;

use cli_def::{Cli, HELP, argv_as_task_text};

/// Library entry point — contains the full top-level dispatch previously
/// hosted in `fn main()`.
///
/// Why: Exposing the binary's startup logic via the library lets private
///      launchers (e.g. `open-mpm-local`) install additional agent plugins
///      via `install_plugins(...)` before delegating to this function, so
///      the published `open-mpm` crate stays free of references to
///      `publish = false` agent crates.
/// What: Performs argv parsing, env loading, tracing setup, plugin spawn,
///       subcommand dispatch, mode-flag dispatch (--api/--workflow/--direct
///       /--agent/...), and finally the interactive REPL or CTRL fallback.
/// Test: Indirectly via `cargo run -p open-mpm`/`open-mpm-local` and the
///       crate's integration tests.
pub async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // All pre-parse bootstrapping (env loading, tracing, the early-exit
    // `--version`/`--api`/`--search-service` dispatches, state-dir + run-id
    // setup, migrations, and the background message bus) lives in `startup`.
    // A `false` return means an early-exit path already handled the run.
    if !startup::run_startup_init(&args).await? {
        return Ok(());
    }

    // Subcommand prefixes (`memory`, `agents`, `skills`, `om start|stop|status`,
    // `session`, `dashboard`, …) own their own clap schema and are dispatched
    // on argv before the top-level parser. A `false` return means a subcommand
    // already handled the invocation.
    if !subcommands::dispatch_subcommands(&args).await? {
        return Ok(());
    }

    // Top-level clap parse: every non-subcommand mode flag is captured here
    // so the dispatch below can read fields off `cli` instead of rescanning
    // argv five different ways. We use `try_parse_from` so a parse error
    // returns a friendly clap-rendered message instead of panicking.
    //
    // Issue #216: when the parse fails on an unknown subcommand or unknown
    // argument, attach the workspace-shared `trusty_common::help` suggester
    // hint. The native open-mpm `cli::did_you_mean` path (KNOWN_SUBCOMMANDS
    // scan in `subcommands`) catches the common typos before we reach here;
    // this layer covers residual cases that don't match `KNOWN_SUBCOMMANDS`
    // but do match the declarative `help.yaml`.
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(e) => {
            let kind = e.kind();
            if matches!(
                kind,
                clap::error::ErrorKind::InvalidSubcommand | clap::error::ErrorKind::UnknownArgument
            ) {
                eprintln!("{e}");
                trusty_common::help::print_suggestion_hint(&args, &HELP);
                std::process::exit(e.exit_code());
            }
            return Err(anyhow::anyhow!("{e}"));
        }
    };

    // #126 bug 1: allow inline `--task <STRING>` as an alternative to
    // `--task-file <path>` or piping via stdin.
    //
    // #223: Read --task-file eagerly via std::fs::read_to_string (synchronous,
    // blocking) immediately after clap parse, before any stdin involvement.
    // When stdout is piped the async stdin path inside read_task_text_with_inline
    // can return an empty string (stdin closes in the subshell), producing a
    // spurious "empty task" error. Reading the file here — before the workflow
    // or direct dispatch — ensures the content is always sourced from the file
    // regardless of how stdin/stdout are wired. The task_file *path* is still
    // threaded through to run_workflow for label generation and session records.
    let task_file_content: Option<String> = if let Some(path) = cli.task_file.as_deref() {
        Some(
            std::fs::read_to_string(path)
                .with_context(|| format!("--task-file: failed to read '{path}'"))?,
        )
    } else {
        None
    };
    // inline_task: --task flag takes highest precedence; --task-file content is
    // second; stdin fallback happens inside read_task_text_with_inline when both
    // are None.
    let inline_task: Option<String> = cli.task.clone().or(task_file_content);

    mode_dispatch::dispatch_cli_mode(cli, args, inline_task).await
}

/// Persist post-run skill effectiveness + usage to `~/.open-mpm/skills/index.json`
/// (#171, #174).
///
/// Why: Skill rankings only improve over time if observations from each run
/// flow back into the persisted score. The observe-agent now emits a
/// structured `## Skill Ratings` block (#174) that lets us feed fine-grained,
/// per-skill scores instead of one coarse pass/fail signal applied to every
/// injected skill. When the structured block is absent (older runs, observe
/// skipped, parse failure) we fall back to the original status-derived signal
/// so this hook always produces some signal.
/// What: Rebuilds the registry from the canonical search paths, merges the
/// existing index, increments `use_count` + `last_used` for each skill in
/// `perf_record.skills_used`. If `observe_output` contains a `## Skill Ratings`
/// block with at least one parseable rating, applies those scores via
/// `update_effectiveness`. Otherwise applies a coarse status-derived signal
/// (`success`→0.8, `partial`→0.5, anything else→0.3) to every used skill.
/// All errors are logged at WARN and swallowed so persistence never breaks a
/// run.
/// Test: Indirect for I/O — verified by running with skills auto-injected and
/// inspecting `~/.open-mpm/skills/index.json`. Behavior is unit-tested at the
/// registry level (`merge_index_restores_effectiveness_after_reload`) and the
/// rating-parser level (`parse_skill_ratings_*`).
pub(super) fn update_skill_usage_after_run(
    perf_record: &perf::PerfRecord,
    observe_output: Option<&str>,
) {
    if perf_record.skills_used.is_empty() {
        return;
    }
    let mut reg = skills::registry::SkillRegistry::load(&skills::registry::skill_search_paths(
        &default_bundled_config_dir(),
    ));
    let index_path = skills::registry::skill_index_path();
    if let Err(e) = reg.merge_index(&index_path) {
        tracing::warn!(
            error = %e,
            path = %index_path.display(),
            "skill registry: failed to merge persisted index before update (continuing)"
        );
    }

    let now_iso = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    // Always record the usage counter / last_used timestamp for every skill
    // that was injected — that's independent of how we score effectiveness.
    for name in &perf_record.skills_used {
        reg.record_use(name, &now_iso);
    }

    // Prefer fine-grained ratings emitted by the observe-agent (#174). If the
    // structured block is present and parseable, only those skills receive
    // updates this run; coarse fallback only applies when no ratings are found.
    let ratings = observe_output
        .map(skills::rating::parse_skill_ratings)
        .unwrap_or_default();

    let updated_count = if !ratings.is_empty() {
        for rating in &ratings {
            reg.update_effectiveness(&rating.skill, rating.score);
        }
        tracing::info!(
            count = ratings.len(),
            "skill ratings: updated {} skills from observe-agent",
            ratings.len()
        );
        ratings.len()
    } else {
        // Coarse fallback: derive a single signal from run status and apply it
        // to every injected skill. This guarantees even non-rating runs (older
        // observe-agent prompts, observe phase skipped, parse errors) still
        // contribute *some* signal to the EMA.
        let signal = skills::rating::coarse_fallback_signal(&perf_record.status);
        for name in &perf_record.skills_used {
            reg.update_effectiveness(name, signal);
        }
        tracing::info!(
            count = perf_record.skills_used.len(),
            status = %perf_record.status,
            signal = signal,
            "skill ratings: no structured block found; applied coarse fallback"
        );
        perf_record.skills_used.len()
    };

    if let Err(e) = reg.save_index(&index_path) {
        tracing::warn!(
            error = %e,
            path = %index_path.display(),
            "skill registry: failed to save updated effectiveness index (continuing)"
        );
    } else {
        tracing::info!(
            count = updated_count,
            status = %perf_record.status,
            path = %index_path.display(),
            "skill registry: persisted post-run effectiveness update"
        );
    }
}

#[cfg(test)]
mod main_tests {
    use super::argv_as_task_text;
    use super::workflow_mode::read_task_text_with_inline;
    use crate::runtime::cli_def::Cli;
    use clap::Parser;

    #[test]
    fn argv_as_task_text_strips_flags_and_joins() {
        let args: Vec<String> = vec!["open-mpm", "write", "hello", "world"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(argv_as_task_text(&args), "write hello world");
    }

    #[test]
    fn argv_as_task_text_ignores_long_flags() {
        let args: Vec<String> = vec!["open-mpm", "--ctrl", "do", "thing"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(argv_as_task_text(&args), "do thing");
    }

    #[test]
    fn argv_as_task_text_empty_when_no_positional() {
        let args: Vec<String> = vec!["open-mpm".to_string()];
        assert_eq!(argv_as_task_text(&args), "");
    }

    /// Why: #223 — verify clap parses --task-file correctly so the path is
    /// not lost into `rest` due to trailing_var_arg interaction.
    /// What: Parses a workflow invocation with --task-file and asserts that
    /// `task_file` is `Some` and `rest` is empty.
    /// Test: This test itself.
    #[test]
    fn clap_task_file_parses_correctly_with_workflow() {
        let args = vec![
            "open-mpm",
            "--workflow",
            "prescriptive",
            "--task-file",
            "level-1.txt",
        ];
        let cli = Cli::try_parse_from(args).expect("clap should parse");
        assert_eq!(
            cli.task_file.as_deref(),
            Some("level-1.txt"),
            "task_file should capture the path, not be None"
        );
        assert_eq!(cli.workflow.as_deref(), Some("prescriptive"));
        assert!(
            cli.rest.is_empty(),
            "rest should not consume the --task-file value: {:?}",
            cli.rest
        );
    }

    /// Why: #223 — verify clap parses --task-file with --out-dir correctly.
    /// What: Ensures multiple named flags all parse without leaking values
    /// into `rest`.
    /// Test: This test itself.
    #[test]
    fn clap_task_file_parses_correctly_with_out_dir() {
        let args = vec![
            "open-mpm",
            "--workflow",
            "prescriptive",
            "--task-file",
            "tasks/level-2.txt",
            "--out-dir",
            "/tmp/out",
        ];
        let cli = Cli::try_parse_from(args).expect("clap should parse");
        assert_eq!(cli.task_file.as_deref(), Some("tasks/level-2.txt"));
        assert_eq!(cli.out_dir.as_deref(), Some("/tmp/out"));
        assert!(cli.rest.is_empty(), "rest should be empty: {:?}", cli.rest);
    }

    /// Why: #223 — verify read_task_text_with_inline returns file content
    /// when task_file is None but inline_task is provided (simulates the
    /// eagerly-read file content path added by the #223 fix).
    /// What: Inline task content bypasses all file/stdin reads.
    /// Test: This test itself.
    #[tokio::test]
    async fn read_task_text_inline_takes_priority_over_file() {
        let result = read_task_text_with_inline(None, Some("  hello world  "))
            .await
            .unwrap();
        assert_eq!(result, "hello world");
    }

    /// Why: #223 — verify read_task_text_with_inline reads from an actual file
    /// when task_file path is given and inline_task is None.
    /// What: Writes a temp file, calls the function, asserts content is read.
    /// Test: This test itself.
    #[tokio::test]
    async fn read_task_text_reads_from_file_when_path_given() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("task.txt");
        std::fs::write(&path, "  write a hello world script  ").unwrap();
        let path_str = path.to_str().unwrap();
        let result = read_task_text_with_inline(Some(path_str), None)
            .await
            .unwrap();
        assert_eq!(result, "write a hello world script");
    }
}

// Bundled agent config directory — honors `OPEN_MPM_CONFIG_DIR` with a
// CWD-relative `.open-mpm/` fallback (#167).
//
// Why: The registry search-path function wants the "bundled" config root
// (it appends `/agents` internally). We honor the same env var as
// `agents::mod::agent_config_path` so packaged binaries can point the
// loader at a vendored config tree.
// What: Returns `${OPEN_MPM_CONFIG_DIR}` as-is if set (so search_paths
// appends `/agents`), else `./.open-mpm`. Note: the repo's bundled config
// lives at `.open-mpm/` now (formerly `config/`); runtime state is in
// `.open-mpm/state/` (gitignored).
//
// Note: `default_bundled_config_dir` is defined once in `crate::lib` and
//       pulled in at the top of this file via `use crate::default_bundled_config_dir;`.
//       No duplicate definition here.
