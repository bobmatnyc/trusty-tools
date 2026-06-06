//! Workflow runtime context: task text, out_dir, and collected phase outputs.
//!
//! Why: Passing a single mutable context through the phase loop is simpler
//! than threading scattered arguments. The builder pattern keeps initial
//! construction (immutable inputs) separate from progressive accumulation
//! (phase outputs).
//! What: `WorkflowContext` holds task + out_dir + `HashMap<String,String>`
//! of phase outputs. `render_template` does `{{key}}` substitution.
//! Test: `render_template` with nested keys and empty hashmap.

use std::collections::HashMap;
use std::path::PathBuf;

/// Immutable + progressively-filled state for a running workflow.
///
/// `phase_outputs` holds the FULL content of each phase (used for disk
/// extraction and debugging). `phase_summaries` holds the extracted summary
/// from each phase — these are what flow into downstream `{{phase_name}}`
/// template substitutions (issue #27).
#[derive(Debug, Clone)]
pub struct WorkflowContext {
    pub task: String,
    pub out_dir: Option<PathBuf>,
    pub phase_outputs: HashMap<String, String>,
    pub phase_summaries: HashMap<String, String>,
    /// Parsed `GoalBlock` emitted by the planner (#68). Injected at the top
    /// of every downstream agent's prompt once populated.
    pub goal_block: Option<crate::context::GoalBlock>,
    /// #140: Auto-discovered project root (the directory containing
    /// `pyproject.toml`, typically a subdirectory of `out_dir`). Populated by
    /// the engine after the code phase writes files, and exposed to
    /// downstream phase templates as `{{project_dir}}`. Falls back to
    /// `out_dir` when no project subdirectory is detected so templates that
    /// use `{{project_dir}}` remain correct even when files land directly in
    /// `out_dir`.
    pub project_dir: Option<PathBuf>,
}

impl WorkflowContext {
    pub fn builder(task: impl Into<String>) -> WorkflowContextBuilder {
        WorkflowContextBuilder::new(task)
    }

    /// Record a phase's full textual output (summary defaulted to content).
    ///
    /// Why: Preserves the pre-#27 behavior for test helpers that don't care
    /// about summarization.
    /// What: Stores `output` in both `phase_outputs` and `phase_summaries`.
    /// Test: See `render_template_substitutes_values`.
    #[allow(dead_code)]
    pub fn record_phase_output(&mut self, phase: &str, output: String) {
        self.phase_outputs.insert(phase.to_string(), output.clone());
        self.phase_summaries.insert(phase.to_string(), output);
    }

    /// Record both the full content AND the extracted summary for a phase.
    ///
    /// Why: #27 — downstream templates need the summary, not 30k chars of code.
    /// What: `content` goes into `phase_outputs`; `summary` (or content if
    /// summary is None) goes into `phase_summaries` which is what
    /// `render_template` reads for `{{phase_name}}` substitution.
    /// Test: `render_uses_summary_not_content`.
    pub fn record_phase(&mut self, phase: &str, content: String, summary: Option<String>) {
        let summary_val = summary.unwrap_or_else(|| content.clone());
        self.phase_outputs.insert(phase.to_string(), content);
        self.phase_summaries.insert(phase.to_string(), summary_val);
    }

    /// Render a template string by replacing `{{key}}` tokens.
    ///
    /// Why: Keeps the templating zero-dependency; workflows only need simple
    /// variable substitution, not full Jinja semantics.
    /// What: Replaces `{{task}}`, `{{out_dir}}`, and `{{<phase_name>}}` with
    /// their current values. Missing keys are replaced with `(missing: <key>)`
    /// so the final prompt is debuggable rather than silently corrupted.
    /// Test: See `render_template_substitutes_values` below.
    pub fn render_template(&self, template: &str) -> String {
        let mut vars: HashMap<&str, String> = HashMap::new();
        vars.insert("task", self.task.clone());
        vars.insert(
            "out_dir",
            self.out_dir
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        );
        // #140: `{{project_dir}}` resolves to the discovered project root
        // (containing pyproject.toml) when set, otherwise falls back to
        // out_dir. This lets QA / docs templates reference the actual
        // project root regardless of whether the engineer wrote files
        // directly into out_dir or into a subdirectory.
        vars.insert(
            "project_dir",
            self.project_dir
                .as_ref()
                .or(self.out_dir.as_ref())
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        );
        // #123: `{{project_root}}` resolves to the harness's current working
        // directory (the git repository root the engine was invoked from).
        // Why: When the code phase runs under a `claude-code` runner, the
        // claude CLI sometimes writes relative paths against this directory
        // rather than `RunContext::working_dir`. The post-code reconcile
        // step relocates those files into `out_dir`, but exposing the path
        // also lets QA prompt templates point at the git root explicitly
        // when needed (e.g. "if no tests in {{project_dir}}, also check
        // {{project_root}}"). Falls back to empty string on stat failure.
        vars.insert(
            "project_root",
            std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        );
        // #27: Template substitution uses the SUMMARY (not full content) so
        // downstream phases see a concise ~500-word digest rather than 30k
        // chars of raw code. Fall back to phase_outputs for legacy callers.
        let phase_vars: Vec<(String, String)> = self
            .phase_summaries
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .chain(
                self.phase_outputs
                    .iter()
                    .filter(|(k, _)| !self.phase_summaries.contains_key(*k))
                    .map(|(k, v)| (k.clone(), v.clone())),
            )
            .collect();

        let mut out = String::with_capacity(template.len());
        let mut chars = template.char_indices().peekable();
        while let Some((i, ch)) = chars.next() {
            if ch == '{'
                && chars.peek().map(|(_, c)| *c) == Some('{')
                && let Some(end) = template[i..].find("}}")
            {
                let inner = &template[i + 2..i + end];
                let key = inner.trim();
                let value = if let Some(v) = vars.get(key) {
                    v.clone()
                } else if let Some((_, v)) = phase_vars.iter().find(|(k, _)| k == key) {
                    v.clone()
                } else {
                    format!("(missing: {key})")
                };
                out.push_str(&value);
                // Advance iterator past `}}`.
                let skip_to = i + end + 2;
                while let Some(&(j, _)) = chars.peek() {
                    if j < skip_to {
                        chars.next();
                    } else {
                        break;
                    }
                }
                continue;
            }
            out.push(ch);
        }
        out
    }
}

