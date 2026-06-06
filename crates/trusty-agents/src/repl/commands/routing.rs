//! Part of the `commands` module (split from the monolithic `commands.rs`
//! for the 500-line file cap — see #357). Holds an `impl TrustyAgentsRepl` block
//! for one slash-command handler group.

use std::fmt::Write as _;

use crate::repl::TrustyAgentsRepl;
use crate::repl::ollama::{ollama_host, probe_ollama};

impl TrustyAgentsRepl {
    /// Handle `/provider [<name>|reset]` slash command (#284).
    pub(crate) fn handle_provider_command_into(&mut self, arg: &str, out: &mut String) {
        const VALID: &[&str] = &["openrouter", "claude-code", "bedrock", "local"];
        // Bookmark: future support for "anthropic-api" and "openai-api" goes here.
        // Note: "local" is handled by `handle_provider_local_into` (async, probes ollama).

        if arg.is_empty() {
            match self.provider_override.as_deref() {
                Some(p) => {
                    let _ = writeln!(out, "Provider: {} (session override)", p);
                }
                None => {
                    let _ = writeln!(out, "Provider: default (auto from env)");
                }
            }
            let _ = writeln!(out, "Valid: {} (or 'reset')", VALID.join(", "));
            return;
        }
        if arg == "reset" {
            self.provider_override = None;
            let _ = writeln!(out, "Provider reset to default (auto from env)");
            return;
        }
        if VALID.contains(&arg) {
            self.provider_override = Some(arg.to_string());
            let _ = writeln!(out, "Provider set to: {}", arg);
        } else {
            let _ = writeln!(
                out,
                "Unknown provider: {}. Valid: {}",
                arg,
                VALID.join(", ")
            );
        }
    }

    /// Handle `/provider local` — probe ollama and switch to local routing.
    pub(crate) async fn handle_provider_local_into(&mut self, out: &mut String) {
        let host = ollama_host();
        match probe_ollama(&host).await {
            Err(e) => {
                let _ = writeln!(
                    out,
                    "ollama not running at {} (set OLLAMA_HOST to override)",
                    host
                );
                let _ = writeln!(out, "details: {e:#}");
            }
            Ok(models) if models.is_empty() => {
                let _ = writeln!(
                    out,
                    "ollama is running at {} but has no models pulled. Run e.g. `ollama pull llama3.2` and retry.",
                    host
                );
            }
            Ok(models) => {
                self.provider_override = Some("local".to_string());
                // Cache for the next `/model` picker so it shows actual
                // locally-pulled models.
                self.ollama_models = models.clone();
                let _ = writeln!(out, "ollama running at {host}. Available models:");
                for m in &models {
                    let _ = writeln!(out, "  {m}");
                }
                let _ = writeln!(out, "Use /model <name> to select.");
            }
        }
    }

