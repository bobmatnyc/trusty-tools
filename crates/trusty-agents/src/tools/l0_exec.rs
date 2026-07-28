//! `l0_shell_exec` — the L0-orchestration-tier shell/build/test execution
//! grant (#4173, epic #4167).
//!
//! Why: L0 ("orchestration assistant") has to orchestrate fix rounds, verify
//! in-flight PRs and run build/test commands; #4167's gap analysis names "no
//! shell/build/test execution grant" as one of the four reasons an L1 persona
//! cannot do PM-tier work. Shell execution is ALSO the capability that made
//! #4126 a P0: untrusted Gmail/Drive content reached a persona, the persona
//! delegated to `engineer`, and `engineer` had an ungated shell under
//! `runner = "claude-code"` — prompt injection to code execution. PR #4161
//! closed that path and PR #4200 added the L0/L1 tier boundary; this module
//! deliberately hands shell BACK to exactly one tier, and does it behind a
//! single code-level gate rather than a convention.
//!
//! THE SECURITY BOUNDARY IS THE TIER GATE — [`l0_execution_tools`] — NOT the
//! command text. L0 is the owner's explicitly-accepted YOLO tier and is
//! deliberately NOT sandboxed (#4167); what makes an unsandboxed shell
//! acceptable there is that L0 must never ingest untrusted content (no
//! Gmail/Drive/Calendar surface), whereas L1 holds the full Google surface and
//! therefore never gets this tool. Two narrower, independent guards are layered
//! on top and are NOT the boundary either: the catastrophic-pattern predicate
//! reused from [`crate::tools::run_bash::is_dangerous_command`] (defense in
//! depth) and the cwd containment in [`resolve_working_dir`] (a per-call
//! starting-directory check — a general shell can still `cd` once it is
//! running, which is inherent to granting a shell at all and is why the tier
//! gate, not this function, is what keeps L1 out).
//!
//! What: [`L0ShellExecTool`] implements `ToolExecutor` under the name
//! `l0_shell_exec` (a name distinct from the existing `run_bash` /
//! `shell_exec` / `pytest_exec` executors so the L0-only surface is greppable
//! and can never be confused with a grant some other agent already holds).
//! [`l0_execution_tools`] is the ONE place that decides whether that tool
//! exists in a registry at all: it returns it for
//! [`AgentTier::L0Orchestration`] and an empty vector for
//! [`AgentTier::L1Standard`].
//! Test: `l0_execution_tools_denies_l1_tier`,
//! `l0_execution_tools_grants_l0_tier`,
//! `l0_execution_tools_denies_every_fail_closed_tier_string`,
//! `l0_shell_exec_runs_echo`,
//! `l0_shell_exec_refuses_working_dir_outside_the_root`,
//! `l0_shell_exec_denied_to_read_only_service_tier` (all in the sibling
//! `l0_exec_tests.rs`); registry-construction and delegation-composition
//! coverage lives alongside the gate it wires, in
//! `runtime/tool_registry_tests.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;

use crate::agents::AgentTier;
use crate::rbac::ServiceTier;
use crate::tools::traits::{ToolExecutor, ToolResult};

/// The dispatch name of the L0 execution grant.
///
/// Why: Referenced by the registry-construction gate, its tests, and the
/// `skills/manifest/builtin/ops.rs` catalog row; a `const` keeps those from
/// drifting apart by a typo.
pub const L0_SHELL_EXEC: &str = "l0_shell_exec";

/// Per-command wall-clock budget.
///
/// Why: `run_bash`'s 30s is tuned for coordinator one-liners (`git status`)
/// and would time out on the very commands #4173 exists for — `cargo build`,
/// `cargo test -p <crate>`. 10 minutes covers a cold workspace test run while
/// still bounding a hung command. Raising `run_bash`'s own constant instead was
/// rejected: it is shared with `ctrl`/`pm` and changing their timeout is not
/// this issue's business.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(600);

/// Combined stdout+stderr cap.
///
/// Why: #4173 asks for output that is "legible and unfiltered"; a build/test
/// log is far larger than `run_bash`'s 8 KiB coordinator cap, so this is 4x
/// wider — but still bounded, because an unbounded `cargo test` log would blow
/// the context window and the truncation marker tells the model what it lost.
const MAX_OUTPUT_CHARS: usize = 32_768;

