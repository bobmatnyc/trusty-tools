//! The `agents:` config group and its first occupant, `agents.ticketing`
//! (#6918).
//!
//! Why: #6914 gave the harness one label table in [`crate::core::policy_labels`],
//! but that table is compiled in — a project that wants an extra component
//! label, a different colour, or a non-default `issue-state.yaml` had nowhere
//! to say so, and the ticketing agent had no way to READ the standard it is
//! supposed to apply. This section is that surface. Split into its own sibling
//! module for the same reason `untracked_sync` and `log_drain` are: the parent
//! `trusty_tools_config.rs` is already near the 500-SLOC production cap.
//!
//! What: [`AgentsConfig`] is the `agents:` group, [`TicketingConfig`] its
//! `ticketing:` block, and [`resolve_ticketing`] the fallible translation into
//! [`ResolvedTicketing`] — the value every consumer reads. An absent block
//! resolves to [`ResolvedTicketing::default`], which is byte-for-byte the
//! behaviour that shipped in #6914.
//!
//! Test: the `tests` submodule.
//!
//! # Two fields exist only to be refused
//!
//! [`TicketingConfig::pr_link_keyword`] and [`ConfiguredLabel::role`] have
//! exactly one accepted value each. They are in the schema anyway, because the
//! surrounding [`TrustyToolsConfig`] parse is LENIENT (see `core::config_keys`):
//! a key the schema does not define is dropped with a `warn` line and no
//! further consequence. An operator who writes `pr_link_keyword: Closes` would
//! get a warning about an unrecognised key — not a refusal — and would
//! reasonably read that as "the knob exists, I spelled it wrong". Declaring
//! both fields turns that into a load-time error that names the field and the
//! rule. Two rulings are what they encode, and neither is configurable:
//!
//! - **`Refs #N`, never `Closes #N`.** A merge must not auto-close an issue
//!   before live verification (root `CLAUDE.md`, "Key Conventions"). A one-off
//!   `Closes` stays the deliberate `tm pr open --closes` flag.
//! - **`trusty-mpm` is a component label.** It names the owning crate, never a
//!   lifecycle position; the lifecycle lives in `issue-state.yaml` and is
//!   REFERENCED from here ([`TicketingConfig::lifecycle_model`]), never
//!   embedded.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::core::policy_labels::PolicyLabel;

use super::TrustyToolsConfig;

/// Default PR assignee when the block names none.
///
/// Why: `tm pr open` has always shipped `--assignee @me`; naming it here keeps
/// the resolved default and the historical hardcoded value provably identical.
/// Test: `absent_block_resolves_to_builtin_defaults`.
pub const DEFAULT_ASSIGNEE: &str = "@me";

/// The only accepted [`TicketingConfig::pr_link_keyword`] value.
///
/// Why: see the module doc — configuration cannot relax the `Refs #N` rule.
/// Test: `closes_pr_link_keyword_is_rejected`, `refs_pr_link_keyword_is_accepted`.
pub const REQUIRED_PR_LINK_KEYWORD: &str = "Refs";

/// The commented starter `agents.ticketing` block `tm issue seed-config`
/// writes into `config.yaml` (#7067).
///
/// Why: the verb seeds `issue-state.yaml` — the lifecycle half of the standard.
/// The other half lives in a different file, and #7067 added three keys to it,
/// so a seeded lifecycle with no block leaves the milestone and project rules
/// undiscoverable. `seed_ticketing_block` lands this text in that file in three
/// of its four outcomes; only the refusal case — an `agents:` key already
/// present without `ticketing:`, where a second top-level `agents:` would make
/// the file unparseable — prints it for the operator to paste by hand.
/// What: every key set to its built-in default, so pasting the block changes
/// nothing until an entry is edited. `default_project` is commented out —
/// there is no default title to state.
/// Test: `the_seed_template_parses_to_the_builtin_defaults`.
pub const TICKETING_BLOCK_TEMPLATE: &str = "\
# The ticketing standard. Read the resolved values back with `tm issue standard`.
agents:
  ticketing:
    # #7067: every new issue carries exactly one milestone. An issue filed with
    # none needs a `no-milestone: <reason>` comment on it.
    milestone_required: true
    # #7067: every new issue joins at least one GitHub Project.
    project_required: true
    # #7067: the Project TITLE new issues join by default. Leave it out to pick
    # per issue from the live list `tm issue standard` prints.
    # default_project: trusty-mpm
    # Whether session launch ensures the component labels exist.
    ensure_labels: true
    # Whether claiming an issue posts a comment naming the claiming session.
    claim_comment: true
    # Whether closing requires live-verification evidence.
    close_requires_note: true
