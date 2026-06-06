//! Part of the `commands` module (split from the monolithic `commands.rs`
//! for the 500-line file cap — see #357). Holds an `impl TrustyAgentsRepl` block
//! for one slash-command handler group.

use std::fmt::Write as _;

use crate::repl::TrustyAgentsRepl;

impl TrustyAgentsRepl {
    /// Feature B5: Print the last 20 NDJSON entries from today's chat log.
    pub(crate) fn print_recent_logs_into(&self, out: &mut String) {
        let logger = match crate::logging::global() {
            Some(l) => l,
            None => {
                let _ = writeln!(out, "logs: chat logging is not enabled.");
                return;
            }
        };
        let path = logger.today_log_path();
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => {
                let _ = writeln!(out, "logs: no entries yet today ({}).", path.display());
                return;
            }
        };
        let lines: Vec<&str> = raw.lines().collect();
        let start = lines.len().saturating_sub(20);
        let _ = writeln!(
            out,
            "Recent chat log entries ({} shown of {} today):",
            lines.len() - start,
            lines.len()
        );
        for line in &lines[start..] {
            let parsed: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let ts = parsed
                .get("ts")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.format("%H:%M").to_string())
                .unwrap_or_else(|| "--:--".to_string());
            let role = parsed
                .get("role")
                .and_then(|v| v.as_str())
                .or_else(|| parsed.get("tool").and_then(|v| v.as_str()))
                .unwrap_or("?");
            let content = parsed
                .get("content")
                .and_then(|v| v.as_str())
                .or_else(|| parsed.get("output").and_then(|v| v.as_str()))
                .unwrap_or("");
            let preview: String = content
                .chars()
                .take(80)
                .collect::<String>()
                .replace('\n', " ");
            let _ = writeln!(out, "[{ts}] {role}: {preview}");
        }
    }

    /// Tail the last N lines of the perf runs log into `out`.
    pub(crate) fn tail_log_into(&self, n: usize, out: &mut String) {
        let log_path = self
            .project_dir
            .join("docs")
            .join("performance")
            .join("runs.log");
        match std::fs::read_to_string(&log_path) {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let start = lines.len().saturating_sub(n);
                for line in &lines[start..] {
                    let _ = writeln!(out, "{line}");
                }
            }
            Err(e) => {
                let _ = writeln!(out, "log: cannot read {}: {e}", log_path.display());
            }
        }
    }

    /// Print last N entries from the REPL input history file into `out`.
    pub(crate) fn print_history_into(&self, n: usize, out: &mut String) {
        match std::fs::read_to_string(&self.history_path) {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let _ = writeln!(out, "REPL input history (last {n} of {}):", lines.len());
                let start = lines.len().saturating_sub(n);
                for (i, line) in lines[start..].iter().enumerate() {
                    let _ = writeln!(out, "{:4}  {line}", start + i + 1);
                }
            }
            Err(e) => {
                let _ = writeln!(out, "history: {e}");
            }
        }
    }
}
