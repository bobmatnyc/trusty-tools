//! Agent / persona slash-command handlers for the REPL.
//!
//! Why: `/agent`, `/switch`, `/agents`, and `/skills` and the friendly
//! natural-language switcher all sit together — extracting them keeps
//! `mod.rs` focused on lifecycle.
//! What: `impl TrustyAgentsRepl` block hosting the persona-switch handlers plus
//! the `detect_agent_switch` free function used by `ReplBridge::handle_input`
//! for short natural-language phrases.
//! Test: Covered by the `try_handle_slash_switch_*` and `detect_agent_switch_*`
//! unit tests in `mod.rs` (which remain there via `use super::*`).

use std::fmt::Write as _;

use super::{TrustyAgentsRepl, discover_agent_names};

impl TrustyAgentsRepl {
    /// Resolve and activate a persona (or list assistants), writing output to `out`.
    pub(crate) fn handle_agent_command_into(&mut self, arg: &str, out: &mut String) {
        if arg.is_empty() {
            self.list_assistant_agents_into(out);
            return;
        }

        let toml_path = self.agents_dir.join(format!("{}.toml", arg));
        if !toml_path.is_file() {
            let user_path = dirs::home_dir().map(|h| {
                h.join(".trusty-agents")
                    .join("agents")
                    .join(format!("{}.toml", arg))
            });
            let resolved = match user_path {
                Some(p) if p.is_file() => p,
                _ => {
                    let _ = writeln!(out, "agent '{}' not found at {}", arg, toml_path.display());
                    return;
                }
            };
            match crate::agents::AgentConfig::load(&resolved) {
                Ok(cfg) => self.activate_persona_into(arg, &cfg, out),
                Err(e) => {
                    let _ = writeln!(out, "error loading agent '{}': {e:#}", arg);
                }
            }
            return;
        }

        match crate::agents::AgentConfig::load(&toml_path) {
            Ok(cfg) => self.activate_persona_into(arg, &cfg, out),
            Err(e) => {
                let _ = writeln!(out, "error loading agent '{}': {e:#}", arg);
            }
        }
    }

    /// Handle `/switch [<persona>]` — flip the active front-end "voice".
    ///
    /// Why: Users want a single discoverable command to switch between the
    /// three blessed personas (ctrl, Izzie, CTO Assistant) without
    /// memorising TOML stems. `/agent` is the more general primitive (any
    /// agent in the dir); `/switch` is the curated subset.
    /// What: Accepts friendly aliases — "ctrl" / "izzie" / "Izzie" /
    /// "cto" / "cto-assistant" / "CTO Assistant" (case-insensitive). Empty
    /// arg lists the choices (the no-arg picker path is handled upstream
    /// in `ReplBridge`). Switching to "ctrl" clears any active persona.
    /// Test: `try_handle_slash_switch_*` unit tests below.
    pub(crate) fn handle_switch_command_into(&mut self, arg: &str, out: &mut String) {
        if arg.is_empty() {
            let _ = writeln!(out, "Usage: /switch <persona>");
            let _ = writeln!(out, "Available personas:");
            let _ = writeln!(
                out,
                "  ctrl            project-aware orchestrator (default)"
            );
            let _ = writeln!(out, "  Izzie           friendly personal assistant");
            let _ = writeln!(out, "  CTO Assistant   strategic Duetto-aware advisor");
            return;
        }
        let stem = match arg.to_lowercase().trim() {
            "ctrl" => "ctrl",
            "izzie" => "izzie",
            "cto" | "cto-assistant" | "cto assistant" => "cto-assistant",
            other => {
                let _ = writeln!(
                    out,
                    "Unknown persona: {}. Valid: ctrl, Izzie, CTO Assistant",
                    other
                );
                return;
            }
        };
        if stem == "ctrl" {
            // Clearing the persona returns us to the default ctrl path. The
            // post-slash hook in `ReplBridge` re-emits LabelChanged + scope.
            self.active_persona = None;
            self.project_name = "ctrl".to_string();
            self.conversation_history.clear();
            self.chat_log.clear();
            let _ = writeln!(out, "[trusty-agents] Switched to: ctrl");
            return;
        }
        // izzie / cto-assistant — defer to the existing persona loader so
        // display_name and prompt_label are honored uniformly with `/agent`.
        self.handle_agent_command_into(stem, out);
    }

