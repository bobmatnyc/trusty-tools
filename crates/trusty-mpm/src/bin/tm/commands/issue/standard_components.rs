//! The component-label section of `tm issue standard` (#7837).
//!
//! Why: the section used to print exactly what `policy_labels_configured`
//! seeds — the `trusty-mpm` convention label and `ws/<session>`. On this
//! 29-crate workspace that is two lines, and an agent reading them concluded
//! the repository has two components, so an issue owned by `trusty-search`
//! got labelled `trusty-mpm`. #7123 had already fixed the same root cause on
//! the other side of the same rule (`tm issue audit` ACCEPTS a label derived
//! from the workspace's crates) and the fix never reached this read side. The
//! two sides now share one derivation, so what the audit accepts is what the
//! standard prints.
//!
//! What: [`render_component_labels`] lists the seeded labels plus every
//! workspace crate — [`workspace_crate_labels_checked`], the #7123 walk — and
//! cross-references the live `gh label list` so a crate whose label nobody
//! created is flagged rather than silently listed as usable. The live list is
//! a cross-reference only, never a source of entries: a label the workspace
//! has no crate for is not a component, whatever its description claims.
//!
//! # Failing loudly
//!
//! Both reads can fail, and both failures print. A crate derivation that
//! fails says so and says the list below is the seeded set only — the exact
//! output #7837 reported, now labelled as the degraded reading it is. A `gh`
//! failure says so and marks every line's presence unknown rather than
//! flagging all of them MISSING, which would read as "create 29 labels".
//!
//! Test: `component_labels_name_every_workspace_crate`,
//! `a_label_the_repo_lacks_is_flagged_missing`,
//! `a_crate_absent_from_the_workspace_is_not_a_component`,
//! `a_failed_crate_derivation_is_reported`,
//! `a_failed_label_list_is_reported`.

use std::fmt::Write as _;
use std::path::Path;

use trusty_mpm::core::component_labels::workspace_crate_labels_checked;
use trusty_mpm::core::policy_labels::policy_labels_configured;
use trusty_mpm::core::trusty_tools_config::ResolvedTicketing;

use super::standard_live::one_line;
use crate::commands::ticket::labels::{RepoLabel, gh_list_repo_labels};
use crate::commands::ticket::runner::CommandRunner;

/// Render the component-label section.
///
/// Why: one place decides what counts as a component label for the agent that
/// is about to file an issue, and it is the same derivation `tm issue audit`
/// judges that issue with.
/// What: the seeded labels (`session` supplies `ws/<session>` when known),
/// then every workspace crate label the walk from `start_dir` yields, each
/// line marked `MISSING` when the repository's live label set does not carry
/// it. A failed derivation or a failed `gh label list` prints as its own
/// line before the list; neither is allowed to pass as a shorter list.
/// Test: see the module doc.
pub(crate) fn render_component_labels(
    ticketing: &ResolvedTicketing,
    session: Option<&str>,
    start_dir: Option<&Path>,
    runner: &dyn CommandRunner,
) -> String {
    let seeded = policy_labels_configured(ticketing, session);
    // #7837: the crates are the components; the seed table is not.
    let derived = workspace_crate_labels_checked(start_dir);
    let live = gh_list_repo_labels(runner);

    let mut names: Vec<String> = seeded.iter().map(|l| l.name.clone()).collect();
    for name in derived.iter().flatten() {
        if !names.iter().any(|existing| existing == name) {
            names.push(name.clone());
        }
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "\ncomponent labels ({}) — the seeded labels plus every workspace crate,\n\
         \x20                     cross-checked against the repository's live labels:",
        names.len()
    );
    if let Err(reason) = &derived {
        let _ = writeln!(
            out,
            "  crate labels: unavailable ({reason})\n\
             \x20   the list below is the SEEDED set only, not this workspace's components"
        );
    }
    if let Err(e) = &live {
        let _ = writeln!(
            out,
            "  gh label list: unavailable ({})\n\
             \x20   no line below is confirmed to exist on the repository",
            one_line(e)
        );
    }
    for name in &names {
        let _ = writeln!(
            out,
            "  {name}  {}{}",
            detail(name, &seeded, &live),
            status(name, &live)
        );
    }
    out
}

/// The `#<color>  <description>` a line carries, or why it has none.
///
/// Why: a seeded label's color and description are what `tm issue seed-labels`
/// would create, so they are the right thing to show for it; a crate label's
/// only source is the repository itself.
fn detail(name: &str, seeded: &[RepoLabel], live: &anyhow::Result<Vec<RepoLabel>>) -> String {
    seeded
        .iter()
        .find(|l| l.name == name)
        .or_else(|| live.as_ref().ok()?.iter().find(|l| l.name == name))
        .map_or_else(
            || "(not on the repository)".to_string(),
            |l| format!("#{}  {}", l.color, l.description),
        )
}

/// The presence marker a line carries.
///
/// Why: #7837 asks for the labels the repository does NOT carry to be flagged.
/// An unreadable label list is a third answer, never "all of them missing".
fn status(name: &str, live: &anyhow::Result<Vec<RepoLabel>>) -> &'static str {
    match live {
        Err(_) => "  [presence unknown]",
        Ok(labels) if labels.iter().any(|l| l.name == name) => "",
        Ok(_) => "  MISSING — `gh label create` it before an issue names it",
    }
}

#[cfg(test)]
#[path = "standard_components_tests.rs"]
mod tests;
