//! #8206: project-side detection of a runnable test command, the precondition
//! the #2279 verify-before-finish trigger was missing.
//!
//! Why: [`super::names_test_command`] is a pure text match over the task
//! prompt and project context. On an EMPTY project root — a projectless
//! `tcode tui` run whose work root is a fresh scratch dir (#8205) — a prompt
//! that merely says "run `cargo test`" tripped the gate even though no
//! command could possibly have satisfied it, and the run wedged: the agent
//! was refused, could not comply, and was refused again. This module answers
//! the question the gate should have asked first — "is there anything here a
//! test command could run against?".
//! What: [`detect_test_command`] probes the bound project root for a manifest
//! that implies a specific test command (`Cargo.toml`, a `package.json` with
//! a `test` script, a pytest config, `go.mod`), falling back to the
//! prompt-named command when the root is non-empty but carries no recognised
//! manifest. A missing root, or an empty one, yields an
//! [`UndetectableReason`] instead — the gate turns that into an accepted
//! finish carrying a note, never a refusal.
//! Test: `verify_gate::tests::detect_*`.

use std::path::Path;

/// What a runnable test command was detected FROM, for the bound project.
///
/// Why: The refusal message must name the evidence so the agent knows what to
/// run — #8206's second closure condition. Carrying the manifest/command
/// through the detection result is what makes that message specific rather
/// than the pre-#8206 generic "a pytest/cargo/npm/pnpm/go test invocation".
/// What: `Manifest` when a root manifest implies a command; `PromptNamed`
/// when the root is non-empty but only the prompt names one.
/// Test: `verify_gate::tests::detect_manifest_names_each_ecosystem`,
/// `verify_gate::tests::detect_falls_back_to_prompt_named_on_non_empty_root`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestCommandTarget {
    /// A manifest at the project root implies this test command.
    Manifest {
        /// The manifest's filename, e.g. `Cargo.toml`.
        manifest: String,
        /// The command it implies, e.g. `cargo test`.
        command: String,
    },
    /// No recognised manifest, but the root is non-empty and the prompt names
    /// a command — the matched prompt text.
    PromptNamed {
        /// The command text matched in the prompt/project context.
        command: String,
    },
}

impl TestCommandTarget {
    /// The evidence phrase embedded in a refusal message.
    ///
    /// Why: #8206 closure condition 2 — a refusal must name the detected
    /// manifest or command.
    /// What: A clause reading naturally after "finish_task rejected: ".
    /// Test: `verify_gate::tests::default_gate_refusal_names_the_manifest`.
    pub fn evidence(&self) -> String {
        match self {
            Self::Manifest { manifest, command } => format!(
                "the project root's `{manifest}` implies a runnable test command (`{command}`)"
            ),
            Self::PromptNamed { command } => format!(
                "the task prompt or project context names a runnable test command (`{command}`)"
            ),
        }
    }
}

/// Why no test command could be detected — the reason the accepted finish
/// records instead of a refusal.
///
/// Why: #8206 closure condition 3 — `finish_task` is accepted in this arm and
/// the report states WHY no test command ran.
/// What: The three unsatisfiable states: no bound project, an empty root, or
/// an agent whose registry carries no `bash` tool to run anything with.
/// Test: `verify_gate::tests::detect_undetectable_without_root_or_on_empty_root`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndetectableReason {
    /// No project is bound to this run at all.
    NoProjectBound,
    /// A root is bound, but it holds no files.
    EmptyProjectRoot,
    /// The agent's tool registry carries no `bash` tool.
    NoBashTool,
}

impl UndetectableReason {
    /// One clause explaining this reason, for the accepted finish's note.
    ///
    /// Test: `verify_gate::tests::default_gate_accepts_on_empty_project_root`,
    /// `verify_gate::tests::default_gate_accepts_without_bash_tool`.
    pub fn explain(self) -> &'static str {
        match self {
            Self::NoProjectBound => {
                "no project is bound to this session, so no test command could run"
            }
            Self::EmptyProjectRoot => {
                "the bound project root is empty, so there is no test suite to run"
            }
            Self::NoBashTool => {
                "this agent's tool registry carries no `bash` tool, so no shell command \
                 could be run at all"
            }
        }
    }
}

