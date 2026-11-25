//! Which configs are INSTANCES of the Assistant type (#4325).
//!
//! Why: startup provisioning needs a roster, and `role = "assistant"` alone is
//! not it. `ctrl` — the orchestrator/concierge — also declares that role, and it
//! is emphatically not one of the "multiple Assistant instances" milestone 22
//! pillar (b) is about: the GUI's null `activeAgentId` MEANS ctrl (Concierge),
//! so giving it a selectable per-instance home would model the one agent that
//! is not an instance as if it were. Selecting on the role alone would have
//! silently provisioned it.
//!
//! What: [`discover_instances`] returns the instances found in the agent
//! directories, under a mechanical rule with no name special-casing — a config
//! is an instance when it declares `role = "assistant"` AND is either the base
//! `assistant` itself or declares `extends = "assistant"`. That is the same
//! lineage the shipped personas already use (`izzie` and `cto-assistant` both
//! extend the base), and it excludes `ctrl`, which declares the role but
//! descends from nothing.
//!
//! Parsing is PARTIAL and forgiving, matching
//! `super::super::stores::binding::load_stores`: reading through `AgentConfig`
//! would demand `[llm]`/`[system_prompt]` be present and valid, which a
//! directory-package `agent.toml` deliberately omits. A malformed file is
//! skipped, never fatal — startup provisioning must not care.
//!
//! Test: `super::tests::roster_tests` — the whole module.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::instance::{ASSISTANT_ROLE, AssistantInstanceId, is_assistant_role};

/// The base agent every Assistant instance descends from.
///
/// Why: the base is both the TYPE's template and, per its own `agent.toml`, a
/// usable default — so it is an instance as well as the thing instances extend.
/// Test: `super::tests::roster_tests::the_base_assistant_is_itself_an_instance`.
pub const ASSISTANT_BASE: &str = ASSISTANT_ROLE;

/// Every Assistant-type INSTANCE discoverable in `agent_dirs`.
///
/// Why/What: see this module's doc comment. Results are deduplicated and
/// sorted, so the startup log is stable across launches rather than reordering
/// with directory iteration.
/// What: an id per instance. A config whose name is not a usable instance id
/// (it would become a directory name) is SKIPPED rather than rejected — an
/// unrelated agent with an odd name must not stop provisioning the rest.
/// Test: `super::tests::roster_tests::finds_the_shipped_instances`,
/// `super::tests::roster_tests::excludes_ctrl_and_non_assistants`,
/// `super::tests::roster_tests::skips_unparseable_and_unusable_entries`.
pub fn discover_instances(agent_dirs: &[PathBuf]) -> Vec<AssistantInstanceId> {
    let mut found: Vec<AssistantInstanceId> = Vec::new();
    for dir in agent_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let (name, config) = if path.is_dir() {
                let package = path.join("agent.toml");
                if !package.is_file() {
                    continue;
                }
                (file_stem(&path), package)
            } else if path.extension().is_some_and(|e| e == "toml") {
                (file_stem(&path), path.clone())
            } else {
                continue;
            };
            let Some(name) = name else { continue };
            if !is_instance(&config) {
                continue;
            }
            let Ok(id) = AssistantInstanceId::new(&name) else {
                continue;
            };
            if !found.contains(&id) {
                found.push(id);
            }
        }
    }
    found.sort();
    found
}

/// Whether the config at `path` describes an Assistant-type instance.
fn is_instance(path: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(partial) = toml::from_str::<Partial>(&raw) else {
        return false;
    };
    let agent = partial.agent;
    if !is_assistant_role(&agent.role) {
        return false;
    }
    // Either the base itself, or something that descends from it. `ctrl`
    // declares the role but neither is nor extends the base.
    agent.name == ASSISTANT_BASE || agent.extends.as_deref() == Some(ASSISTANT_BASE)
}

/// The directory or file-stem name of a roster entry.
fn file_stem(path: &Path) -> Option<String> {
    let raw = if path.is_dir() {
        path.file_name()
    } else {
        path.file_stem()
    };
    raw.map(|n| n.to_string_lossy().to_string())
}

/// Just the `[agent]` keys the instance rule needs.
#[derive(Default, Deserialize)]
struct Partial {
    #[serde(default)]
    agent: PartialAgent,
}

#[derive(Default, Deserialize)]
struct PartialAgent {
    #[serde(default)]
    name: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    extends: Option<String>,
}
