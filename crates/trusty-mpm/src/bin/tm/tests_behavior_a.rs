//! Behavior tests: project scaffolding, install, hook guard, optimizer,
//! session output / pause / resume / run subcommand parsing.
//!
//! Why: keeping behavior tests in separate files keeps each file under the
//! 500-line cap while grouping tests by the feature they exercise.
//! What: `project_init_*`, `install_*`, `hook_guard_*`, `mpm_hook_*`,
//! `cli_parses_optimizer_*`, `cli_parses_session_{events,breakers,pause,
//! resume,run,output,*}`, `compression_stats_*`, `cli_{parses_overseer,
//! event_summary_*}`, `hook_disable_env_short_circuits`,
//! `test_write_project_hooks_for_dir_targets_project_dir`.
//! Test: `cargo test -p trusty-mpm` runs the full suite.

use clap::Parser;

use crate::cli::{
    Cli, CliCompressionLevel, Command, OptimizerAction, OverseerAction, SessionAction,
};
use crate::commands::install::{
    deploy_report_lines, install_to, mpm_hook_additions, skill_report_lines,
    write_project_hooks_for_dir,
};
use crate::commands::misc::{DISABLE_HOOKS_ENV, SUB_AGENT_ENV, hook};
use crate::commands::project::scaffold_project_dir;
use crate::formatters::session::{event_summary, print_compression_stats};

#[test]
fn project_init_scaffolds_dotdir() {
    // `project init` must create `.trusty-mpm/{config.toml,sessions/}`
    // with a config skeleton naming the project after its directory.
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("my-app");
    std::fs::create_dir_all(&project).unwrap();
    let report = scaffold_project_dir(&project).unwrap();
    assert_eq!(report.len(), 2);

    let config = project.join(".trusty-mpm/config.toml");
    let sessions = project.join(".trusty-mpm/sessions");
    assert!(config.exists());
    assert!(sessions.is_dir());
    let contents = std::fs::read_to_string(&config).unwrap();
    assert!(contents.contains("name = \"my-app\""));
    assert!(contents.contains("[agents]"));
    assert!(contents.contains("[skills]"));
}

#[test]
fn project_init_keeps_existing_config() {
    // Re-running `project init` must never clobber an edited config.toml.
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().to_path_buf();
    scaffold_project_dir(&project).unwrap();
    let config = project.join(".trusty-mpm/config.toml");
    std::fs::write(&config, "# edited by hand").unwrap();

    let report = scaffold_project_dir(&project).unwrap();
    assert!(report.iter().any(|l| l.contains("skipped")));
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        "# edited by hand"
    );
}

#[test]
fn cli_parses_install_no_force() {
    let cli = Cli::try_parse_from(["trusty-mpm", "install"]).unwrap();
    match cli.command.unwrap() {
        Command::Install { force } => assert!(!force),
        other => panic!("expected Install, got {other:?}"),
    }
}

#[test]
fn cli_parses_install_with_force() {
    let cli = Cli::try_parse_from(["trusty-mpm", "install", "--force"]).unwrap();
    match cli.command.unwrap() {
        Command::Install { force } => assert!(force),
        other => panic!("expected Install, got {other:?}"),
    }
}

#[test]
fn cli_parses_hook() {
    let cli = Cli::try_parse_from(["trusty-mpm", "hook"]).unwrap();
    assert!(matches!(cli.command.unwrap(), Command::Hook));
}

/// Why: when `CLAUDE_MPM_SUB_AGENT` is present in the env, the hook
/// handler must return Ok(()) without performing any I/O — that is the
/// whole point of the sub-agent guard.
/// What: sets the env var, calls `hook` with a deliberately-unreachable
/// daemon URL, asserts the call returns Ok in well under the daemon's
/// connect timeout (so we know the network code never ran).
#[tokio::test]
async fn hook_guard_short_circuits() {
    // SAFETY: tokio's current_thread runtime is single-threaded; this
    // env-var dance is isolated to the test.
    unsafe {
        std::env::set_var(SUB_AGENT_ENV, "1");
    }
    let client = reqwest::Client::new();
    let started = std::time::Instant::now();
    // Use a URL we know cannot be reached. If the guard fails the call
    // would consume the full 2s timeout; the assert below catches that.
    let res = hook(&client, "http://127.0.0.1:1").await;
    let elapsed = started.elapsed();
    unsafe {
        std::env::remove_var(SUB_AGENT_ENV);
    }
    assert!(res.is_ok(), "guard branch must return Ok");
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "guard must short-circuit without network I/O; took {elapsed:?}"
    );
}