/// Whether `root` holds a manifest implying a specific test command.
///
/// Why: A manifest is the strongest evidence a test command is actually
/// runnable here, and it names the command precisely enough to put in the
/// refusal.
/// What: First match wins, in the order Rust → Node → Python → Go. A
/// `package.json` counts only when it declares a non-empty `scripts.test`;
/// the command it implies is `pnpm test` when a `pnpm-lock.yaml` sits beside
/// it, `npm test` otherwise.
/// Test: `verify_gate::tests::detect_manifest_names_each_ecosystem`,
/// `verify_gate::tests::detect_package_json_without_test_script_is_not_a_manifest`.
fn manifest_command(root: &Path) -> Option<(String, String)> {
    if root.join("Cargo.toml").is_file() {
        return Some(("Cargo.toml".to_string(), "cargo test".to_string()));
    }
    if package_json_has_test_script(root) {
        let command = if root.join("pnpm-lock.yaml").is_file() {
            "pnpm test"
        } else {
            "npm test"
        };
        return Some(("package.json".to_string(), command.to_string()));
    }
    for config in ["pyproject.toml", "pytest.ini"] {
        if root.join(config).is_file() {
            return Some((config.to_string(), "pytest".to_string()));
        }
    }
    if root.join("go.mod").is_file() {
        return Some(("go.mod".to_string(), "go test ./...".to_string()));
    }
    None
}

/// Whether `root/package.json` declares a non-empty `scripts.test`.
///
/// Why: A `package.json` with no `test` script implies no runnable command —
/// `npm test` there exits non-zero with "Missing script", which is not a
/// suite the agent can reconcile.
/// What: Unreadable or unparsable JSON is `false` — fail toward accepting the
/// finish, never toward a refusal the agent cannot satisfy.
/// Test: `verify_gate::tests::detect_package_json_without_test_script_is_not_a_manifest`.
fn package_json_has_test_script(root: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(root.join("package.json")) else {
        return false;
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    parsed
        .get("scripts")
        .and_then(|s| s.get("test"))
        .and_then(|t| t.as_str())
        .is_some_and(|t| !t.trim().is_empty())
}

/// Whether `root` holds no entries at all (or cannot be read).
///
/// Why: An unreadable root is treated as empty for the same reason an empty
/// one is — the gate must not refuse a finish over a suite it cannot show is
/// there.
/// Test: `verify_gate::tests::detect_undetectable_without_root_or_on_empty_root`.
fn is_empty_dir(root: &Path) -> bool {
    match std::fs::read_dir(root) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => true,
    }
}

/// Detect the test command the gate would require, for the bound project.
///
/// Why: The missing half of #2279's trigger — the gate may only refuse a
/// finish over a command that could actually have been run here (#8206).
/// What: `Err(UndetectableReason)` when no root is bound or the root is
/// empty; otherwise `Ok` with the manifest-implied command, or the
/// `prompt_named` text when the non-empty root carries no recognised
/// manifest. `prompt_named` is the substring `super::named_test_command`
/// matched in the task prompt / project context.
/// Test: `verify_gate::tests::detect_manifest_names_each_ecosystem`,
/// `verify_gate::tests::detect_undetectable_without_root_or_on_empty_root`,
/// `verify_gate::tests::detect_falls_back_to_prompt_named_on_non_empty_root`.
pub fn detect_test_command(
    root: Option<&Path>,
    prompt_named: &str,
) -> Result<TestCommandTarget, UndetectableReason> {
    let Some(root) = root else {
        return Err(UndetectableReason::NoProjectBound);
    };
    if let Some((manifest, command)) = manifest_command(root) {
        return Ok(TestCommandTarget::Manifest { manifest, command });
    }
    if is_empty_dir(root) {
        return Err(UndetectableReason::EmptyProjectRoot);
    }
    Ok(TestCommandTarget::PromptNamed {
        command: prompt_named.to_string(),
    })
}