";

/// The `agents:` group of `~/.trusty-tools/trusty-mpm/config.yaml` (#6918).
///
/// Why: settings that belong to ONE bundled agent are grouped under that
/// agent's name rather than scattered across the file's top level, so a reader
/// can tell at a glance which component a key governs.
/// What: currently one occupant, `ticketing`. Absent → every agent runs on its
/// built-in defaults.
/// Test: `agents_config_yaml_round_trip`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
// #6918: `#[non_exhaustive]` for the same reason `TrustyToolsConfig` carries it
// — a new public field on a constructible all-public struct is a semver major.
#[non_exhaustive]
pub struct AgentsConfig {
    /// The `ticketing:` block. `None` → [`ResolvedTicketing::default`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticketing: Option<TicketingConfig>,
}

/// The `agents.ticketing:` block — the ticketing standard, declared.
///
/// Why: the labels the harness applies, the assignee it names, and the
/// lifecycle model it reads were all compiled in. This is where a project says
/// otherwise, in the one config file the owner ruling picked
/// (`~/.trusty-tools/trusty-mpm/config.yaml`).
/// What: every field is optional so a partial block falls back per-field.
/// [`resolve_ticketing`] is what turns it into usable values, and is where the
/// two non-negotiable rules are enforced.
/// Test: `ticketing_config_yaml_round_trip`, `extra_labels_extend_the_policy_set`,
/// `filing_target_keys_round_trip`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TicketingConfig {
    /// Whether SESSION LAUNCH ensures the policy labels exist.
    ///
    /// `None` → `true` (the #6914 behaviour). `Some(false)` turns off the
    /// launch-time ensure only — an operator who types `tm issue seed-labels`
    /// meant it, and that verb always seeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ensure_labels: Option<bool>,

    /// Component labels beyond the built-in policy table.
    ///
    /// An entry whose `name` matches a built-in policy label RESTYLES it
    /// (colour and description); any other entry is appended. Lifecycle
    /// (`status:*`) labels do not belong here — they come from
    /// [`Self::lifecycle_model`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_labels: Vec<ConfiguredLabel>,

    /// Assignee for PRs `tm pr open` creates. `None` → [`DEFAULT_ASSIGNEE`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_assignee: Option<String>,

    /// Whether claiming an issue posts a comment naming the claiming session.
    ///
    /// `None` → `true`. Read by the ticketing agent through
    /// `tm issue standard`; trusty-mpm itself posts no claim comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_comment: Option<bool>,

    /// Whether closing an issue requires live-verification evidence.
    ///
    /// `None` → `true`. Mirrors the `requires_note` flag `issue-state.yaml`
    /// sets on the closing edge, which is what `tm issue transition` actually
    /// enforces; this field is how the agent READS that expectation before it
    /// has a model in hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_requires_note: Option<bool>,

    /// The PR-body issue-link keyword. `None` → [`REQUIRED_PR_LINK_KEYWORD`],
    /// which is also the only accepted value — see the module doc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_link_keyword: Option<String>,

    /// Path to the `issue-state.yaml` that defines the lifecycle.
    ///
    /// `None` → the existing discovery chain is untouched. When set, this path
    /// slots in as a HOST default: `--config` flag > `./issue-state.yaml` >
    /// this > `~/.trusty-tools/trusty-mpm/issue-state.yaml` > embedded. The
    /// model is referenced, never inlined here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_model: Option<PathBuf>,

    // #7067: the milestone/project slice of the standard. Titles, never ids —
    // an agent reads a title back from `tm issue standard` and hands the same
    // string to `gh issue create --add-project`.
    /// GitHub Project (v2) TITLE every new issue joins by default.
    ///
    /// `None` → no default; the agent picks a project by crate/topic fit from
    /// the live list `tm issue standard` prints. Present but blank is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_project: Option<String>,

    /// Whether every new issue must carry exactly one milestone.
    ///
    /// `None` → `true`. Setting `false` is how a project that does not use
    /// milestones says so; it does not make an unset milestone invisible —
    /// `tm issue standard` still prints the flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone_required: Option<bool>,

    /// Whether every new issue must join at least one GitHub Project.
    ///
    /// `None` → `true`. Read by the ticketing agent through
    /// `tm issue standard`; trusty-mpm files no issues itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_required: Option<bool>,
}

