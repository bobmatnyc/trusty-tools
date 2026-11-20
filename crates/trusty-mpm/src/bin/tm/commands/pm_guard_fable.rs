//! Refuse a PM dispatch that would run a non-complex agent on Fable (#11353).
//!
//! Why: owner ruling Dq7 — the Fable cap must reach the live path. A real
//! delegation is a Claude Code subagent; trusty-mpm never launches it, so
//! `resolve_agent_model_for_role` never sees it. Claude Code picks the
//! subagent's model from, in order, `CLAUDE_CODE_SUBAGENT_MODEL`, the
//! dispatch's `model` argument, the agent's frontmatter `model:`, and for
//! `inherit` or no `model:` the parent session's model. The PreToolUse hook on
//! the `Agent`/`Task` call is the one point that sees all four before the
//! subagent starts. `updatedInput` for `Agent` is not verifiable from this
//! binary (see `pm_guard.rs`, #5769), so the cap refuses rather than rewrites.
//! What: [`fable_cap_refusal`] resolves the model the dispatch would run on and
//! returns deny text when it is a Fable id and
//! [`fable_allowed_for`] refuses the agent's role. [`evaluate_fable_cap`] reads
//! the inputs from the payload, the deployed agent tiers, the environment and
//! the statusline's session-model record.
//! Test: the `#[cfg(test)]` suite below.

use std::path::Path;

use serde_json::Value;
use trusty_mpm::core::agent::is_subagent_dispatch_tool;
use trusty_mpm::core::config::{MpmConfig, fable_allowed_for, is_fable_model};
use trusty_mpm::core::delegation_authority::{deployed_agent_dirs, scan_agents_reporting};
use trusty_mpm::core::dispatch_isolation::dispatch_agent;

/// Claude Code's process-wide subagent model override.
const SUBAGENT_MODEL_ENV: &str = "CLAUDE_CODE_SUBAGENT_MODEL";

/// What the model of one dispatch depends on, highest precedence first.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct DispatchModel<'a> {
    /// `CLAUDE_CODE_SUBAGENT_MODEL`, when set.
    pub env: Option<&'a str>,
    /// The dispatch's own `model` argument.
    pub argument: Option<&'a str>,
    /// The agent's frontmatter `model:`.
    pub frontmatter: Option<&'a str>,
    /// The agent file could not be read, so `frontmatter` is unknown rather
    /// than absent.
    pub frontmatter_unreadable: bool,
    /// The parent session's model, used for `inherit` or no `model:`.
    pub parent: Option<&'a str>,
}

/// Why the model a dispatch would run on cannot be named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unknown {
    /// `inherit` or no `model:`, and no session-model record for the parent.
    Parent,
    /// The agent was not found and a deployed agent file was unreadable.
    Roster,
}

impl<'a> DispatchModel<'a> {
    /// The model request that wins, or why none can be named.
    fn winning(&self) -> Result<&'a str, Unknown> {
        // #11353: `inherit` defers to the next source, from the argument too.
        let named = |v: Option<&'a str>| {
            v.map(str::trim)
                .filter(|m| !m.is_empty() && !m.eq_ignore_ascii_case("inherit"))
        };
        if let Some(m) = named(self.env).or(named(self.argument)) {
            return Ok(m);
        }
        if self.frontmatter_unreadable {
            return Err(Unknown::Roster);
        }
        named(self.frontmatter)
            .or(named(self.parent))
            .ok_or(Unknown::Parent)
    }
}

