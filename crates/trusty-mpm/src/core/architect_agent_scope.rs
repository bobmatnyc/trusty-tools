//! The bundled agents an Architect launch does not load (MB159a).
//!
//! Why: the Architect never dispatched 22 of the bundled agents in 30 sessions
//! (MB1598), yet their roster lines ride in every Architect request (~4k chars).
//! They cannot simply go undeployed: bundled agents live in ONE machine-global
//! tier (`FrameworkPaths::agent_deploy_dir`) that every PM session reads, so
//! the roster is cut where the Architect's launch is built, not where files
//! are written.
//! What: [`ARCHITECT_UNUSED_AGENTS`] is the list; [`deny_rules`] turns it into
//! `permissions.deny` entries (`Agent(<name>)` and the older `Task(<name>)`
//! spelling). [`super::architect_lockdown::Lockdown::PRODUCTION`] merges them
//! into the Architect's one `--settings` payload, so every launch seam carries
//! them and a PM launch never does.
//! ON DEMAND: the files stay deployed. A language engineer is dispatched by
//! the PM session of the project that needs it (a PM keeps the full roster);
//! to use one from the Architect itself, remove its name from this list.
//! Test: `an_architect_launch_hides_the_unused_agents_and_keeps_the_needed_ones`,
//! `a_pm_launch_hides_no_agent`,
//! `the_unused_agent_list_names_bundled_agents_and_both_spellings`
//! (in `architect_lockdown_tests.rs`).

/// The never-dispatched agents (MB1598 row 9): language engineers and
/// framework, QA, data, ops and manager specialists the Architect leaves to
/// its project PMs. `ops` is the legacy file a prior install left deployed.
pub const ARCHITECT_UNUSED_AGENTS: [&str; 22] = [
    "dotnet-engineer",
    "golang-engineer",
    "elixir-engineer",
    "svelte-engineer",
    "php-engineer",
    "typescript-engineer",
    "java-engineer",
    "ruby-engineer",
    "dart-engineer",
    "tauri-engineer",
    "react-engineer",
    "javascript-engineer",
    "refactoring-engineer",
    "api-qa",
    "data-designer",
    "phoenix-engineer",
    "gcp-ops",
    "ops",
    "memory-manager",
    "prompt-engineer",
    "mpm-agent-manager",
    "mpm-skills-manager",
];

/// The `permissions.deny` rules that hide `agents` from a session.
///
/// What: `Agent(<name>)` then `Task(<name>)` per name, in list order; the two
/// spellings cover Claude Code releases before and after the Task rename.
/// Test: `the_unused_agent_list_names_bundled_agents_and_both_spellings`.
pub fn deny_rules(agents: &[&str]) -> Vec<String> {
    agents
        .iter()
        .flat_map(|name| [format!("Agent({name})"), format!("Task({name})")])
        .collect()
}
