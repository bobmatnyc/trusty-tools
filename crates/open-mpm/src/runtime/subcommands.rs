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

//! Pre-clap subcommand-prefix dispatch (`memory`, `code`, `agents`, `skills`,
//! `start`/`stop`/`status`, `session`, `dashboard`, …) and the
//! flag-aware subcommand locator used by the `om` shell alias.

use anyhow::Result;

use super::{postmortem, registry_cmds, service_cmds, session_cmds};
use crate::{cli, debugger, inspection};

/// Dispatch subcommand prefixes that own their own clap schema and must be
/// detected on argv before the top-level `Cli::try_parse_from`.
///
/// Why: Each subcommand handler parses its own argv tail, so they must be
/// matched before the top-level parser claims the tokens. Pulling the whole
/// block out of `run()` keeps `mod.rs` under the 500-line cap.
/// What: Inspects argv in the original priority order; returns `Ok(false)`
/// when a subcommand handled the invocation (so the caller should
/// `return Ok(())`), `Ok(true)` to continue into the clap parse + mode
/// dispatch. May `std::process::exit` for typo suggestions.
/// Test: Indirectly via the CLI integration tests (`agents list`, `om start`,
/// `om --ctrl session list`, etc.).
pub(super) async fn dispatch_subcommands(args: &[String]) -> Result<bool> {
    // #366: Friendly typo suggestions for top-level subcommands. We only
    // suggest when the first positional token doesn't start with `-` (so
    // top-level flags like `--workflow` still flow through to clap) and
    // when the input doesn't already match a known subcommand. Edit-distance
    // <= 3 catches "memori" -> "memory", "skilss" -> "skills" without
    // hijacking a typed slash command or unrelated arg.
    const KNOWN_SUBCOMMANDS: &[&str] = &[
        "memory",
        "code",
        "memories",
        "agents",
        "skills",
        "inspect",
        "postmortem",
        "debug",
        "eval",
        // #403: persistent service lifecycle subcommands
        "start",
        "stop",
        "status",
        // #405: connect to project + launch REPL in client mode
        "connect",
        // #406: CTRL session management (new/list/attach/kill)
        "session",
        // #442: launch Tauri desktop dashboard GUI ("dash" is an alias)
        "dashboard",
        "dash",
    ];
    if args.len() > 1 {
        let candidate = &args[1];
        if !candidate.starts_with('-')
            && !candidate.starts_with('/')
            && !KNOWN_SUBCOMMANDS.contains(&candidate.as_str())
            && let Some(suggestion) = cli::did_you_mean(candidate, KNOWN_SUBCOMMANDS, 3)
        {
            eprintln!("open-mpm: unknown subcommand '{candidate}'");
            eprintln!("  Did you mean '{suggestion}'?");
            eprintln!("  Run `open-mpm --help` for available commands.");
            std::process::exit(1);
        }
    }

    // CLI subcommands: `memory search`, `memory run`, `code search`.
    // These run against the local store only; no LLM key required.
    if args.len() > 1 && (args[1] == "memory" || args[1] == "code") {
        cli::run_search_command(&args[1..]).await?;
        return Ok(false);
    }

    // `memories <export|import|list>` — cross-machine session sharing.
    if args.len() > 1 && args[1] == "memories" {
        cli::run_memories_command(&args[2..]).await?;
        return Ok(false);
    }

    // #186: `postmortem [--session <id>] [--last N]` — run the postmortem
    // agent against either a specific session's mistake log or the N most
    // recent mistakes from the global log.
    if args.len() > 1 && args[1] == "postmortem" {
        postmortem::run_postmortem_subcommand(&args[2..]).await?;
        return Ok(false);
    }

    // #167: `agents list` — print all agents discovered from the hierarchical
    // search paths with their source + capability tags. Useful to verify
    // per-project / per-user overrides are being picked up.
    if args.len() > 1 && args[1] == "agents" {
        registry_cmds::run_agents_subcommand(&args[2..]).await?;
        return Ok(false);
    }

    // #168: `skills list [--tag <tag>]` — print all skills discovered from the
    // hierarchical search paths with their source + tags. Supports `--tag`
    // to filter + rank by tag overlap.
    if args.len() > 1 && args[1] == "skills" {
        registry_cmds::run_skills_subcommand(&args[2..]).await?;
        return Ok(false);
    }

    // #237: `debug [--session <name>] [--lines <N>] [--no-launch]` —
    // launch open-mpm REPL inside detached tmux session and render a
    // ratatui split-pane TUI in the invoking terminal. See
    // `src/debugger/mod.rs` for full behaviour.
    if args.len() > 1 && args[1] == "debug" {
        debugger::run_debug_subcommand(&args[2..]).await?;
        return Ok(false);
    }

    // PM harness inspection: `open-mpm inspect --task <text> [--dry-run]`.
    // Reports which agent + skills the registry would pick for a task
    // without spawning a sub-agent. Dry-run mode does zero LLM calls.
    if args.len() > 1 && args[1] == "inspect" {
        inspection::run_inspect_subcommand(&args[2..]).await?;
        return Ok(false);
    }

    // #414: `open-mpm plugins [list|status|check]` — report which optional
    // MCP plugins (trusty-search, trusty-memory) are present on PATH.
    if args.len() > 1 && args[1] == "plugins" {
        registry_cmds::run_plugins_subcommand(&args[2..]).await?;
        return Ok(false);
    }

    // #449: `open-mpm eval run --suite <path>` — run a behavior eval suite.
    if args.len() > 1 && args[1] == "eval" {
        registry_cmds::run_eval_subcommand(&args[2..]).await?;
        return Ok(false);
    }

    // #403: `open-mpm start|stop|status` — persistent background server lifecycle.
    //
    // Why: Mirror the existing `--service start|stop|status` flag as
    // first-class subcommands so users can type `om start` instead of
    // `om --service start`. The underlying mechanics (`start_service`,
    // `stop_service`, `status_line` from `src/service/mod.rs`) are reused
    // verbatim — this is purely a friendlier CLI surface.
    // Test: `om start` brings up the daemon, `om status` reports it,
    // `om stop` shuts it down.
    // #409: Dispatch service-lifecycle and session subcommands by scanning
    // for the subcommand token even when preceded by mode flags like
    // `--ctrl` (injected by the `om` shell alias). Without this, those
    // subcommands fall through to the CTRL REPL and the REST API response
    // is swallowed before it can print to stdout.
    // #442: `open-mpm dashboard` — launch the Tauri desktop GUI.
    //
    // Why: Users want a one-shot way to spin up the bundled UI without
    // hunting for the binary path. We resolve it relative to the installed
    // `om` binary and the current working directory so it works whether the
    // user invokes `om` from a clone or after `cargo install`.
    // Test: With the UI built, `om dashboard` should spawn the Tauri binary
    // and exit 0. Without it, it prints a build hint and exits non-zero.
    if let Some(idx) = find_subcommand_index(args, &["dashboard", "dash"]) {
        registry_cmds::run_dashboard_subcommand(&args[idx + 1..]).await?;
        return Ok(false);
    }

    const SERVICE_SUBCOMMANDS: &[&str] = &["start", "stop", "status", "connect", "session"];
    if let Some(idx) = find_subcommand_index(args, SERVICE_SUBCOMMANDS) {
        match args[idx].as_str() {
            "start" => service_cmds::run_start_subcommand(&args[idx + 1..]).await?,
            "stop" => service_cmds::run_stop_subcommand(&args[idx + 1..]).await?,
            "status" => service_cmds::run_status_subcommand(&args[idx + 1..]).await?,
            "connect" => service_cmds::run_connect_subcommand(&args[idx + 1..]).await?,
            "session" => session_cmds::handle_session_subcommand(&args[idx + 1..]).await?,
            _ => unreachable!("SERVICE_SUBCOMMANDS guards this match"),
        }
        return Ok(false);
    }

    Ok(true)
}

