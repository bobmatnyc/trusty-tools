//! `advance_workflow_phase` tool — appends a workflow-phase audit record.
//!
//! Why: When agents drive their own phase transitions, we need an immutable
//! audit trail describing who advanced what and why. A JSONL file under the
//! workflow's `out_dir` gives us append-only, grep-friendly history without
//! adding a database dependency.
//! What: `PhaseAuditTool` holds an `out_dir: PathBuf` and appends one JSON
//! line (`{timestamp, phase, reason}`) to `{out_dir}/phase-audit.jsonl` per
//! call. It's a `ToolExecutor` so it plugs into the existing per-agent
//! registry.
//! Test: Construct against a tempdir, call `execute`, assert the JSONL file
//! has the expected line.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{ToolExecutor, ToolResult};

/// Audit tool that appends a single JSON line per invocation.
pub struct PhaseAuditTool {
    out_dir: PathBuf,
}

impl PhaseAuditTool {
    /// Construct against an explicit output directory.
    ///
    /// Why: The workflow engine knows the per-run `out_dir`; injecting it
    /// keeps the tool free of env/CWD lookups.
    /// What: Stores the path for later appends.
    /// Test: Used by unit test below.
    pub fn new(out_dir: PathBuf) -> Self {
        Self { out_dir }
    }

    fn audit_path(&self) -> PathBuf {
        self.out_dir.join("phase-audit.jsonl")
    }
}

#[async_trait]
impl ToolExecutor for PhaseAuditTool {
    fn name(&self) -> &str {
        "advance_workflow_phase"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "advance_workflow_phase",
                "description": "Record that the current workflow phase should advance. Appends a JSONL audit entry; does not itself change workflow state.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "phase": {
                            "type": "string",
                            "description": "Name of the phase being advanced (e.g. 'research')."
                        },
                        "reason": {
                            "type": "string",
                            "description": "Short free-text rationale for advancing now."
                        }
                    },
                    "required": ["phase", "reason"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(phase) = args.get("phase").and_then(Value::as_str) else {
            return ToolResult::err("advance_workflow_phase: missing 'phase'");
        };
        let Some(reason) = args.get("reason").and_then(Value::as_str) else {
            return ToolResult::err("advance_workflow_phase: missing 'reason'");
        };
        let timestamp = chrono::Utc::now().to_rfc3339();
        let entry = json!({
            "timestamp": timestamp,
            "phase": phase,
            "reason": reason,
        });
        let mut line = serde_json::to_string(&entry).unwrap_or_else(|_| "{}".to_string());
        line.push('\n');

        if let Err(e) = tokio::fs::create_dir_all(&self.out_dir).await {
            return ToolResult::err(format!(
                "advance_workflow_phase: failed to create out_dir {}: {e:#}",
                self.out_dir.display()
            ));
        }
        let path = self.audit_path();
        // Append (create if missing). Using std::fs::OpenOptions for append.
        let write = tokio::task::spawn_blocking({
            let path = path.clone();
            let line = line.clone();
            move || {
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)?;
                f.write_all(line.as_bytes())?;
                Ok::<(), std::io::Error>(())
            }
        })
        .await;
        match write {
            Ok(Ok(())) => ToolResult::ok(format!("recorded phase '{phase}'")),
            Ok(Err(e)) => ToolResult::err(format!(
                "advance_workflow_phase: failed to append to {}: {e:#}",
                path.display()
            )),
            Err(e) => ToolResult::err(format!("advance_workflow_phase: join error: {e:#}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "trusty-agents-phase-audit-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[tokio::test]
    async fn appends_jsonl_line() {
        let dir = tempdir();
        let tool = PhaseAuditTool::new(dir.clone());
        let out = tool
            .execute(json!({"phase": "research", "reason": "have enough context"}))
            .await;
        assert!(!out.is_error(), "unexpected error: {}", out.content());

        let path = dir.join("phase-audit.jsonl");
        let contents = std::fs::read_to_string(&path).expect("audit file exists");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1);
        let parsed: Value = serde_json::from_str(lines[0]).expect("valid json");
        assert_eq!(parsed["phase"], "research");
        assert_eq!(parsed["reason"], "have enough context");
        assert!(parsed["timestamp"].as_str().is_some());
    }

    #[tokio::test]
    async fn missing_args_return_error() {
        let dir = tempdir();
        let tool = PhaseAuditTool::new(dir);
        let out = tool.execute(json!({"phase": "x"})).await;
        assert!(out.is_error());
        assert!(out.content().contains("reason"));
    }

    #[tokio::test]
    async fn multiple_calls_append() {
        let dir = tempdir();
        let tool = PhaseAuditTool::new(dir.clone());
        for i in 0..3 {
            let r = tool
                .execute(json!({"phase": format!("phase-{i}"), "reason": "x"}))
                .await;
            assert!(!r.is_error());
        }
        let contents = std::fs::read_to_string(dir.join("phase-audit.jsonl")).unwrap();
        assert_eq!(contents.lines().count(), 3);
    }
}