/// Deny text when this dispatch would, or might, put a non-allowed role on
/// Fable.
///
/// Why: #11353 — only PM, engineer (any `*-engineer`, including
/// `data-engineer`) and research agents may run on Fable, whatever the
/// frontmatter, config or dispatch asks for. An unknown model fails closed: a
/// subagent that inherits an unrecorded parent may inherit Fable.
/// What: `None` when [`fable_allowed_for`] accepts `agent`/`role`. Otherwise
/// expands the winning request through the config's tier aliases and refuses a
/// Fable id. When no model can be named, the parent is taken to run on the
/// config's `[models] default`; a known non-Fable default passes, anything else
/// is refused with a request for an explicit `model:`.
/// Test: `a_qa_dispatch_asking_for_fable_is_refused`,
/// `allowed_roles_may_run_on_fable`, `inherit_under_a_fable_parent_is_refused`,
/// `the_env_override_is_checked_first`, `non_fable_requests_pass`,
/// `an_unknown_model_is_refused_for_a_capped_role`,
/// `a_known_non_fable_default_admits_an_unknown_parent`.
pub(crate) fn fable_cap_refusal(
    config: &MpmConfig,
    agent: &str,
    role: Option<&str>,
    model: DispatchModel<'_>,
) -> Option<String> {
    if fable_allowed_for(agent, role) {
        return None;
    }
    let role_name = role.unwrap_or(agent);
    let requested = match model.winning() {
        Ok(requested) => requested,
        Err(unknown) => {
            let default = config.models.default.as_deref().map(str::trim);
            let default = default.filter(|d| !d.is_empty() && !d.eq_ignore_ascii_case("inherit"));
            if unknown == Unknown::Parent
                && let Some(d) = default
                && !is_fable_model(config.expand_model_alias(d))
            {
                return None;
            }
            let why = match unknown {
                Unknown::Parent => {
                    "it inherits the parent session's model, and the parent model is unrecorded"
                }
                Unknown::Roster => {
                    "its agent file could not be read, so its frontmatter `model:` is unknown"
                }
            };
            return Some(format!(
                "Dispatch refused (#11353): `{agent}` (role `{role_name}`) may run on the Fable \
                 model: {why}. Fable is reserved for PM, engineer and research agents. Re-issue \
                 the dispatch with `model: \"opus\"` (or another non-Fable model) explicitly."
            ));
        }
    };
    let resolved = config.expand_model_alias(requested);
    if !is_fable_model(resolved) {
        return None;
    }
    Some(format!(
        "Dispatch refused (#11353): `{agent}` (role `{role_name}`) would run on the Fable model \
         `{resolved}`. Fable is reserved for PM, engineer and research agents. Re-issue the \
         dispatch with `model: \"opus\"` (or a lower tier){}.",
        if model.env.is_some() {
            format!(" and unset `{SUBAGENT_MODEL_ENV}`, which overrides it")
        } else {
            String::new()
        }
    ))
}

/// [`fable_cap_refusal`] for a live PreToolUse payload.
///
/// Why: the production entry point; it gathers the four model sources.
/// What: `None` for a non-dispatch tool. Otherwise reads `subagent_type` and
/// `model` from `tool_input`, the agent's role and frontmatter `model:` from the
/// first deployed tier that defines it ([`deployed_agent_dirs`]), the env
/// override, and the parent model the statusline recorded for `session_id`,
/// else the managed launch's `TRUSTY_MPM_LAUNCH_MODEL` stamp.
/// Test: `evaluate_reads_role_and_model_from_a_deployed_tier`.
pub(crate) fn evaluate_fable_cap(
    tool_name: &str,
    tool_input: Option<&Value>,
    cwd: &Path,
    session_id: &str,
) -> Option<String> {
    if !is_subagent_dispatch_tool(tool_name) {
        return None;
    }
    let env = std::env::var_os(SUBAGENT_MODEL_ENV).map(|v| v.to_string_lossy().into_owned());
    let parent = crate::commands::managed_root::resolve_managed_paths(None)
        .ok()
        .and_then(|paths| {
            trusty_mpm::core::session_model::read_session_model(&paths.root, session_id)
        })
        // #11353: a managed launch stamps the model it passed as `--model`.
        .or_else(|| {
            std::env::var(trusty_mpm::core::session_profile::LAUNCH_MODEL_ENV)
                .ok()
                .filter(|m| !m.trim().is_empty())
        });
    evaluate_in(
        &MpmConfig::load_default(),
        tool_input,
        &deployed_agent_dirs(cwd),
        env.as_deref(),
        parent.as_deref(),
    )
}

