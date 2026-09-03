//! A mockable process-runner seam for `tm ticket` (+ `watch`/`issue`).
//!
//! Why: the ticket workflow shells out to `gh` (issue fetch/comment/PR) and
//! `git` (default-branch lookup). Hiding those calls behind a trait lets the
//! orchestration logic be unit-tested with a scripted fake instead of a live
//! GitHub repo + network, while the production path uses the real binaries.
//! This boundary is ALSO where #1265's per-project GitHub identity binding is
//! applied: [`RealCommandRunner`] carries the resolved env overrides (from the
//! `gh_identity` module) and sets them on every spawned `Command`, so the
//! identity is bound centrally rather than at each of `tm`'s many `gh` call
//! sites. config_dir/token_env bind the identity WITHOUT mutating global gh
//! state: they set `GH_CONFIG_DIR`/`GH_TOKEN`/`GH_HOST` on the child, and
//! (#6668) remove from that child every other identity var gh would otherwise
//! read ahead of them.
//! What: the [`CommandRunner`] trait with one `run` method returning captured
//! stdout/stderr/exit-status, a [`CommandOutput`] value type, and the
//! [`RealCommandRunner`] that executes via `std::process::Command` after applying
//! any per-project env overrides.
//! Test: env application is covered by `real_runner_applies_env_overrides`; the
//! trait is driven by `FakeRunner` in `system.rs`/`tests` modules.

use anyhow::Context as _;

/// Captured result of running an external command.
///
/// Why: callers need the exit status AND the captured streams (stdout for issue
/// JSON / PR URLs, stderr for actionable error messages) in one value so the
/// fake and real runners are interchangeable.
/// What: holds whether the command succeeded, its stdout, and its stderr as
/// UTF-8 lossy strings.
/// Test: constructed by both runners; asserted across the orchestration tests.
#[derive(Debug, Clone)]
pub(crate) struct CommandOutput {
    /// Whether the process exited 0.
    pub(crate) success: bool,
    /// Captured stdout (UTF-8 lossy).
    pub(crate) stdout: String,
    /// Captured stderr (UTF-8 lossy).
    pub(crate) stderr: String,
}

impl CommandOutput {
    /// Return the trimmed stdout, or an error carrying stderr when the command
    /// failed.
    ///
    /// Why: most call sites want "the output if it worked, else a useful error";
    /// folding the status check here keeps every caller from repeating it.
    /// What: returns `Ok(stdout.trim())` on success, else an `anyhow` error that
    /// names the program and includes the captured stderr.
    /// Test: `command_output_ok_returns_stdout`,
    /// `command_output_err_includes_stderr` in `system.rs`.
    pub(crate) fn ok_or_stderr(&self, program: &str) -> anyhow::Result<String> {
        if self.success {
            Ok(self.stdout.trim().to_string())
        } else {
            let detail = self.stderr.trim();
            anyhow::bail!(
                "`{program}` failed{}",
                if detail.is_empty() {
                    String::new()
                } else {
                    format!(": {detail}")
                }
            )
        }
    }
}

/// A seam for running external programs (`gh`, `git`).
///
/// Why: decouples the ticket orchestration from real process spawning so the
/// logic is testable without GitHub or a git checkout, mirroring the
/// runtime-adapter trait seam used elsewhere in trusty-mpm.
/// What: a single `run(program, args)` method returning a [`CommandOutput`].
/// Implementors capture stdout/stderr and never inherit the parent's streams.
/// Test: `RealCommandRunner` via the live path; `FakeRunner` in the unit tests.
pub(crate) trait CommandRunner {
    /// Run `program` with `args`, capturing stdout/stderr.
    fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<CommandOutput>;
}