    /// Handle `/local [on|off|test]` slash command (#319).
    pub(crate) async fn handle_local_command_into(&mut self, arg: &str, out: &mut String) {
        let arg = arg.trim();
        match arg {
            "on" => {
                let mut cfg = crate::mcp::GlobalConfig::load_or_create()
                    .await
                    .unwrap_or_default();
                cfg.local_inference.enabled = true;
                if let Err(e) = cfg.save().await {
                    let _ = writeln!(out, "failed to persist config: {e:#}");
                    return;
                }
                let _ = writeln!(out, "Local inference: ENABLED");
                let _ = writeln!(out, "Model: {}", cfg.local_inference.model);
                let _ = writeln!(out, "Probing {}...", cfg.local_inference.ollama_host);
                let ok = crate::local_inference::probe_ollama_now(&cfg.local_inference.ollama_host)
                    .await;
                if ok {
                    let _ = writeln!(out, "Ollama: reachable");
                } else {
                    let _ = writeln!(out, "Ollama: NOT reachable — start with `ollama serve`");
                }
                return;
            }
            "off" => {
                let mut cfg = crate::mcp::GlobalConfig::load_or_create()
                    .await
                    .unwrap_or_default();
                cfg.local_inference.enabled = false;
                if let Err(e) = cfg.save().await {
                    let _ = writeln!(out, "failed to persist config: {e:#}");
                    return;
                }
                let _ = writeln!(out, "Local inference: DISABLED");
                return;
            }
            "test" => {
                let cfg = crate::mcp::GlobalConfig::load().await;
                let host = &cfg.local_inference.ollama_host;
                let _ = writeln!(out, "Probing {}...", host);
                let ok = crate::local_inference::probe_ollama_now(host).await;
                if ok {
                    let _ = writeln!(out, "Ollama: reachable at {}", host);
                    match probe_ollama(host).await {
                        Ok(models) if !models.is_empty() => {
                            let _ = writeln!(out, "Available models:");
                            for m in models.iter().take(20) {
                                let _ = writeln!(out, "  {}", m);
                            }
                        }
                        Ok(_) => {
                            let _ = writeln!(
                                out,
                                "(no models pulled — run e.g. `ollama pull qwen3:30b`)"
                            );
                        }
                        Err(e) => {
                            let _ = writeln!(out, "Failed to list models: {e:#}");
                        }
                    }
                } else {
                    let _ = writeln!(
                        out,
                        "Ollama: NOT reachable at {} — start with `ollama serve`",
                        host
                    );
                }
                return;
            }
            "" => {} // fall through to status display
            other => {
                let _ = writeln!(
                    out,
                    "unknown /local subcommand: {other}\nusage: /local [on|off|test]"
                );
                return;
            }
        }

        // Status display.
        let cfg = crate::mcp::GlobalConfig::load().await;
        let li = &cfg.local_inference;
        let _ = writeln!(out, "Local Inference (Ollama)");
        let _ = writeln!(
            out,
            "  Status:   {}",
            if li.enabled { "enabled" } else { "disabled" }
        );
        let _ = writeln!(out, "  Model:    {}", li.model);
        let _ = writeln!(out, "  Host:     {}", li.ollama_host);
        let _ = writeln!(
            out,
            "  Fallback: {}",
            if li.fallback_on_error { "on" } else { "off" }
        );
        let _ = writeln!(out, "  Max tokens: {}", li.max_tokens);

        // Probe ollama for live status.
        let reachable = crate::local_inference::probe_ollama_now(&li.ollama_host).await;
        let _ = writeln!(
            out,
            "  Ollama:   {}",
            if reachable {
                "reachable"
            } else {
                "NOT reachable (run `ollama serve`)"
            }
        );

        if reachable
            && let Ok(models) = probe_ollama(&li.ollama_host).await
            && !models.is_empty()
        {
            let _ = writeln!(out);
            let _ = writeln!(out, "Available models:");
            for m in models.iter().take(20) {
                let _ = writeln!(out, "  {}", m);
            }
        }
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "Toggle with `/local on` / `/local off`, or edit ~/.trusty-agents/config.toml [local_inference]."
        );
    }

    /// Handle `/model [<id>|reset]` slash command (#284).
    pub(crate) fn handle_model_command_into(&mut self, arg: &str, out: &mut String) {
        if arg.is_empty() {
            match self.model_override.as_deref() {
                Some(m) => {
                    let _ = writeln!(out, "Model: {} (session override)", m);
                }
                None => {
                    let m = self.resolve_active_model();
                    let _ = writeln!(out, "Model: {} (from agent TOML)", m);
                }
            }
            let _ = writeln!(out, "Usage: /model <id> | /model reset");
            return;
        }
        if arg == "reset" {
            self.model_override = None;
            // Status bar shows the TOML-resolved model when no override active.
            self.status_bar.model = self.resolve_active_model();
            let _ = writeln!(out, "Model reset to default");
            return;
        }
        self.model_override = Some(arg.to_string());
        self.status_bar.model = arg.to_string();
        let _ = writeln!(out, "Model set to: {}", arg);
    }
}