/// [`evaluate_fable_cap`] with every environment lookup injected.
fn evaluate_in(
    config: &MpmConfig,
    tool_input: Option<&Value>,
    tiers: &[std::path::PathBuf],
    env: Option<&str>,
    parent: Option<&str>,
) -> Option<String> {
    let agent = dispatch_agent(tool_input).unwrap_or("general-purpose");
    let argument = tool_input
        .and_then(|input| input.get("model"))
        .and_then(Value::as_str);
    let mut unreadable = false;
    let summary = tiers.iter().find_map(|tier| {
        let scan = scan_agents_reporting(tier);
        unreadable |= !scan.unreadable.is_empty();
        scan.agents.into_iter().find(|a| a.name == agent)
    });
    let role = summary
        .as_ref()
        .map(|s| s.role.as_str())
        .filter(|r| !r.is_empty());
    let frontmatter = summary.as_ref().and_then(|s| s.model.as_deref());
    fable_cap_refusal(
        config,
        agent,
        role,
        DispatchModel {
            env,
            argument,
            frontmatter,
            // #11353: a missing agent beside an unreadable file is unknown, not absent.
            frontmatter_unreadable: summary.is_none() && unreadable,
            parent,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::hermetic_temp_dir;

    fn ask(argument: Option<&str>, frontmatter: Option<&str>) -> DispatchModel<'static> {
        // Leaked so the borrow outlives the helper; test-only.
        DispatchModel {
            argument: argument.map(|s| &*s.to_string().leak()),
            frontmatter: frontmatter.map(|s| &*s.to_string().leak()),
            ..DispatchModel::default()
        }
    }

    /// The live-path regression: a qa dispatch asking for Fable, by argument or
    /// by frontmatter, is refused. Before #11353's wiring nothing on the
    /// dispatch path read the cap, so these calls were admitted.
    #[test]
    fn a_qa_dispatch_asking_for_fable_is_refused() {
        let cfg = MpmConfig::default();
        for model in [
            ask(Some("fable"), None),
            ask(None, Some("claude-fable-5-2")),
        ] {
            let reason = fable_cap_refusal(&cfg, "qa", Some("qa"), model)
                .expect("qa on Fable must be refused");
            assert!(
                reason.contains("#11353") && reason.contains("opus"),
                "{reason}"
            );
        }
        // The argument beats a non-Fable frontmatter.
        assert!(
            fable_cap_refusal(&cfg, "ticketing", None, ask(Some("fable"), Some("haiku"))).is_some()
        );
    }

    /// PM, engineer, research and every `*-engineer` (data-engineer included)
    /// keep Fable.
    #[test]
    fn allowed_roles_may_run_on_fable() {
        let cfg = MpmConfig::default();
        for (agent, role) in [
            ("pm", Some("pm")),
            ("engineer", Some("engineer")),
            ("research", Some("research")),
            ("rust-engineer", Some("engineer")),
            ("data-engineer", Some("data-engineer")),
            ("data-engineer", None),
        ] {
            assert_eq!(
                fable_cap_refusal(&cfg, agent, role, ask(Some("fable"), None)),
                None,
                "{agent}"
            );
        }
    }

    /// `inherit`, or no `model:`, takes the parent's model: a Fable PM session
    /// would hand Fable to a qa subagent.
    #[test]
    fn inherit_under_a_fable_parent_is_refused() {
        let cfg = MpmConfig::default();
        let parent = DispatchModel {
            frontmatter: Some("inherit"),
            parent: Some("claude-fable-5-2"),
            ..DispatchModel::default()
        };
        assert!(fable_cap_refusal(&cfg, "qa", Some("qa"), parent).is_some());
        let absent = DispatchModel {
            parent: Some("claude-fable-5-2"),
            ..DispatchModel::default()
        };
        assert!(fable_cap_refusal(&cfg, "qa", Some("qa"), absent).is_some());
        // An explicit sonnet frontmatter does not inherit.
        let pinned = DispatchModel {
            frontmatter: Some("sonnet"),
            parent: Some("claude-fable-5-2"),
            ..DispatchModel::default()
        };
        assert_eq!(fable_cap_refusal(&cfg, "qa", Some("qa"), pinned), None);
    }

    /// `CLAUDE_CODE_SUBAGENT_MODEL` outranks the argument, both ways.
    #[test]
    fn the_env_override_is_checked_first() {
        let cfg = MpmConfig::default();
        let env_fable = DispatchModel {
            env: Some("fable"),
            argument: Some("opus"),
            ..DispatchModel::default()
        };
        let reason = fable_cap_refusal(&cfg, "qa", None, env_fable).expect("refused");
        assert!(reason.contains(SUBAGENT_MODEL_ENV), "{reason}");
        let env_opus = DispatchModel {
            env: Some("opus"),
            argument: Some("fable"),
            ..DispatchModel::default()
        };
        assert_eq!(fable_cap_refusal(&cfg, "qa", None, env_opus), None);
    }

    /// An explicit non-Fable argument or frontmatter passes with no parent
    /// record; nothing named and no parent is refused.
    #[test]
    fn non_fable_requests_pass() {
        let cfg = MpmConfig::default();
        for model in [ask(Some("opus"), None), ask(None, Some("sonnet"))] {
            assert_eq!(fable_cap_refusal(&cfg, "qa", Some("qa"), model), None);
        }
        assert!(fable_cap_refusal(&cfg, "qa", Some("qa"), ask(None, None)).is_some());
    }

    /// The fail-open regression: with no env, no argument (or `inherit`), an
    /// `inherit`/absent frontmatter and no parent record, a qa dispatch is
    /// refused and told to pass a model. Allowed roles are untouched.
    #[test]
    fn an_unknown_model_is_refused_for_a_capped_role() {
        let cfg = MpmConfig::default();
        for model in [
            ask(None, None),
            ask(None, Some("inherit")),
            ask(Some("inherit"), None),
            ask(Some("inherit"), Some("inherit")),
        ] {
            let reason = fable_cap_refusal(&cfg, "qa", Some("qa"), model)
                .expect("an unknown model must be refused for qa");
            assert!(
                reason.contains("unrecorded") && reason.contains("model: \"opus\""),
                "{reason}"
            );
            assert_eq!(fable_cap_refusal(&cfg, "rust-engineer", None, model), None);
        }
        // `inherit` as an argument defers to a Fable parent, not to a name.
        let inherit_arg = DispatchModel {
            argument: Some("inherit"),
            parent: Some("claude-fable-5-2"),
            ..DispatchModel::default()
        };
        assert!(fable_cap_refusal(&cfg, "qa", Some("qa"), inherit_arg).is_some());
        // An unreadable roster leaves the frontmatter unknown, even with a
        // non-Fable parent; an explicit argument still decides.
        let unreadable = DispatchModel {
            frontmatter_unreadable: true,
            parent: Some("claude-opus-5-5"),
            ..DispatchModel::default()
        };
        let reason = fable_cap_refusal(&cfg, "qa", None, unreadable).expect("refused");
        assert!(reason.contains("could not be read"), "{reason}");
        let named = DispatchModel {
            argument: Some("opus"),
            ..unreadable
        };
        assert_eq!(fable_cap_refusal(&cfg, "qa", None, named), None);
    }

    /// A known non-Fable `[models] default` stands in for an unrecorded
    /// parent; a Fable default does not.
    #[test]
    fn a_known_non_fable_default_admits_an_unknown_parent() {
        let mut cfg = MpmConfig::default();
        cfg.models.default = Some("opus".into());
        assert_eq!(
            fable_cap_refusal(&cfg, "qa", Some("qa"), ask(None, None)),
            None
        );
        cfg.models.default = Some("fable".into());
        assert!(fable_cap_refusal(&cfg, "qa", Some("qa"), ask(None, None)).is_some());
    }

    /// The payload path reads `role:` and `model:` from a deployed agent file,
    /// so a qa-role agent with an engineer-sounding name is still capped.
    #[test]
    fn evaluate_reads_role_and_model_from_a_deployed_tier() {
        let dir = hermetic_temp_dir();
        std::fs::write(
            dir.path().join("code-critic.md"),
            "---\nname: code-critic\nrole: qa\nmodel: fable\n---\nbody\n",
        )
        .expect("write agent");
        let tiers = vec![dir.path().to_path_buf()];
        let cfg = MpmConfig::default();
        let input = serde_json::json!({"subagent_type": "code-critic", "prompt": "go"});
        assert!(evaluate_in(&cfg, Some(&input), &tiers, None, None).is_some());
        let opus = serde_json::json!({"subagent_type": "code-critic", "model": "opus"});
        assert_eq!(evaluate_in(&cfg, Some(&opus), &tiers, None, None), None);
        assert_eq!(
            evaluate_fable_cap("Bash", Some(&input), dir.path(), "s"),
            None
        );
    }
}