/// The L0-tier execution tool set for a registry rooted at `root`.
///
/// Why: THE gate. #4173's hard requirement is that this grant is L0-only
/// *enforced in code through #4200's fail-closed tier resolver*, never by
/// convention or by documentation, and never reachable by an L1 persona that
/// merely declares the tool in its `[tools].allow`. Concentrating the decision
/// in one function means every assembly point (today
/// `runtime::tool_registry::build_assistant_tier_registry`) shares one
/// definition of "who gets a shell", and a reviewer has exactly one place to
/// check. Fail-closed comes free from [`crate::agents::AgentInfo::tier`]:
/// absent, blank and unrecognized `tier` values all resolve to
/// [`AgentTier::L1Standard`] before they ever reach this function, so an
/// indeterminate tier is DENIED.
/// What: `L0Orchestration` -> a one-element vector holding
/// [`L0ShellExecTool`] rooted at `root`; `L1Standard` -> an empty vector, so
/// the tool is never registered, never appears in `ToolRegistry::schemas()`,
/// and therefore can never survive the `[tools].allow` glob intersection in
/// `runtime::tool_registry::scope_assistant_allowed_tools` (nor be reached by
/// `dispatch`, which has nothing registered under the name). The match is
/// deliberately exhaustive with NO wildcard arm: adding a third tier must be a
/// compile error that forces an explicit grant/deny decision here, not a
/// silent fall-through in either direction.
/// Test: `l0_execution_tools_denies_l1_tier`,
/// `l0_execution_tools_grants_l0_tier`,
/// `l0_execution_tools_denies_every_fail_closed_tier_string`.
pub fn l0_execution_tools(tier: AgentTier, root: PathBuf) -> Vec<Arc<dyn ToolExecutor>> {
    match tier {
        AgentTier::L0Orchestration => vec![Arc::new(L0ShellExecTool::new(root))],
        AgentTier::L1Standard => Vec::new(),
    }
}

/// Shell/build/test executor for the L0 orchestration tier.
///
/// Why: See the module docs — this exists so L0 can run `cargo test`, drive a
/// fix round and verify a PR without telling the owner to run the command.
/// What: Holds the project `root` that every command starts in. Commands run
/// via `sh -c` with [`COMMAND_TIMEOUT`]; stdout and stderr are combined and
/// truncated at [`MAX_OUTPUT_CHARS`]; the exit code is always reported.
/// Test: See the module-level `Test:` pointer.
pub struct L0ShellExecTool {
    /// The project root every command starts in, and the containment boundary
    /// for a caller-supplied `working_dir` (see [`resolve_working_dir`]).
    pub root: PathBuf,
}

impl L0ShellExecTool {
    /// Construct the tool rooted at `root`.
    ///
    /// Why: Binding the root at construction (rather than reading the process
    /// CWD inside `execute`) means the containment check cannot be moved by a
    /// later `set_current_dir` anywhere else in the process.
    /// What: Stores `root` verbatim; it is canonicalized per call so a root
    /// that is created or replaced after construction still resolves.
    /// Test: `l0_shell_exec_runs_in_the_root_by_default`.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

/// Resolve the directory a command should run in, refusing anything outside
/// `root`.
///
/// Why: #4173 asks for execution "scoped to project directories" that "cannot
/// escape (containment via cwd)". A pure predicate keeps that rule unit-
/// testable and keeps the traversal cases (`..`, an absolute path elsewhere, a
/// symlink pointing out of the tree) in one place. It is a per-call STARTING
/// directory check, not a sandbox: see the module docs.
/// What: `None`/blank -> `root`. Otherwise the request is joined onto `root`
/// when relative, then both sides are canonicalized (which resolves `..` and
/// symlinks) and the result must be `root` itself or a descendant. A path that
/// does not exist, or one that escapes, returns `Err(reason)`. The reason
/// names the offending REQUEST, never the absolute root, so the message stays
/// safe to surface.
/// Test: `l0_shell_exec_accepts_working_dir_inside_the_root`,
/// `l0_shell_exec_refuses_working_dir_outside_the_root`,
/// `l0_shell_exec_refuses_parent_traversal_working_dir`,
/// `l0_shell_exec_refuses_nonexistent_working_dir`,
/// `resolve_working_dir_defaults_to_the_root`.
pub fn resolve_working_dir(root: &Path, requested: Option<&str>) -> Result<PathBuf, String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("project root is not resolvable: {e}"))?;
    let Some(req) = requested.map(str::trim).filter(|r| !r.is_empty()) else {
        return Ok(canonical_root);
    };
    let candidate = {
        let p = Path::new(req);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            canonical_root.join(p)
        }
    };
    let canonical = candidate
        .canonicalize()
        .map_err(|e| format!("working_dir '{req}' is not a resolvable directory: {e}"))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(format!(
            "working_dir '{req}' resolves outside the project root; \
             execution is contained to the project tree"
        ));
    }
    Ok(canonical)
}