/// What a [`ConfiguredLabel`] is FOR.
///
/// Why: see the module doc — `lifecycle` is declarable so that declaring it is
/// an error with a reason, rather than an unrecognised key with a shrug.
/// What: `Component` (the default, and the only accepted value) and
/// `Lifecycle`, which [`resolve_ticketing`] always refuses.
/// Test: `lifecycle_role_on_the_convention_label_is_rejected`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelRole {
    /// Names the owning crate/component. The only accepted role.
    #[default]
    Component,
    /// A lifecycle position. Always refused: the lifecycle is
    /// `issue-state.yaml`'s, referenced via
    /// [`TicketingConfig::lifecycle_model`].
    Lifecycle,
}

/// One label declared in `agents.ticketing.extra_labels`.
///
/// Why: the same name/colour/description shape [`PolicyLabel`] already carries,
/// plus the [`LabelRole`] that makes the component-only rule checkable.
/// What: `name` (required, non-blank), `color` (6-hex, no `#`; empty omits the
/// flag), `description` (empty omits the flag), `role` (defaults to
/// [`LabelRole::Component`]).
/// Test: `extra_labels_extend_the_policy_set`, `blank_label_name_is_rejected`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ConfiguredLabel {
    /// Label name (e.g. `area/cli`).
    pub name: String,
    /// 6-hex colour, no `#`. Empty means "let GitHub pick".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub color: String,
    /// Description shown in the GitHub label UI. Empty means none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// What the label is for. Only [`LabelRole::Component`] is accepted.
    #[serde(default)]
    pub role: LabelRole,
}

/// Why an `agents.ticketing` block was refused.
///
/// Why: a malformed section is an ERROR, never a silent fall-back to defaults —
/// the same stance `log_drain` takes, and for the same reason: a ticketing
/// standard that quietly reverted to the built-in one would be
/// indistinguishable from one that applied. Every variant names the exact
/// dotted field path so the fix is mechanical.
/// Test: `closes_pr_link_keyword_is_rejected`,
/// `lifecycle_role_on_the_convention_label_is_rejected`,
/// `blank_label_name_is_rejected`, `blank_assignee_is_rejected`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TicketingConfigError {
    /// `pr_link_keyword` was set to anything but `Refs`.
    #[error(
        "agents.ticketing.pr_link_keyword: `{found}` is not accepted. Fix PRs link with \
         `{REQUIRED_PR_LINK_KEYWORD} #N` so a merge never auto-closes an issue before live \
         verification, and configuration cannot relax that. A one-off `Closes` is the \
         deliberate `tm pr open --closes` flag."
    )]
    PrLinkKeyword {
        /// The refused value, verbatim.
        found: String,
    },

    /// A label was declared with `role: lifecycle`.
    #[error(
        "agents.ticketing.extra_labels[{index}].role: `{name}` cannot be a lifecycle label. \
         Lifecycle labels are declared in issue-state.yaml and referenced by \
         agents.ticketing.lifecycle_model, never embedded here — and `trusty-mpm` in \
         particular is a component label only."
    )]
    LifecycleRole {
        /// Index of the offending entry in `extra_labels`.
        index: usize,
        /// The label's name.
        name: String,
    },

    /// A label entry had a blank `name`.
    #[error("agents.ticketing.extra_labels[{index}].name: a label name cannot be blank")]
    BlankLabelName {
        /// Index of the offending entry in `extra_labels`.
        index: usize,
    },

    /// `default_assignee` was present but blank.
    #[error("agents.ticketing.default_assignee: cannot be blank (omit the key for `@me`)")]
    BlankAssignee,

    /// `default_project` was present but blank (#7067).
    #[error(
        "agents.ticketing.default_project: cannot be blank (omit the key to let the agent pick \
         a project from the live list `tm issue standard` prints)"
    )]
    BlankDefaultProject,
}