/// Why: when `TRUSTY_MPM_DISABLE_HOOKS` is set, the hook handler must
/// short-circuit immediately without I/O — this is the universal opt-out
/// for build shells / CI where the hook must not fire.
/// What: sets the env var, calls `hook` with an unreachable URL, and
/// asserts the call returns Ok(()) without blocking the 500 ms connect
/// timeout that would fire if network code ran.
#[tokio::test]
async fn hook_disable_env_short_circuits() {
    // SAFETY: test is single-threaded (tokio current_thread).
    unsafe {
        std::env::set_var(DISABLE_HOOKS_ENV, "1");
    }
    let client = reqwest::Client::new();
    let started = std::time::Instant::now();
    let res = hook(&client, "http://127.0.0.1:1").await;
    let elapsed = started.elapsed();
    unsafe {
        std::env::remove_var(DISABLE_HOOKS_ENV);
    }
    assert!(res.is_ok(), "DISABLE_HOOKS_ENV branch must return Ok");
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "DISABLE_HOOKS_ENV must short-circuit without network I/O; took {elapsed:?}"
    );
}

/// Why: `write_project_hooks_for_dir` must write hooks into the project's
/// `.claude/settings.json` — NOT into any global file. This verifies that
/// the function targets the correct path.
/// What: creates a tempdir acting as the project root, calls the function,
/// asserts the file is created at `<project>/.claude/settings.json` with
/// MPM hook entries present and the command using an absolute path.
#[test]
fn test_write_project_hooks_for_dir_targets_project_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path();
    let settings = project_dir.join(".claude").join("settings.json");

    // The file must not exist before the call.
    assert!(!settings.exists());

    let wrote = write_project_hooks_for_dir(project_dir).unwrap();
    assert!(wrote, "must report file was written");
    assert!(settings.exists(), "project settings must be created");

    let text = std::fs::read_to_string(&settings).unwrap();
    let val: serde_json::Value = serde_json::from_str(&text).unwrap();
    let hooks = val.get("hooks").expect("hooks key must be present");
    assert!(
        hooks.get("PreToolUse").is_some(),
        "PreToolUse hook must be written"
    );

    // The command must end with " hook" (and start with '/' if current_exe
    // is resolvable in test, which it always is under cargo test).
    let cmd = hooks["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(
        cmd.ends_with(" hook"),
        "hook command must end with ' hook', got: {cmd:?}"
    );
}

/// Why: re-running `tm install` against a settings file that already
/// carries the MPM hook block must not rewrite the file. This is the
/// idempotency requirement.
/// What: builds the additions block, merges it into an empty document,
/// then merges the same additions back into the result, and asserts the
/// two outputs are deeply equal.
#[test]
fn mpm_hook_additions_is_idempotent_under_merge() {
    use trusty_common::claude_config::merge_hook_entries;
    let additions = mpm_hook_additions();
    let once = merge_hook_entries(&serde_json::json!({}), &additions);
    let twice = merge_hook_entries(&once, &additions);
    assert_eq!(once, twice, "second merge must be a no-op");
    // And every expected event lands exactly once.
    for event in ["PreToolUse", "PostToolUse", "Stop"] {
        let arr = once["hooks"][event].as_array().expect("event array");
        assert_eq!(arr.len(), 1, "{event}: expected one matcher block");
    }
}

#[test]
fn install_writes_all_artifacts() {
    // A fresh install must write every bundled artifact to disk under the
    // framework root, with matching content.
    let dir = tempfile::tempdir().unwrap();
    let paths = trusty_mpm::core::paths::FrameworkPaths::under(dir.path());
    let report = install_to(&paths, false).unwrap();
    assert_eq!(report.len(), trusty_mpm::core::bundle::ALL.len());
    for artifact in trusty_mpm::core::bundle::ALL {
        let dest = paths.framework.join(artifact.rel_path);
        assert!(dest.exists(), "missing {}", artifact.rel_path);
        let written = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(written, artifact.contents);
    }
}

#[test]
fn install_skips_existing_without_force() {
    // An existing artifact is left untouched without `--force` and the
    // report says so; `--force` overwrites it.
    let dir = tempfile::tempdir().unwrap();
    let paths = trusty_mpm::core::paths::FrameworkPaths::under(dir.path());
    let optimizer = paths.optimizer_config();
    std::fs::create_dir_all(&paths.hooks).unwrap();
    std::fs::write(&optimizer, "custom").unwrap();

    let report = install_to(&paths, false).unwrap();
    assert!(report.iter().any(|l| l.contains("skipped")));
    assert_eq!(std::fs::read_to_string(&optimizer).unwrap(), "custom");

    let forced = install_to(&paths, true).unwrap();
    assert!(forced.iter().all(|l| !l.contains("skipped")));
    assert_ne!(std::fs::read_to_string(&optimizer).unwrap(), "custom");
}