    /// Public wrapper used by `handle_input` for natural-language agent
    /// switches; output is discarded since the bridge emits its own
    /// `StatusMessage`. Kept as a thin shim so the legacy call site doesn't
    /// need to allocate a buffer it won't use.
    pub(crate) fn handle_agent_command(&mut self, arg: &str) {
        let mut sink = String::new();
        self.handle_agent_command_into(arg, &mut sink);
    }

    /// Apply persona switch, writing the "Switched to" line into `out`.
    pub(crate) fn activate_persona_into(
        &mut self,
        name: &str,
        cfg: &crate::agents::AgentConfig,
        out: &mut String,
    ) {
        let display = cfg
            .agent
            .display_name
            .clone()
            .unwrap_or_else(|| cfg.agent.name.clone());
        let label = cfg
            .agent
            .prompt_label
            .clone()
            .unwrap_or_else(|| cfg.agent.name.clone());
        self.active_persona = Some(name.to_string());
        self.project_name = label;
        self.conversation_history.clear();
        self.chat_log.clear();
        let _ = writeln!(out, "[trusty-agents] Switched to: {}", display);
    }

    /// List available assistant-role agents into `out`.
    pub(crate) fn list_assistant_agents_into(&self, out: &mut String) {
        let mut found: Vec<(String, String)> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.agents_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                    continue;
                }
                let stem = match path.file_stem().and_then(|s| s.to_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                if let Ok(cfg) = crate::agents::AgentConfig::load(&path)
                    && cfg.agent.role == "assistant"
                {
                    found.push((stem, cfg.agent.description.clone()));
                }
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        if found.is_empty() {
            let _ = writeln!(
                out,
                "No assistant-role agents found in {}",
                self.agents_dir.display()
            );
            let _ = writeln!(
                out,
                "Drop a TOML with `role = \"assistant\"` into that directory."
            );
            return;
        }
        let _ = writeln!(out, "Available assistant agents:");
        for (name, desc) in found {
            let _ = writeln!(out, "  {:<24}  {}", name, desc);
        }
        let _ = writeln!(
            out,
            "\nUsage: /agent <name>  (e.g. /agent personal-assistant)"
        );
    }

    pub(crate) fn print_agents_into(&self, out: &mut String) {
        let names = discover_agent_names(&self.agents_dir);
        if names.is_empty() {
            let _ = writeln!(out, "no agents found in {}", self.agents_dir.display());
        } else {
            let _ = writeln!(out, "Agents ({}):", names.len());
            for name in names {
                let _ = writeln!(out, "  - {name}");
            }
        }
    }

    pub(crate) fn print_skills_into(&self, out: &mut String) {
        let mut skills = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.skills_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("md")
                    && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                {
                    skills.push(stem.to_string());
                }
            }
        }
        skills.sort();
        if skills.is_empty() {
            let _ = writeln!(out, "no skills found in {}", self.skills_dir.display());
        } else {
            let _ = writeln!(out, "Skills ({}):", skills.len());
            for s in skills {
                let _ = writeln!(out, "  - {s}");
            }
        }
    }
}

/// Detect whether a short user phrase is a natural-language agent switch
/// request, and if so return the target agent's TOML stem (or the literal
/// `"ctrl"` to clear the active persona).
pub(crate) fn detect_agent_switch(input: &str, has_active_persona: bool) -> Option<&'static str> {
    let lower = input.to_lowercase();
    let back_to_ctrl = (lower.contains("switch")
        || lower.contains("back")
        || lower.contains("exit")
        || lower.contains("go"))
        && (lower.contains("ctrl")
            || lower.contains("default")
            || lower.contains("normal")
            || lower.contains("agent"));
    if has_active_persona && back_to_ctrl {
        return Some("ctrl");
    }
    if lower.contains("izzie") || lower.contains("personal assistant") {
        return Some("personal-assistant");
    }
    if lower.contains("cto")
        && (lower.contains("switch")
            || lower.contains("use")
            || lower.contains("be ")
            || lower.contains("mode"))
    {
        return Some("cto-assistant");
    }
    None
}