/// Builder for `WorkflowContext` enforcing ordered construction.
pub struct WorkflowContextBuilder {
    task: String,
    out_dir: Option<PathBuf>,
    phase_outputs: HashMap<String, String>,
    phase_summaries: HashMap<String, String>,
}

impl WorkflowContextBuilder {
    pub fn new(task: impl Into<String>) -> Self {
        Self {
            task: task.into(),
            out_dir: None,
            phase_outputs: HashMap::new(),
            phase_summaries: HashMap::new(),
        }
    }

    pub fn with_out_dir(mut self, path: Option<PathBuf>) -> Self {
        self.out_dir = path;
        self
    }

    #[allow(dead_code)]
    pub fn with_phase_output(mut self, phase: &str, output: String) -> Self {
        self.phase_outputs.insert(phase.to_string(), output.clone());
        // Default summary to the output for builder-style construction in tests.
        self.phase_summaries.insert(phase.to_string(), output);
        self
    }

    #[allow(dead_code)]
    pub fn with_phase_summary(mut self, phase: &str, summary: String) -> Self {
        self.phase_summaries.insert(phase.to_string(), summary);
        self
    }

    pub fn build(self) -> WorkflowContext {
        WorkflowContext {
            task: self.task,
            out_dir: self.out_dir,
            phase_outputs: self.phase_outputs,
            phase_summaries: self.phase_summaries,
            goal_block: None,
            project_dir: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_template_substitutes_values() {
        let ctx = WorkflowContext::builder("do x")
            .with_out_dir(Some(PathBuf::from("/tmp/out")))
            .with_phase_output("research", "r-data".into())
            .build();
        let s = ctx.render_template("Task: {{task}}\nOut: {{out_dir}}\nR: {{research}}");
        assert!(s.contains("Task: do x"));
        assert!(s.contains("Out: /tmp/out"));
        assert!(s.contains("R: r-data"));
    }

    #[test]
    fn render_template_marks_missing_keys() {
        let ctx = WorkflowContext::builder("x").build();
        let s = ctx.render_template("{{nonexistent}}");
        assert!(s.contains("missing"));
    }

    #[test]
    fn render_template_no_tokens_is_identity() {
        let ctx = WorkflowContext::builder("x").build();
        let s = ctx.render_template("no vars here");
        assert_eq!(s, "no vars here");
    }

    #[test]
    fn render_uses_summary_not_content_when_both_present() {
        // #27: downstream templates must see the summary, not the full content.
        let mut ctx = WorkflowContext::builder("t").build();
        ctx.record_phase(
            "research",
            "FULL CONTENT 30k chars of code here".into(),
            Some("CONCISE SUMMARY".into()),
        );
        let s = ctx.render_template("Research result: {{research}}");
        assert!(s.contains("CONCISE SUMMARY"));
        assert!(!s.contains("FULL CONTENT"));
    }

    #[test]
    fn record_phase_without_summary_falls_back_to_content() {
        let mut ctx = WorkflowContext::builder("t").build();
        ctx.record_phase("code", "some code body".into(), None);
        let s = ctx.render_template("{{code}}");
        assert_eq!(s, "some code body");
    }

    #[test]
    fn project_dir_falls_back_to_out_dir_when_unset() {
        // #140: When no explicit project_dir is discovered, {{project_dir}}
        // should render as out_dir so existing templates keep working when
        // the engineer writes files directly into out_dir.
        let ctx = WorkflowContext::builder("t")
            .with_out_dir(Some(PathBuf::from("/tmp/out")))
            .build();
        let s = ctx.render_template("{{project_dir}}");
        assert_eq!(s, "/tmp/out");
    }

    #[test]
    fn project_dir_uses_discovered_subdirectory_when_set() {
        // #140: When the engine discovers a project subdirectory (containing
        // pyproject.toml), {{project_dir}} should resolve to that path, not
        // out_dir. This is the fix for the bug where pytest ran against
        // out_dir/ instead of out_dir/task_board/ and collected zero tests.
        let mut ctx = WorkflowContext::builder("t")
            .with_out_dir(Some(PathBuf::from("/tmp/out")))
            .build();
        ctx.project_dir = Some(PathBuf::from("/tmp/out/task_board"));
        let s = ctx.render_template("{{project_dir}}");
        assert_eq!(s, "/tmp/out/task_board");
    }

    #[test]
    fn project_dir_empty_when_neither_set() {
        let ctx = WorkflowContext::builder("t").build();
        let s = ctx.render_template("[{{project_dir}}]");
        assert_eq!(s, "[]");
    }

    #[test]
    fn builder_chains() {
        let ctx = WorkflowContext::builder("t")
            .with_out_dir(Some(PathBuf::from("/d")))
            .with_phase_output("a", "A".into())
            .with_phase_output("b", "B".into())
            .build();
        assert_eq!(ctx.task, "t");
        assert_eq!(ctx.phase_outputs.get("a"), Some(&"A".to_string()));
        assert_eq!(ctx.phase_outputs.get("b"), Some(&"B".to_string()));
    }
}
