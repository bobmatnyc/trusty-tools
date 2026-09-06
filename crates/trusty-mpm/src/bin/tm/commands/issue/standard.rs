//! `tm issue standard` — print the ticketing standard in effect (#6918).
//!
//! Why: the ticketing agent and the `tm-ticketing` skill are told what labels
//! to apply, who to assign, and how to link a PR. Before this verb, every one
//! of those facts was prose in an asset file, so an operator who configured
//! `agents.ticketing` had no way to make the agent read the change — the agent
//! would keep applying what its prompt said. This verb is the read side of the
//! config block: one command, no `gh`, no mutation, whose output IS the
//! standard.
//! What: [`render_standard`] builds the report as a string (pure, so it is
//! testable), [`print_standard`] writes it to stdout. It reports the resolved
//! [`ResolvedTicketing`], the component labels
//! `policy_labels_configured` would seed, and the lifecycle labels the loaded
//! `issue-state.yaml` declares — the two families side by side, which is the
//! distinction the block cannot blur.
//! Test: `standard_reports_the_builtin_defaults`,
//! `standard_reports_configured_extra_labels`,
//! `standard_states_the_two_fixed_rules`,
//! `standard_lists_the_model_lifecycle_labels`.

use std::fmt::Write as _;

use trusty_mpm::core::policy_labels::{CONVENTION_LABEL, policy_labels_configured};
use trusty_mpm::core::trusty_tools_config::ResolvedTicketing;

use super::config::StateModel;

/// Print the effective ticketing standard to stdout.
///
/// Why: the agent-facing entry point; the render is separate so a test can
/// assert the text without capturing stdout.
/// Test: side-effect only — see [`render_standard`]'s tests.
pub(crate) fn print_standard(ticketing: &ResolvedTicketing, model: &StateModel) {
    print!("{}", render_standard(ticketing, model));
}

/// Render the effective ticketing standard.
///
/// Why: everything an agent needs before it files its first issue, in one
/// block, derived from the same values the code acts on — so the report cannot
/// drift from the behaviour the way a prose asset can.
/// What: the config-file location, the two rules configuration cannot relax,
/// the resolved scalar settings, the component labels
/// [`policy_labels_configured`] yields, and the lifecycle labels the loaded
/// model declares. `session` supplies the `ws/<session>` label when known; the
/// caller passes `None` outside tmux and the line is simply absent.
/// Test: `standard_reports_the_builtin_defaults`,
/// `standard_reports_configured_extra_labels`,
/// `standard_states_the_two_fixed_rules`,
/// `standard_lists_the_model_lifecycle_labels`.
pub(crate) fn render_standard(ticketing: &ResolvedTicketing, model: &StateModel) -> String {
    let session = crate::commands::tmux_attach::current_tmux_session_name();
    let mut out = String::new();

    out.push_str(
        "ticketing standard (agents.ticketing in ~/.trusty-tools/trusty-mpm/config.yaml)\n",
    );
    out.push_str("\nfixed — configuration cannot change these:\n");
    let _ = writeln!(
        out,
        "  pr_link_keyword: {} #N   (a merge must not auto-close before live verification;\n\
         \x20                        a one-off Closes is `tm pr open --closes`, never config)",
        ticketing.pr_link_keyword
    );
    let _ = writeln!(
        out,
        "  {CONVENTION_LABEL}: component label only, never a lifecycle label"
    );

    out.push_str("\nsettings:\n");
    let _ = writeln!(out, "  default_assignee:    {}", ticketing.default_assignee);
    let _ = writeln!(
        out,
        "  ensure_labels:       {} (launch-time; `tm issue seed-labels` always seeds)",
        ticketing.ensure_labels
    );
    let _ = writeln!(out, "  claim_comment:       {}", ticketing.claim_comment);
    let _ = writeln!(
        out,
        "  close_requires_note: {}",
        ticketing.close_requires_note
    );
    let _ = writeln!(
        out,
        "  lifecycle_model:     {}",
        ticketing
            .lifecycle_model
            .as_ref()
            .map_or_else(|| "(discovered)".to_string(), |p| p.display().to_string())
    );

    let labels = policy_labels_configured(ticketing, session.as_deref());
    let _ = writeln!(out, "\ncomponent labels ({}):", labels.len());
    for label in &labels {
        let _ = writeln!(
            out,
            "  {}  #{}  {}",
            label.name, label.color, label.description
        );
    }

    let lifecycle: Vec<&super::config::StateLabel> = model
        .states
        .iter()
        .filter_map(|s| s.label.as_ref())
        .collect();
    let _ = writeln!(
        out,
        "\nlifecycle labels ({}) — from issue-state.yaml, never from this block:",
        lifecycle.len()
    );
    for label in lifecycle {
        let _ = writeln!(
            out,
            "  {}  #{}  {}",
            label.name, label.color, label.description
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::issue::config::DEFAULT_MODEL_YAML;

    fn model() -> StateModel {
        serde_yaml::from_str(DEFAULT_MODEL_YAML).expect("embedded default parses")
    }

    #[test]
    fn standard_reports_the_builtin_defaults() {
        let text = render_standard(&ResolvedTicketing::default(), &model());
        assert!(text.contains("default_assignee:    @me"), "{text}");
        assert!(text.contains("ensure_labels:       true"), "{text}");
        assert!(text.contains("lifecycle_model:     (discovered)"), "{text}");
        assert!(text.contains("trusty-mpm"), "{text}");
    }

    #[test]
    fn standard_states_the_two_fixed_rules() {
        // #6918: both rulings have to be visible to the agent that reads this,
        // or the config block looks like it could relax them.
        let text = render_standard(&ResolvedTicketing::default(), &model());
        assert!(text.contains("pr_link_keyword: Refs #N"), "{text}");
        assert!(
            text.contains("component label only, never a lifecycle label"),
            "{text}"
        );
    }

    #[test]
    fn standard_reports_configured_extra_labels() {
        let cfg = ResolvedTicketing::default()
            .with_extra_labels(vec![trusty_mpm::core::policy_labels::PolicyLabel::new(
                "area/cli",
                "0E8A16",
                "CLI surface",
            )])
            .with_default_assignee("bobmatnyc");
        let text = render_standard(&cfg, &model());
        assert!(text.contains("area/cli  #0E8A16  CLI surface"), "{text}");
        assert!(text.contains("default_assignee:    bobmatnyc"), "{text}");
    }

    #[test]
    fn standard_lists_the_model_lifecycle_labels() {
        let m = model();
        let text = render_standard(&ResolvedTicketing::default(), &m);
        let first = m
            .states
            .iter()
            .find_map(|s| s.label.as_ref())
            .expect("the factory model labels its states");
        assert!(text.contains(&first.name), "{text}");
        assert!(
            text.contains("from issue-state.yaml, never from this block"),
            "{text}"
        );
    }
}