/// The ticketing standard in effect, after applying the block on top of the
/// built-in defaults.
///
/// Why: every consumer — `tm issue seed-labels`, session launch, `tm pr open`,
/// `tm issue standard` — needs concrete values, not the optional on-disk shape.
/// Resolving once here keeps the defaults in a single tested place.
/// What: always-populated fields. [`Self::extra_labels`] is already
/// [`PolicyLabel`]-shaped, so `policy_labels::policy_labels_configured` can
/// merge it without re-deriving anything.
/// Test: `absent_block_resolves_to_builtin_defaults`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ResolvedTicketing {
    /// Whether session launch ensures the policy labels.
    pub ensure_labels: bool,
    /// Assignee `tm pr open` names.
    pub default_assignee: String,
    /// Whether claiming an issue posts a claim comment.
    pub claim_comment: bool,
    /// Whether closing requires live-verification evidence.
    pub close_requires_note: bool,
    /// The PR-body issue-link keyword. Always [`REQUIRED_PR_LINK_KEYWORD`].
    pub pr_link_keyword: &'static str,
    /// Configured `issue-state.yaml` path, or `None` for plain discovery.
    pub lifecycle_model: Option<PathBuf>,
    /// Component labels the block adds to (or restyles in) the policy table.
    pub extra_labels: Vec<PolicyLabel>,
    /// GitHub Project title new issues join by default, or `None` (#7067).
    pub default_project: Option<String>,
    /// Whether every new issue must carry exactly one milestone (#7067).
    pub milestone_required: bool,
    /// Whether every new issue must join at least one project (#7067).
    pub project_required: bool,
}

impl ResolvedTicketing {
    /// This standard with `extra_labels` replaced.
    ///
    /// Why: the type is `#[non_exhaustive]`, so code outside this crate — the
    /// `tm` binary and its tests — cannot build one with a struct literal or
    /// `..Default::default()`. These three take the place of that.
    /// Test: `configured_labels_append_extra_labels`.
    #[must_use]
    pub fn with_extra_labels(mut self, labels: Vec<PolicyLabel>) -> Self {
        self.extra_labels = labels;
        self
    }

    /// This standard with a different `default_assignee`.
    /// Test: `open_assignee_comes_from_the_ticketing_block`.
    #[must_use]
    pub fn with_default_assignee(mut self, assignee: impl Into<String>) -> Self {
        self.default_assignee = assignee.into();
        self
    }

    /// This standard with `ensure_labels` set.
    /// Test: `launch_labels_skip_entirely_when_ensure_labels_is_false`.
    #[must_use]
    pub fn with_ensure_labels(mut self, ensure: bool) -> Self {
        self.ensure_labels = ensure;
        self
    }

    /// This standard with a `default_project` title (#7067).
    /// Test: `standard_reports_the_filing_targets`.
    #[must_use]
    pub fn with_default_project(mut self, project: impl Into<String>) -> Self {
        self.default_project = Some(project.into());
        self
    }

    /// This standard with the two filing requirements set (#7067).
    /// Test: `standard_reports_the_filing_targets`.
    #[must_use]
    pub fn with_filing_requirements(mut self, milestone: bool, project: bool) -> Self {
        self.milestone_required = milestone;
        self.project_required = project;
        self
    }
}

impl Default for ResolvedTicketing {
    /// The built-in standard — exactly what shipped in #6914.
    fn default() -> Self {
        Self {
            ensure_labels: true,
            default_assignee: DEFAULT_ASSIGNEE.to_string(),
            claim_comment: true,
            close_requires_note: true,
            pr_link_keyword: REQUIRED_PR_LINK_KEYWORD,
            lifecycle_model: None,
            extra_labels: Vec::new(),
            // #7067: both default ON. A filing that carries neither is a
            // standard violation the agent can be held to, and an operator who
            // does not use milestones or projects turns the flag off by hand.
            default_project: None,
            milestone_required: true,
            project_required: true,
        }
    }
}