/// Production [`CommandRunner`] that spawns real processes.
///
/// Why: the live `tm ticket`/`watch`/`issue` paths must actually invoke `gh` and
/// `git`. This is the centralised boundary where #1265's per-project GitHub
/// identity is applied: any resolved env overrides (e.g. `GH_CONFIG_DIR`,
/// `GH_TOKEN`, `GH_HOST`) are set on the spawned child only — never on the
/// parent process — so the binding does not mutate the operator's global gh
/// state and cannot leak across unrelated invocations.
/// What: holds an ordered list of `(name, value)` env overrides plus the #6668
/// removal list. The unit-style `RealCommandRunner::default()` carries NEITHER
/// (ambient identity, pre-#1265 behaviour); [`RealCommandRunner::with_gh_env`]
/// binds a resolved set. `run` spawns the program via
/// `std::process::Command::output` with the removals applied first and the
/// overrides second, capturing both streams; an inability to spawn (e.g. binary
/// not installed) is surfaced as an actionable `anyhow` error rather than a
/// panic.
/// Test: `real_runner_applies_env_overrides` asserts the child sees the vars; the
/// no-override default is exercised by every existing live path.
#[derive(Debug, Clone, Default)]
pub(crate) struct RealCommandRunner {
    /// Per-project env overrides applied to each spawned child (#1265).
    env: Vec<(String, String)>,
    /// #6668: inherited identity vars removed from each child before `env` is
    /// applied, so a shell-exported `GH_TOKEN` cannot outrank the binding.
    unset: Vec<String>,
}

impl RealCommandRunner {
    /// Construct a runner bound to a resolved
    /// [`GhEnv`](trusty_mpm::core::gh_identity::GhEnv) (#6668).
    ///
    /// Why: the constructor this replaces took only the vars to SET, which is
    /// what let an inherited `GH_TOKEN` beat a per-project `GH_CONFIG_DIR`.
    /// Taking the whole `GhEnv` carries the removals with it, so a call site
    /// cannot bind half the identity.
    /// What: stores both the overrides and the removal list.
    /// Test: `real_runner_applies_env_overrides`.
    pub(crate) fn with_gh_env(gh_env: &trusty_mpm::core::gh_identity::GhEnv) -> Self {
        Self {
            env: gh_env.vars().to_vec(),
            unset: gh_env.unset_vars().to_vec(),
        }
    }
}

impl CommandRunner for RealCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<CommandOutput> {
        let mut cmd = std::process::Command::new(program);
        cmd.args(args);
        // #6668: removals first, then the overrides.
        for k in &self.unset {
            cmd.env_remove(k);
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        let output = cmd.output().with_context(|| {
            format!("failed to run `{program}` — is it installed and on your PATH?")
        })?;
        Ok(CommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the central #1265 guarantee — env overrides bound to a
    /// [`RealCommandRunner`] are actually applied to the spawned child (and only
    /// the child, not the parent). We prove it by running `printenv` for the
    /// var a `config_dir` binding resolves to and asserting the child echoes
    /// the value. #6668 adds the other half: the same binding must strip the
    /// inherited `GH_TOKEN`, which `printenv GH_TOKEN` proves by exiting
    /// non-zero even when this process has one.
    /// Test: itself (uses the always-present `/usr/bin/env`-style `printenv`).
    #[test]
    fn real_runner_applies_env_overrides() {
        let gh_env = trusty_mpm::core::gh_identity::resolve_gh_env(Some(
            &trusty_mpm::core::trusty_tools_config::GithubConfig {
                config_dir: Some(std::path::PathBuf::from("/cfg/project")),
                ..Default::default()
            },
        ))
        .expect("a config_dir binding resolves");
        let runner = RealCommandRunner::with_gh_env(&gh_env);
        // `printenv VAR` prints the value and exits 0 when set. It is present on
        // Linux and macOS; skip gracefully if the platform lacks it.
        match runner.run("printenv", &["GH_CONFIG_DIR"]) {
            Ok(out) => {
                assert!(out.success, "printenv failed: {}", out.stderr);
                assert_eq!(out.stdout.trim(), "/cfg/project");
            }
            Err(_) => {
                // `printenv` not available — the env-application path is still
                // covered by the gh_identity resolution tests; nothing to assert.
            }
        }
        if let Ok(out) = runner.run("printenv", &["GH_TOKEN"]) {
            assert!(
                !out.success,
                "#6668: a bound child must not inherit GH_TOKEN, got {:?}",
                out.stdout
            );
        }
        // Neither the override nor the removal may leak into the parent.
        assert_ne!(
            std::env::var("GH_CONFIG_DIR").ok().as_deref(),
            Some("/cfg/project"),
            "the binding must apply to the child only"
        );
    }

    /// Why: a default/ambient runner must apply no overrides (no regression).
    /// Test: itself.
    #[test]
    fn real_runner_default_has_no_env() {
        let runner = RealCommandRunner::default();
        assert!(runner.env.is_empty());
    }
}