#[async_trait]
impl ToolExecutor for L0ShellExecTool {
    fn name(&self) -> &str {
        L0_SHELL_EXEC
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": L0_SHELL_EXEC,
                "description": "Run a shell command in the project working tree — builds, \
                                test suites, git and gh operations, scripts. Returns the exit \
                                code with stdout and stderr combined and unfiltered. Commands \
                                start in the project root; an optional working_dir must stay \
                                inside it.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to run, e.g. 'cargo test -p trusty-agents'."
                        },
                        "working_dir": {
                            "type": "string",
                            "description": "Optional directory to run in. Relative to the project root; must resolve inside it."
                        }
                    },
                    "required": ["command"],
                    "additionalProperties": false
                }
            }
        })
    }

    /// Deny the two non-operator transports outright.
    ///
    /// Why: An independent second dimension from the persona tier gate. Even
    /// inside a legitimately L0 session, a request arriving over a read-only
    /// or analytics-only transport (an unauthenticated HTTP caller, a guest
    /// Slack user — see `crate::rbac`) must not reach an unsandboxed shell.
    /// Mirrors `tools::pm_bridge`'s owner-locked posture and is enforced at
    /// the dispatch boundary by `ToolRegistry::dispatch_for_user`, so no
    /// transport author can bypass it by forgetting to pre-filter.
    /// What: Only `ServiceTier::All` (the authenticated operator/controller)
    /// may invoke this tool.
    /// Test: `l0_shell_exec_denied_to_read_only_service_tier`,
    /// `l0_shell_exec_denied_to_analytics_service_tier`,
    /// `l0_shell_exec_allowed_for_operator_service_tier`.
    fn restricted_tiers(&self) -> &[ServiceTier] {
        &[ServiceTier::ReadOnly, ServiceTier::Analytics]
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let command = match args.get("command").and_then(Value::as_str) {
            Some(c) if !c.trim().is_empty() => c.to_string(),
            _ => return ToolResult::err("l0_shell_exec: 'command' is required"),
        };

        // Defense in depth, NOT the security boundary (see module docs). The
        // predicate is reused from `run_bash` rather than re-listed here so
        // there is one catastrophic-pattern list in the crate.
        if let Err(reason) = crate::tools::run_bash::is_dangerous_command(&command) {
            return ToolResult::err(format!("l0_shell_exec blocked: {reason}"));
        }

        let work_dir = match resolve_working_dir(
            &self.root,
            args.get("working_dir").and_then(Value::as_str),
        ) {
            Ok(dir) => dir,
            Err(reason) => return ToolResult::err(format!("l0_shell_exec: {reason}")),
        };

        let spawn = Command::new("sh")
            .arg("-c")
            .arg(&command)
            .current_dir(&work_dir)
            .output();

        match tokio::time::timeout(COMMAND_TIMEOUT, spawn).await {
            Err(_) => ToolResult::err(format!(
                "l0_shell_exec: command timed out after {}s",
                COMMAND_TIMEOUT.as_secs()
            )),
            Ok(Err(e)) => ToolResult::err(format!("l0_shell_exec: failed to spawn: {e}")),
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let code = output.status.code().unwrap_or(-1);
                let combined = format!("{stdout}{stderr}");
                let body = crate::tools::run_bash::truncate(&combined, MAX_OUTPUT_CHARS);
                ToolResult::ok(format!("[exit {code}]\n{body}"))
            }
        }
    }
}

#[cfg(test)]
#[path = "l0_exec_tests.rs"]
mod tests;
