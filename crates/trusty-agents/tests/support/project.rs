//! `Project` — integration-test fixture that runs the trusty-agents binary
//! against a freshly-constructed tempdir with the repo-bundled `.trusty-agents/`
//! config copied in.
//!
//! Why: End-to-end CLI tests (`inspect --dry-run`, eventually full workflows)
//! need a reproducible workspace where the binary can find agent + skill
//! configs and write output without polluting the repo. Centralising the
//! "copy bundled config + spawn binary" dance in one place keeps individual
//! tests trivial.
//! What: `Project::new()` creates a tempdir, copies
//! `<repo>/.trusty-agents` into `<tempdir>/.trusty-agents`, and remembers the path
//! to the compiled `trusty-agents` binary via `env!("CARGO_BIN_EXE_tagent")`.
//! `run_inspect` shells the binary in `inspect --task <text> --dry-run` mode
//! and parses stdout as JSON. `run_task` is provided for future use.
//! Test: Exercised by `tests/cli_project.rs`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// One-shot test workspace + handle to the compiled `trusty-agents` binary.
pub struct Project {
    pub root: TempDir,
    pub binary: PathBuf,
}

/// Parsed result of running the binary in workflow mode.
#[derive(Debug)]
pub struct TaskResult {
    pub status: String,
    pub narrative: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Project {
    /// Build a fresh tempdir with the repo-bundled `.trusty-agents` config copied
    /// into it.
    ///
    /// Why: `default_bundled_config_dir` resolves to `.trusty-agents` relative to
    /// the process cwd; copying the bundled tree into the tempdir makes the
    /// binary self-contained for the test run.
    /// What: Locates the manifest dir via `CARGO_MANIFEST_DIR`, copies
    /// `<manifest>/.trusty-agents` recursively into `<tempdir>/.trusty-agents`, and
    /// reads the binary path from `CARGO_BIN_EXE_tagent` (set by Cargo
    /// for integration tests).
    /// Test: Implicit via the integration tests in `cli_project.rs`.
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("create tempdir");
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let src_cfg = manifest.join(".trusty-agents");
        let dst_cfg = root.path().join(".trusty-agents");
        copy_dir_recursive(&src_cfg, &dst_cfg).expect("copy .trusty-agents");
        let binary = PathBuf::from(env!("CARGO_BIN_EXE_tagent"));
        Self { root, binary }
    }

    /// Run the binary in workflow mode against this tempdir.
    ///
    /// Why: Integration tests for full workflow runs need a single helper that
    /// spawns the binary, pipes the task, and projects exit + parsed output
    /// into a `TaskResult` value.
    /// What: Spawns `trusty-agents --workflow <workflow> --out-dir <root>/out`
    /// with `--task <task>` inline, captures stdout/stderr, and returns a
    /// `TaskResult` populated from the binary's exit code. `status` is
    /// derived as `success` on exit code 0 else `failed`. Requires
    /// `OPENROUTER_API_KEY` (or `ANTHROPIC_API_KEY`) at runtime; not used
    /// by the dry-run tests in this PR.
    /// Test: Reserved for future LLM-backed integration tests.
    pub async fn run_task(&self, task: &str, workflow: &str) -> Result<TaskResult> {
        let out_dir = self.root.path().join("out");
        std::fs::create_dir_all(&out_dir).ok();
        let mut child = Command::new(&self.binary)
            .current_dir(self.root.path())
            .arg("--workflow")
            .arg(workflow)
            .arg("--task")
            .arg(task)
            .arg("--out-dir")
            .arg(&out_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawn trusty-agents")?;
        // Close stdin to signal EOF (binary may also read piped task).
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.shutdown().await;
        }
        let output = child
            .wait_with_output()
            .await
            .context("wait for trusty-agents")?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);
        let status = if exit_code == 0 { "success" } else { "failed" };
        Ok(TaskResult {
            status: status.to_string(),
            narrative: stdout.clone(),
            exit_code,
            stdout,
            stderr,
        })
    }

    /// Run the binary in dry-run inspection mode and return the parsed JSON
    /// report.
    ///
    /// Why: Dry-run inspection is pure (no LLM calls), so it's the fastest
    /// way to assert that registries and signal extraction wire up correctly
    /// against a real config tree.
    /// What: Invokes `trusty-agents inspect --task <task> --dry-run` with cwd =
    /// the tempdir, captures stdout, parses as JSON. Returns an error if
    /// the binary exits non-zero or stdout fails to parse.
    /// Test: Exercised by `project_inspect_returns_matched_skills` and
    /// `project_inspect_returns_agent_match`.
    pub async fn run_inspect(&self, task: &str) -> Result<Value> {
        let output = Command::new(&self.binary)
            .current_dir(self.root.path())
            .arg("inspect")
            .arg("--task")
            .arg(task)
            .arg("--dry-run")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("spawn trusty-agents inspect")?;
        if !output.status.success() {
            anyhow::bail!(
                "trusty-agents inspect failed (exit={:?}): {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let v: Value = serde_json::from_str(&stdout)
            .with_context(|| format!("parse inspect JSON; stdout was: {stdout}"))?;
        Ok(v)
    }
}

/// Recursive directory copy.
///
/// Why: `std::fs` has no built-in recursive copy; bringing in `fs_extra`
/// just for tests would inflate the dep tree.
/// What: Walks `src` with a manual stack, mirroring directories and
/// copying files into `dst`.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    if !src.exists() {
        anyhow::bail!("source config dir not found: {}", src.display());
    }
    std::fs::create_dir_all(dst)?;
    let mut stack: Vec<(PathBuf, PathBuf)> = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((s, d)) = stack.pop() {
        for entry in std::fs::read_dir(&s)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            let from = entry.path();
            let to = d.join(entry.file_name());
            if ft.is_dir() {
                std::fs::create_dir_all(&to)?;
                stack.push((from, to));
            } else if ft.is_file() {
                std::fs::copy(&from, &to)?;
            }
            // Symlinks/other: skip.
        }
    }
    Ok(())
}
