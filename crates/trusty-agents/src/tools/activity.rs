//! Claude stream tool telemetry with explicit task attribution and no tool payloads.
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
#[derive(Clone)]
pub(crate) struct ActivityContext {
    pub session: String,
    rows: Arc<Mutex<Vec<Value>>>,
}
impl ActivityContext {
    pub fn new(session: &str) -> Self {
        Self {
            session: session.into(),
            rows: Default::default(),
        }
    }
    pub fn history(&self) -> Vec<Value> {
        self.rows.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn record(&self, id: &str, tool: &str, status: &str) {
        if self.session.is_empty() {
            return;
        }
        let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let row = json!({"kind":"trusty.tool-activity","version":1,"call_id":id,"tool":tool,"status":status});
        if let Some(old) = rows.iter_mut().find(|v| v["call_id"] == id) {
            *old = row;
        } else if rows.len() < 128 {
            rows.push(row);
        } else {
            return; // Suppress both lifecycle events once the bounded transcript is full.
        }
        drop(rows);
        crate::events::publish(crate::events::Event::ToolActivity {
            session_id: self.session.clone(),
            call_id: id.into(),
            tool: tool.into(),
            status: status.into(),
        });
    }
    pub fn observe(&self, event: &Value) {
        let kind = event["type"].as_str().unwrap_or("");
        if !["assistant", "user"].contains(&kind) {
            return;
        }
        let Some(blocks) = event["message"]["content"].as_array() else {
            return;
        };
        for block in blocks {
            match block["type"].as_str() {
                Some("tool_use") if kind == "assistant" => {
                    if let (Some(id), Some(name)) = (block["id"].as_str(), block["name"].as_str()) {
                        self.record(id, name, "running");
                    }
                }
                Some("tool_result") if kind == "user" => {
                    if let Some(id) = block["tool_use_id"].as_str() {
                        let name = self
                            .history()
                            .iter()
                            .find(|v| v["call_id"] == id)
                            .and_then(|v| v["tool"].as_str())
                            .map(str::to_owned);
                        if let Some(name) = name {
                            self.record(
                                id,
                                &name,
                                if block["is_error"].as_bool() == Some(true) {
                                    "error"
                                } else {
                                    "complete"
                                },
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
tokio::task_local! { pub(crate) static CLI_ACTIVITY: ActivityContext; }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_activity_cli_matches_ids_and_excludes_payloads() {
        let ctx = ActivityContext::new("fixture");
        ctx.observe(&json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"one","name":"Read","input":{"secret":"sensitive"}}]}}));
        ctx.observe(&json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"other","content":"private"}]}}));
        assert_eq!(ctx.history()[0]["status"], "running");
        ctx.observe(&json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"one","is_error":true,"content":"private"}]}}));
        let rows = ctx.history();
        assert_eq!(rows[0]["status"], "error");
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].to_string().contains("private"));
        assert!(!rows[0].to_string().contains("sensitive"));
    }
}

#[cfg(test)]
mod cap_tests {
    use super::*;
    #[tokio::test]
    async fn tool_activity_cli_cap_never_emits_unfinishable_calls() {
        let ctx = ActivityContext::new("cap-fixture-unique");
        let mut rx = crate::events::subscribe();
        for n in 0..130 {
            let id = n.to_string();
            ctx.observe(&json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":id,"name":"Read"}]}}));
            ctx.observe(&json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":id}]}}));
        }
        assert_eq!(ctx.history().len(), 128);
        assert!(ctx.history().iter().all(|v| v["status"] == "complete"));
        let mut count = 0;
        while let Ok(event) = rx.try_recv() {
            if let crate::events::Event::ToolActivity { session_id, .. } = event {
                if session_id == "cap-fixture-unique" {
                    count += 1;
                }
            }
        }
        assert_eq!(count, 256);
    }
}