/// Resolve the effective ticketing standard from a loaded config.
///
/// Why: one place where "absent block" and "partial block" become concrete
/// values, and the one place the two non-negotiable rules are checked. Callers
/// that must not fail (session launch) fall back to
/// [`ResolvedTicketing::default`] and log; callers an operator invoked
/// (`tm issue …`, `tm pr open`) surface the error.
/// What: applies each field's default, then validates — `pr_link_keyword` must
/// be [`REQUIRED_PR_LINK_KEYWORD`] (case-insensitively, so `refs` is fine),
/// every label needs a non-blank name and [`LabelRole::Component`], and a
/// present `default_assignee` must be non-blank.
/// Test: `absent_block_resolves_to_builtin_defaults`,
/// `closes_pr_link_keyword_is_rejected`,
/// `lifecycle_role_on_the_convention_label_is_rejected`,
/// `blank_label_name_is_rejected`, `blank_assignee_is_rejected`,
/// `extra_labels_extend_the_policy_set`, `filing_target_keys_round_trip`,
/// `blank_default_project_is_rejected`.
pub fn resolve_ticketing(
    config: &TrustyToolsConfig,
) -> Result<ResolvedTicketing, TicketingConfigError> {
    let Some(block) = config.agents.as_ref().and_then(|a| a.ticketing.as_ref()) else {
        return Ok(ResolvedTicketing::default());
    };

    if let Some(keyword) = block.pr_link_keyword.as_deref()
        && !keyword
            .trim()
            .eq_ignore_ascii_case(REQUIRED_PR_LINK_KEYWORD)
    {
        return Err(TicketingConfigError::PrLinkKeyword {
            found: keyword.to_string(),
        });
    }

    let default_assignee = match block.default_assignee.as_deref() {
        Some(a) if a.trim().is_empty() => return Err(TicketingConfigError::BlankAssignee),
        Some(a) => a.trim().to_string(),
        None => DEFAULT_ASSIGNEE.to_string(),
    };

    // #7067: same shape as `default_assignee` — a present-but-blank title is a
    // typo, not "no default", and resolving it silently to `None` would leave
    // the agent picking a project the operator thought they had named.
    let default_project = match block.default_project.as_deref() {
        Some(p) if p.trim().is_empty() => return Err(TicketingConfigError::BlankDefaultProject),
        Some(p) => Some(p.trim().to_string()),
        None => None,
    };

    let mut extra_labels = Vec::with_capacity(block.extra_labels.len());
    for (index, label) in block.extra_labels.iter().enumerate() {
        let name = label.name.trim();
        if name.is_empty() {
            return Err(TicketingConfigError::BlankLabelName { index });
        }
        if label.role == LabelRole::Lifecycle {
            return Err(TicketingConfigError::LifecycleRole {
                index,
                name: name.to_string(),
            });
        }
        extra_labels.push(PolicyLabel::new(
            name,
            label.color.trim(),
            label.description.trim(),
        ));
    }

    Ok(ResolvedTicketing {
        ensure_labels: block.ensure_labels.unwrap_or(true),
        default_assignee,
        claim_comment: block.claim_comment.unwrap_or(true),
        close_requires_note: block.close_requires_note.unwrap_or(true),
        pr_link_keyword: REQUIRED_PR_LINK_KEYWORD,
        lifecycle_model: block.lifecycle_model.clone(),
        extra_labels,
        default_project,
        milestone_required: block.milestone_required.unwrap_or(true),
        project_required: block.project_required.unwrap_or(true),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a YAML document as a whole [`TrustyToolsConfig`], the way
    /// `TrustyToolsConfig::load` does — so a test proves the SECTION parses
    /// where it actually lives, not just in isolation.
    fn config_from(yaml: &str) -> TrustyToolsConfig {
        serde_yaml::from_str(yaml).expect("config parses")
    }

    #[test]
    fn absent_block_resolves_to_builtin_defaults() {
        let cfg = config_from("auto_resume: true\n");
        let resolved = resolve_ticketing(&cfg).expect("resolves");
        assert_eq!(resolved, ResolvedTicketing::default());
        assert!(resolved.ensure_labels);
        assert_eq!(resolved.default_assignee, "@me");
        assert_eq!(resolved.pr_link_keyword, "Refs");
        assert!(resolved.extra_labels.is_empty());
        assert!(resolved.lifecycle_model.is_none());
        // #7067: an absent block still REQUIRES a milestone and a project.
        assert!(resolved.default_project.is_none());
        assert!(resolved.milestone_required);
        assert!(resolved.project_required);
    }

    #[test]
    fn filing_target_keys_round_trip() {
        // #7067: the three new keys parse where they live, survive a
        // serialise/re-parse, and resolve to the values written.
        let cfg = config_from(
            "agents:\n\
             \x20 ticketing:\n\
             \x20   default_project: trusty-mpm\n\
             \x20   milestone_required: false\n\
             \x20   project_required: false\n",
        );
        let back = serde_yaml::to_string(&cfg).expect("serialises");
        let again: TrustyToolsConfig = serde_yaml::from_str(&back).expect("re-parses");
        assert_eq!(cfg, again);

        let resolved = resolve_ticketing(&cfg).expect("resolves");
        assert_eq!(resolved.default_project.as_deref(), Some("trusty-mpm"));
        assert!(!resolved.milestone_required);
        assert!(!resolved.project_required);
    }

    #[test]
    fn the_seed_template_parses_to_the_builtin_defaults() {
        // #7067: pasting the seeded block must be a no-op until an entry is
        // edited — otherwise seeding it silently changes the standard.
        let cfg = config_from(TICKETING_BLOCK_TEMPLATE);
        assert_eq!(
            resolve_ticketing(&cfg).expect("the template resolves"),
            ResolvedTicketing::default()
        );
        // Every #7067 key is named, so the template cannot drift from the schema.
        for key in ["milestone_required", "project_required", "default_project"] {
            assert!(TICKETING_BLOCK_TEMPLATE.contains(key), "missing {key}");
        }
    }

    #[test]
    fn blank_default_project_is_rejected() {
        let cfg = config_from("agents:\n  ticketing:\n    default_project: \"  \"\n");
        let err = resolve_ticketing(&cfg).expect_err("blank project title");
        assert_eq!(err, TicketingConfigError::BlankDefaultProject);
        assert!(
            err.to_string().contains("agents.ticketing.default_project"),
            "{err}"
        );
    }

    #[test]
    fn an_unknown_filing_key_is_dropped_not_fatal() {
        // #7067: the surrounding parse stays LENIENT — a misspelt key must not
        // take the whole config down, which is why the three real keys are
        // declared rather than left to the warn-and-drop path.
        let cfg = config_from("agents:\n  ticketing:\n    default_projects: trusty-mpm\n");
        let resolved = resolve_ticketing(&cfg).expect("resolves");
        assert!(resolved.default_project.is_none());
    }

    #[test]
    fn agents_config_yaml_round_trip() {
        let cfg = config_from(
            "agents:\n  ticketing:\n    ensure_labels: false\n    default_assignee: bobmatnyc\n",
        );
        let back = serde_yaml::to_string(&cfg).expect("serialises");
        let again: TrustyToolsConfig = serde_yaml::from_str(&back).expect("re-parses");
        assert_eq!(cfg, again);
    }

    #[test]
    fn ticketing_config_yaml_round_trip() {
        let cfg = config_from(
            "agents:\n\
             \x20 ticketing:\n\
             \x20   ensure_labels: false\n\
             \x20   claim_comment: false\n\
             \x20   close_requires_note: false\n\
             \x20   default_assignee: bobmatnyc\n\
             \x20   pr_link_keyword: refs\n\
             \x20   lifecycle_model: /etc/issue-state.yaml\n\
             \x20   extra_labels:\n\
             \x20     - name: area/cli\n\
             \x20       color: 0E8A16\n\
             \x20       description: CLI surface\n",
        );
        let resolved = resolve_ticketing(&cfg).expect("resolves");
        assert!(!resolved.ensure_labels);
        assert!(!resolved.claim_comment);
        assert!(!resolved.close_requires_note);
        assert_eq!(resolved.default_assignee, "bobmatnyc");
        assert_eq!(resolved.pr_link_keyword, "Refs");
        assert_eq!(
            resolved.lifecycle_model.as_deref(),
            Some(std::path::Path::new("/etc/issue-state.yaml"))
        );
        assert_eq!(
            resolved.extra_labels,
            vec![PolicyLabel::new("area/cli", "0E8A16", "CLI surface")]
        );
    }

    #[test]
    fn closes_pr_link_keyword_is_rejected() {
        let cfg = config_from("agents:\n  ticketing:\n    pr_link_keyword: Closes\n");
        let err = resolve_ticketing(&cfg).expect_err("Closes is refused");
        assert_eq!(
            err,
            TicketingConfigError::PrLinkKeyword {
                found: "Closes".to_string()
            }
        );
        // The message has to name the field, or the operator cannot find it.
        assert!(
            err.to_string().contains("agents.ticketing.pr_link_keyword"),
            "{err}"
        );
    }

    #[test]
    fn refs_pr_link_keyword_is_accepted() {
        let cfg = config_from("agents:\n  ticketing:\n    pr_link_keyword: Refs\n");
        assert_eq!(
            resolve_ticketing(&cfg)
                .expect("Refs is the rule")
                .pr_link_keyword,
            "Refs"
        );
    }

    #[test]
    fn lifecycle_role_on_the_convention_label_is_rejected() {
        let cfg = config_from(
            "agents:\n\
             \x20 ticketing:\n\
             \x20   extra_labels:\n\
             \x20     - name: trusty-mpm\n\
             \x20       color: BFD4F2\n\
             \x20       role: lifecycle\n",
        );
        let err = resolve_ticketing(&cfg).expect_err("component-only is the rule");
        assert_eq!(
            err,
            TicketingConfigError::LifecycleRole {
                index: 0,
                name: "trusty-mpm".to_string()
            }
        );
        assert!(
            err.to_string()
                .contains("agents.ticketing.extra_labels[0].role"),
            "{err}"
        );
    }

    #[test]
    fn component_role_is_accepted() {
        let cfg = config_from(
            "agents:\n  ticketing:\n    extra_labels:\n      - name: trusty-mpm\n        \
             role: component\n",
        );
        let resolved = resolve_ticketing(&cfg).expect("component is fine");
        assert_eq!(resolved.extra_labels[0].name, "trusty-mpm");
    }

    #[test]
    fn blank_label_name_is_rejected() {
        let cfg = config_from("agents:\n  ticketing:\n    extra_labels:\n      - name: \"  \"\n");
        assert_eq!(
            resolve_ticketing(&cfg).expect_err("blank name"),
            TicketingConfigError::BlankLabelName { index: 0 }
        );
    }

    #[test]
    fn blank_assignee_is_rejected() {
        let cfg = config_from("agents:\n  ticketing:\n    default_assignee: \"\"\n");
        assert_eq!(
            resolve_ticketing(&cfg).expect_err("blank assignee"),
            TicketingConfigError::BlankAssignee
        );
    }

    #[test]
    fn extra_labels_extend_the_policy_set() {
        let cfg = config_from(
            "agents:\n  ticketing:\n    extra_labels:\n      - name: area/cli\n        \
             color: 0E8A16\n",
        );
        let resolved = resolve_ticketing(&cfg).expect("resolves");
        let names: Vec<String> =
            crate::core::policy_labels::policy_labels_configured(&resolved, None)
                .into_iter()
                .map(|l| l.name)
                .collect();
        assert!(names.iter().any(|n| n == "trusty-mpm"), "{names:?}");
        assert!(names.iter().any(|n| n == "area/cli"), "{names:?}");
    }

    #[test]
    fn an_extra_label_restyles_a_builtin_of_the_same_name() {
        let cfg = config_from(
            "agents:\n  ticketing:\n    extra_labels:\n      - name: trusty-mpm\n        \
             color: FF0000\n        description: ours\n",
        );
        let resolved = resolve_ticketing(&cfg).expect("resolves");
        let labels = crate::core::policy_labels::policy_labels_configured(&resolved, None);
        assert_eq!(labels.len(), 1, "restyle must not duplicate: {labels:?}");
        assert_eq!(labels[0].color, "FF0000");
        assert_eq!(labels[0].description, "ours");
    }
}