// #409: Find the subcommand position even when preceded by mode flags.
//
// Why: The `om` shell alias prepends `--ctrl` unconditionally, which
// pushes `session new`, `session list`, etc. past argv[1]. Without this
// helper, the early-argv dispatch misses the subcommand and falls
// through to the CTRL REPL, which swallows the REST API response before
// the user can see it.
// What: Returns the index of the first non-flag, non-flag-value token
// that matches a known subcommand name. We treat tokens starting with
// `-` as flags and skip a single positional value after `--port`,
// `--workflow`, `--agent`, etc. (the small set of mode flags that take
// values and could appear before a subcommand).
// Test: `om --ctrl session list` should dispatch to handle_session_subcommand,
// not run_ctrl_repl.
fn find_subcommand_index(args: &[String], known: &[&str]) -> Option<usize> {
    // Mode flags that take a value and might appear before a subcommand.
    // Keep this list narrow — flags not on it are treated as bare flags.
    const VALUE_FLAGS: &[&str] = &[
        "--port",
        "--workflow",
        "--agent",
        "--out-dir",
        "--lines",
        "--session",
        "--task",
    ];
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if a.starts_with('-') {
            if VALUE_FLAGS.contains(&a.as_str()) {
                i += 2;
            } else if let Some(eq_pos) = a.find('=')
                && VALUE_FLAGS.contains(&&a[..eq_pos])
            {
                i += 1;
            } else {
                i += 1;
            }
            continue;
        }
        if known.contains(&a.as_str()) {
            return Some(i);
        }
        // First non-flag token that isn't a known subcommand: stop scanning.
        return None;
    }
    None
}
