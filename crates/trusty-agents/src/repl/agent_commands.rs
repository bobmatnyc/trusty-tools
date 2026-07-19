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
    ///
    /// Why (#3303): the old hand-rolled resolver only checked
    /// `agents_dir.join("{name}.toml")` (with a `$HOME` flat-file fallback),
    /// so it never found directory-package agents (`<name>/agent.toml`, #482)
    /// — `/agent assistant` failed even though the bundled `assistant` agent
    /// is a valid, dispatch-ready package. `AgentConfig::by_name` is the same
    /// sync, directory-package + `extends` aware loader the one-shot
    /// `--direct` path already uses (loader.rs `by_name_unresolved_src`),
    /// and its `agents_dir_candidates()` already covers the project dir plus
    /// the `$HOME/.trusty-agents/agents` fallback tier the old code hand-rolled.
    /// What: Delegates name resolution entirely to `AgentConfig::by_name`
    /// (still sync — safe to call from this sync slash-command handler inside
    /// an async runtime).
    /// Test: `handle_agent_command_resolves_directory_package_fixture`,
    /// `handle_agent_command_activates_bundled_assistant_package`.
    pub(crate) fn handle_agent_command_into(&mut self, arg: &str, out: &mut String) {
        if arg.is_empty() {
            self.list_assistant_agents_into(out);
            return;
        }

        match crate::agents::AgentConfig::by_name(arg) {
            Ok(cfg) => self.activate_persona_into(arg, &cfg, out),
            Err(e) => {
                let _ = writeln!(out, "agent '{}' not found: {e:#}", arg);
            }
        }
    }

    /// Handle `/switch [<persona>]` — flip the active front-end "voice".
    ///
    /// Why: Users want a single discoverable command to switch between the
    /// blessed personas (ctrl, Izzie, CTO Assistant, assistant #3303) without
    /// memorising TOML stems. `/agent` is the more general primitive (any
    /// agent in the dir); `/switch` is the curated subset.
    /// What: Accepts friendly aliases — "ctrl" / "izzie" / "Izzie" /
    /// "cto" / "cto-assistant" / "CTO Assistant" / "assistant"
    /// (case-insensitive). Empty arg lists the choices (the no-arg picker
    /// path is handled upstream in `ReplBridge`). Switching to "ctrl" clears
    /// any active persona.
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
            let _ = writeln!(out, "  assistant       base productivity assistant");
            return;
        }
        let stem = match arg.to_lowercase().trim() {
            "ctrl" => "ctrl",
            "izzie" => "izzie",
            "assistant" => "assistant",
            "cto" | "cto-assistant" | "cto assistant" => "cto-assistant",
            other => {
                let _ = writeln!(
                    out,
                    "Unknown persona: {}. Valid: ctrl, Izzie, CTO Assistant, assistant",
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
        // izzie / cto-assistant / assistant — defer to the existing persona
        // loader so display_name and prompt_label are honored uniformly
        // with `/agent`.
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
    ///
    /// Why (#3303): the old scan was non-recursive `*.toml`-only, so a
    /// directory-package agent (`<name>/agent.toml`, #482) — e.g. the
    /// bundled `assistant` — never appeared, even though `/agent assistant`
    /// can dispatch it once resolution goes through `AgentConfig::by_name`.
    /// What: For each entry in `agents_dir`, resolves a candidate stem from
    /// either a flat `<name>.toml` file or a `<name>/agent.toml` directory
    /// package, then loads it via `AgentConfig::by_name` (so package +
    /// `extends` resolution matches what `/agent <name>` will actually
    /// activate) and keeps it when `role == "assistant"`. A `BTreeMap` both
    /// sorts by name and dedupes the rare case where a name exists as both
    /// a flat file and a package (e.g. bundled `cto-assistant`).
    /// Test: `list_assistant_agents_into_surfaces_directory_package`.
    pub(crate) fn list_assistant_agents_into(&self, out: &mut String) {
        let mut found: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        if let Ok(entries) = std::fs::read_dir(&self.agents_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let stem = if path.is_dir() {
                    if !path.join("agent.toml").is_file() {
                        continue;
                    }
                    path.file_name().and_then(|s| s.to_str())
                } else if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                    path.file_stem().and_then(|s| s.to_str())
                } else {
                    None
                };
                let Some(stem) = stem else { continue };
                if found.contains_key(stem) {
                    continue;
                }
                if let Ok(cfg) = crate::agents::AgentConfig::by_name(stem)
                    && cfg.agent.role == "assistant"
                {
                    found.insert(stem.to_string(), cfg.agent.description.clone());
                }
            }
        }
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