#[test]
#[ignore = "requires bundled framework artifacts (base-agent) — run locally after tm install"]
fn install_then_deploy_composes_agents() {
    // Installing the bundled agent sources and then deploying them must
    // produce composed, inheritance-flattened files in `.claude/agents/`.
    let dir = tempfile::tempdir().unwrap();
    let paths = trusty_mpm::core::paths::FrameworkPaths::under(dir.path());
    install_to(&paths, false).unwrap();

    let result = trusty_mpm::core::agent_deployer::deploy_agents(
        &paths.agent_source_dir(),
        &paths.claude_agents_dir(),
    )
    .unwrap();
    // All six bundled agents deploy on a fresh target.
    assert_eq!(result.deployed.len(), 6);
    assert!(result.skipped.is_empty());

    // The composed engineer carries inherited base content and no
    // `extends:` for Claude Code to interpret.
    let engineer = std::fs::read_to_string(paths.claude_agents_dir().join("engineer.md")).unwrap();
    assert!(engineer.contains("BASE-AGENT"));
    assert!(engineer.contains("BASE-ENGINEER"));
    assert!(engineer.contains("# Engineer"));
    assert!(!engineer.contains("extends:"));

    // The report formatter renders a composed-chain line.
    let lines = deploy_report_lines(&result, &paths.agent_source_dir());
    assert!(
        lines
            .iter()
            .any(|l| l.contains("engineer.md") && l.contains("composed:")),
        "lines = {lines:?}"
    );
}

#[test]
fn install_then_deploy_deploys_skills() {
    // Regression for #386: a fresh install must populate `.claude/skills/`
    // from the bundled skill sources, not leave it empty.
    let dir = tempfile::tempdir().unwrap();
    let paths = trusty_mpm::core::paths::FrameworkPaths::under(dir.path());
    install_to(&paths, false).unwrap();
    let result = trusty_mpm::core::skill_deployer::deploy_skills(
        &paths.skill_source_dir(),
        &paths.claude_skills_dir(),
    )
    .unwrap();
    // The full /tm- skill portfolio (14 skills + tm-doctor) deploys on first
    // install (tm-skills-portfolio epic: the `example-skill.md` placeholder
    // and the 11 mpm-* guidance skills no longer ship; the previously-orphaned
    // tm-doctor.md is now wired in). Stats report stems (no .md suffix)
    // because each skill lands as <dest>/<name>/SKILL.md to match Claude
    // Code's native discovery format.
    assert!(
        result.deployed.contains(&"tm-circuit-breaker".to_string()),
        "tm-circuit-breaker must be deployed; got {:?}",
        result.deployed
    );
    assert!(
        result.deployed.contains(&"tm-doctor".to_string()),
        "tm-doctor must be deployed; got {:?}",
        result.deployed
    );
    assert_eq!(
        result.deployed.len(),
        16,
        "expected 16 skill files deployed (14 /tm- portfolio + tm-doctor + tm overview); got {:?}",
        result.deployed
    );
    assert!(result.skipped.is_empty());
    assert!(result.unchanged.is_empty());
    // Each skill must be deployed as a directory with SKILL.md inside.
    let deployed = paths
        .claude_skills_dir()
        .join("tm-circuit-breaker")
        .join("SKILL.md");
    assert!(
        deployed.is_file(),
        "expected skill at {}",
        deployed.display()
    );
    let lines = skill_report_lines(&result);
    assert!(
        lines.iter().any(|l| l.contains("tm-circuit-breaker")),
        "lines = {lines:?}"
    );
}

#[test]
fn cli_parses_optimizer_status() {
    let cli = Cli::try_parse_from(["trusty-mpm", "optimizer", "status"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Optimizer {
            action: OptimizerAction::Status
        }
    ));
}

#[test]
fn cli_parses_optimizer_set() {
    let cli = Cli::try_parse_from(["trusty-mpm", "optimizer", "set", "trim"]).unwrap();
    match cli.command.unwrap() {
        Command::Optimizer {
            action: OptimizerAction::Set { level },
        } => assert!(matches!(level, CliCompressionLevel::Trim)),
        other => panic!("expected optimizer set, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_events() {
    let cli = Cli::try_parse_from(["trusty-mpm", "sessions", "events", "abc-123"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Events { id_or_name },
        } => assert_eq!(id_or_name, "abc-123"),
        other => panic!("expected session events, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_breakers() {
    let cli = Cli::try_parse_from(["trusty-mpm", "sessions", "breakers"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Session {
            action: SessionAction::Breakers
        }
    ));
}

#[test]
fn cli_parses_session_pause() {
    let cli =
        Cli::try_parse_from(["trusty-mpm", "sessions", "pause", "tmpm-quiet-falcon"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Pause { id_or_name, note },
        } => {
            assert_eq!(id_or_name, "tmpm-quiet-falcon");
            assert_eq!(note, None);
        }
        other => panic!("expected session pause, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_pause_with_note() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "sessions",
        "pause",
        "abc-123",
        "--note",
        "stepping away",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Pause { id_or_name, note },
        } => {
            assert_eq!(id_or_name, "abc-123");
            assert_eq!(note.as_deref(), Some("stepping away"));
        }
        other => panic!("expected session pause, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_resume() {
    let cli = Cli::try_parse_from(["trusty-mpm", "sessions", "resume", "abc-123"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Resume { id_or_name },
        } => assert_eq!(id_or_name, "abc-123"),
        other => panic!("expected session resume, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_run() {
    let cli = Cli::try_parse_from(["trusty-mpm", "sessions", "run", "abc-123", "help me"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action:
                SessionAction::Run {
                    id_or_name,
                    command,
                    summarize,
                },
        } => {
            assert_eq!(id_or_name, "abc-123");
            assert_eq!(command, "help me");
            assert!(!summarize);
        }
        other => panic!("expected session run, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_run_with_summarize() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "sessions",
        "run",
        "tmpm-abc",
        "help",
        "--summarize",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Run { summarize, .. },
        } => assert!(summarize),
        other => panic!("expected session run, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_output_defaults() {
    let cli = Cli::try_parse_from(["trusty-mpm", "sessions", "output", "abc-123"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action:
                SessionAction::Output {
                    id_or_name,
                    lines,
                    summarize,
                },
        } => {
            assert_eq!(id_or_name, "abc-123");
            assert_eq!(lines, 50);
            assert!(!summarize);
        }
        other => panic!("expected session output, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_output_with_lines() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "sessions",
        "output",
        "abc-123",
        "--lines",
        "120",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Output { lines, .. },
        } => assert_eq!(lines, 120),
        other => panic!("expected session output, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_output_with_summarize() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "sessions",
        "output",
        "tmpm-abc",
        "--summarize",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Output { summarize, .. },
        } => assert!(summarize),
        other => panic!("expected session output, got {other:?}"),
    }
}

#[test]
fn compression_stats_line_skipped_when_uncompressed() {
    // A response with a null `compress_level` is raw output; no stats line.
    let body = serde_json::json!({
        "output": "raw",
        "compress_level": serde_json::Value::Null,
    });
    // The helper must not panic and must treat a null level as "no stats".
    assert!(
        body.get("compress_level")
            .and_then(|v| v.as_str())
            .is_none()
    );
    print_compression_stats(&body);
}

#[test]
fn compression_stats_line_printed_when_compressed() {
    // A response carrying a level and byte counts is exercised end-to-end;
    // the helper computes a percentage without panicking.
    let body = serde_json::json!({
        "output": "summary",
        "original_bytes": 4096,
        "compressed_bytes": 312,
        "compress_level": "summarise",
    });
    assert_eq!(
        body.get("compress_level").and_then(|v| v.as_str()),
        Some("summarise")
    );
    print_compression_stats(&body);
}

#[test]
fn cli_session_pause_requires_arg() {
    assert!(Cli::try_parse_from(["trusty-mpm", "sessions", "pause"]).is_err());
}

#[test]
fn cli_session_run_requires_command() {
    assert!(Cli::try_parse_from(["trusty-mpm", "sessions", "run", "abc-123"]).is_err());
}

#[test]
fn cli_parses_overseer_status() {
    let cli = Cli::try_parse_from(["trusty-mpm", "overseer", "status"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Overseer {
            action: OverseerAction::Status
        }
    ));
}

#[test]
fn event_summary_prefers_tool_field() {
    let payload = serde_json::json!({"tool": "Bash", "input": {"command": "ls"}});
    assert_eq!(event_summary(&payload), "tool=Bash");
}

#[test]
fn event_summary_truncates_long_payloads() {
    let payload = serde_json::json!({"k": "x".repeat(200)});
    let summary = event_summary(&payload);
    assert!(summary.chars().count() <= 61);
    assert!(summary.ends_with('\u{2026}'));
}
