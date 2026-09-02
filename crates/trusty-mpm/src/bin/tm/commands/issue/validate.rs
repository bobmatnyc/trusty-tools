//! Load-time validation of the issue state model (#1246, RFC §4.1).
//!
//! Why: an invalid model must be rejected with a clear, actionable error
//! *before* any `gh` mutation, so bad config never half-applies to GitHub. A
//! structured [`ModelError`] (`thiserror`) gives each failure mode a precise
//! message and keeps the checks testable in isolation.
//! What: [`validate_model`] runs every load-time rule — known version, unique
//! state names, transition refs resolve, 6-hex colors, generic terminal-edge
//! check, at most one label-less state per `gh_state`, and `bot` strategy
//! requires an identity.
//! Test: the `validate_*` tests in this file cover each rejection path.

use std::collections::BTreeSet;

use super::config::{GhState, SUPPORTED_VERSION, StateModel};

/// Structured validation failures for the issue state model.
///
/// Why: distinct variants let callers (and tests) assert the exact failure, and
/// `thiserror` renders user-facing messages without a manual `Display` impl.
/// What: one variant per load-time rule in RFC §4.1.
/// Test: each variant is produced by a `validate_*` test below.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ModelError {
    /// `version` is not the supported value.
    #[error("unsupported schema version {found}; this build supports version {supported}")]
    UnsupportedVersion {
        /// The version found in the YAML.
        found: u32,
        /// The version this build supports.
        supported: u32,
    },

    /// The model declares no states.
    #[error("state model has no states; at least one state is required")]
    NoStates,

    /// A state name appears more than once.
    #[error("duplicate state name `{0}`; state names must be unique")]
    DuplicateState(String),

    /// A transition references a state that does not exist.
    #[error("transition {from} → `{to}` references unknown state `{missing}`")]
    DanglingTransition {
        /// Rendered source (`null` or the state name).
        from: String,
        /// The declared destination.
        to: String,
        /// The name that did not resolve.
        missing: String,
    },

    /// A color is not a 6-hex-digit string.
    #[error("label `{label}` has invalid color `{color}`; expected 6 hex digits with no `#`")]
    BadColor {
        /// The label whose color is invalid.
        label: String,
        /// The offending color value.
        color: String,
    },

    /// A terminal state has an outbound transition.
    #[error("terminal state `{0}` must have no outbound transitions")]
    TerminalHasOutbound(String),

    /// The `bot` strategy was selected without an identity.
    #[error("assignee strategy `bot` requires an `identity_pattern` (or `identity_example`)")]
    BotStrategyMissingIdentity,

    /// Two label-less states share one GitHub open/closed flag.
    ///
    /// A label-less state is resolved from the issue's open/closed flag alone,
    /// so two of them on the same flag would be indistinguishable.
    #[error(
        "label-less states `{first}` and `{second}` both map to gh_state \
         `{gh_state}`; at most one label-less state per gh_state"
    )]
    AmbiguousLabellessState {
        /// The first label-less state declared for this flag.
        first: String,
        /// The second (conflicting) one.
        second: String,
        /// The shared `open`/`closed` flag.
        gh_state: &'static str,
    },
}

/// Whether `s` is exactly six hexadecimal digits (no `#`).
///
/// Why: GitHub label colors are 6-hex with no leading `#`; centralising the
/// check keeps state and extra labels consistent.
/// What: returns `true` iff `s.len() == 6` and every char is an ASCII hex digit.
/// Test: exercised via `validate_rejects_bad_hex` and `validate_default_ok`.
fn is_six_hex(s: &str) -> bool {
    s.len() == 6 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Validate the whole state model against the RFC §4.1 rules.
///
/// Why: the single gate every loader calls; returning `anyhow::Result` lets it
/// compose with the binary's error flow while the structured [`ModelError`] is
/// preserved as the source.
/// What: checks version, non-empty + unique states, transition ref integrity,
/// 6-hex colors (states + extra labels), terminal states have no outbound edge,
/// at most one label-less state per `gh_state`, and (for the `bot` strategy) an
/// identity is present.
/// Test: `validate_default_ok` and the `validate_rejects_*` tests.
pub(crate) fn validate_model(model: &StateModel) -> anyhow::Result<()> {
    validate_model_inner(model).map_err(|e| anyhow::anyhow!(e))
}

/// The inner validator returning the structured error (testable directly).
///
/// Why: tests assert exact [`ModelError`] variants; keeping the structured
/// return here while [`validate_model`] adapts to `anyhow` serves both callers.
/// What: see [`validate_model`].
/// Test: the `validate_*` tests call this directly.
fn validate_model_inner(model: &StateModel) -> Result<(), ModelError> {
    // 1. Version.
    if model.version != SUPPORTED_VERSION {
        return Err(ModelError::UnsupportedVersion {
            found: model.version,
            supported: SUPPORTED_VERSION,
        });
    }

    // 2. Non-empty + unique state names.
    if model.states.is_empty() {
        return Err(ModelError::NoStates);
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for s in &model.states {
        if !seen.insert(s.name.as_str()) {
            return Err(ModelError::DuplicateState(s.name.clone()));
        }
    }

    // 3. Transition refs resolve (null source is only allowed as the entry edge).
    for t in &model.transitions {
        if let Some(from) = &t.from
            && !seen.contains(from.as_str())
        {
            return Err(ModelError::DanglingTransition {
                from: format!("`{from}`"),
                to: t.to.clone(),
                missing: from.clone(),
            });
        }
        if !seen.contains(t.to.as_str()) {
            return Err(ModelError::DanglingTransition {
                from: t
                    .from
                    .as_ref()
                    .map(|f| format!("`{f}`"))
                    .unwrap_or_else(|| "null".to_string()),
                to: t.to.clone(),
                missing: t.to.clone(),
            });
        }
    }

    // 4. Colors are 6-hex (states + extra labels). A label-less state has no
    //    color to check.
    for s in &model.states {
        let Some(label) = &s.label else { continue };
        if !is_six_hex(&label.color) {
            return Err(ModelError::BadColor {
                label: label.name.clone(),
                color: label.color.clone(),
            });
        }
    }
    for l in &model.extra_labels {
        if !is_six_hex(&l.color) {
            return Err(ModelError::BadColor {
                label: l.name.clone(),
                color: l.color.clone(),
            });
        }
    }

    // 5. Generic terminal check: NO transition may have `from == <terminal state>`
    //    (for EVERY terminal state, not just hardcoded done/failed).
    for s in &model.states {
        if s.terminal
            && model
                .transitions
                .iter()
                .any(|t| t.from.as_deref() == Some(s.name.as_str()))
        {
            return Err(ModelError::TerminalHasOutbound(s.name.clone()));
        }
    }

    // 6. At most one label-less state per gh_state, so `resolve_current_state`
    //    can name it unambiguously from the issue's open/closed flag.
    let mut labelless: [Option<&str>; 2] = [None, None];
    for s in &model.states {
        if s.label.is_some() {
            continue;
        }
        let slot = usize::from(s.gh_state == GhState::Closed);
        match labelless[slot] {
            None => labelless[slot] = Some(s.name.as_str()),
            Some(first) => {
                return Err(ModelError::AmbiguousLabellessState {
                    first: first.to_string(),
                    second: s.name.clone(),
                    gh_state: if slot == 0 { "open" } else { "closed" },
                });
            }
        }
    }

    // 7. `bot` strategy requires an identity.
    if model.assignee_model.strategy == "bot"
        && model.assignee_model.identity_pattern.is_none()
        && model.assignee_model.identity_example.is_none()
    {
        return Err(ModelError::BotStrategyMissingIdentity);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::config::DEFAULT_MODEL_YAML;
    use super::*;

    fn default_model() -> StateModel {
        serde_yaml::from_str(DEFAULT_MODEL_YAML).expect("default parses")
    }

    #[test]
    fn validate_default_ok() {
        assert!(validate_model_inner(&default_model()).is_ok());
    }

    #[test]
    fn validate_rejects_unknown_version() {
        let mut m = default_model();
        m.version = 99;
        assert_eq!(
            validate_model_inner(&m),
            Err(ModelError::UnsupportedVersion {
                found: 99,
                supported: SUPPORTED_VERSION
            })
        );
    }

    #[test]
    fn validate_rejects_duplicate_states() {
        let mut m = default_model();
        let dup = m.states[0].clone();
        m.states.push(dup);
        let name = m.states[0].name.clone();
        assert_eq!(
            validate_model_inner(&m),
            Err(ModelError::DuplicateState(name))
        );
    }

    #[test]
    fn validate_rejects_dangling_transition_to() {
        let mut m = default_model();
        m.transitions[1].to = "nonexistent".to_string();
        let err = validate_model_inner(&m).unwrap_err();
        assert!(
            matches!(err, ModelError::DanglingTransition { ref missing, .. } if missing == "nonexistent"),
            "got: {err:?}"
        );
    }

    #[test]
    fn validate_rejects_dangling_transition_from() {
        let mut m = default_model();
        m.transitions[1].from = Some("ghost".to_string());
        let err = validate_model_inner(&m).unwrap_err();
        assert!(
            matches!(err, ModelError::DanglingTransition { ref missing, .. } if missing == "ghost"),
            "got: {err:?}"
        );
    }

    #[test]
    fn validate_rejects_bad_hex() {
        let mut m = default_model();
        m.states[0]
            .label
            .as_mut()
            .expect("every factory state is labelled")
            .color = "ZZZ".to_string();
        let err = validate_model_inner(&m).unwrap_err();
        assert!(matches!(err, ModelError::BadColor { .. }), "got: {err:?}");
    }

    #[test]
    fn validate_rejects_bad_hex_in_extra_label() {
        let mut m = default_model();
        m.extra_labels[0].color = "12345".to_string(); // 5 digits
        let err = validate_model_inner(&m).unwrap_err();
        assert!(matches!(err, ModelError::BadColor { .. }), "got: {err:?}");
    }

    #[test]
    fn validate_rejects_terminal_with_outbound_edge() {
        // Add an edge OUT of the terminal `done` state — must be rejected
        // generically (not via a hardcoded done/failed list).
        let mut m = default_model();
        m.transitions.push(super::super::config::Transition {
            from: Some("done".to_string()),
            to: "queued".to_string(),
            trigger: super::super::config::Trigger::HumanLabel,
            requires_note: false,
            description: String::new(),
        });
        assert_eq!(
            validate_model_inner(&m),
            Err(ModelError::TerminalHasOutbound("done".to_string()))
        );
    }

    #[test]
    fn validate_rejects_terminal_with_outbound_edge_for_any_state() {
        // Mark a non-default state terminal and give it an outbound edge to prove
        // the check is generic across ALL terminal states.
        let mut m = default_model();
        // Make `approved` terminal; it has an outbound edge to active-development.
        for s in &mut m.states {
            if s.name == "approved" {
                s.terminal = true;
            }
        }
        assert_eq!(
            validate_model_inner(&m),
            Err(ModelError::TerminalHasOutbound("approved".to_string()))
        );
    }

    #[test]
    fn validate_rejects_bot_strategy_without_identity() {
        let mut m = default_model();
        m.assignee_model.strategy = "bot".to_string();
        m.assignee_model.identity_pattern = None;
        m.assignee_model.identity_example = None;
        assert_eq!(
            validate_model_inner(&m),
            Err(ModelError::BotStrategyMissingIdentity)
        );
    }

    #[test]
    fn validate_rejects_two_labelless_states_on_one_gh_state() {
        // Two label-less `open` states cannot be told apart from an issue's
        // labels or its open/closed flag, so the model must be refused.
        let yaml = r#"
version: 1
label_config: { base: x, approved: x:a, blast_prefix: "b:", status_prefix: "x:" }
states:
  - { name: open, order: 0 }
  - { name: also-open, order: 1 }
transitions:
  - { from: open, to: also-open, trigger: human_label }
assignee_model: { strategy: unchanged, per_state: {} }
"#;
        let m: StateModel = serde_yaml::from_str(yaml).expect("synthetic parses");
        assert_eq!(
            validate_model_inner(&m),
            Err(ModelError::AmbiguousLabellessState {
                first: "open".to_string(),
                second: "also-open".to_string(),
                gh_state: "open",
            })
        );
    }

    #[test]
    fn validate_accepts_one_labelless_state_per_gh_state() {
        // One `open` + one `closed` label-less state IS resolvable.
        let yaml = r#"
version: 1
label_config: { base: x, approved: x:a, blast_prefix: "b:", status_prefix: "x:" }
states:
  - { name: open, gh_state: open, order: 0 }
  - { name: closed, gh_state: closed, order: 1 }
transitions:
  - { from: open, to: closed, trigger: human_label }
assignee_model: { strategy: unchanged, per_state: {} }
"#;
        let m: StateModel = serde_yaml::from_str(yaml).expect("synthetic parses");
        assert!(validate_model_inner(&m).is_ok());
    }

    #[test]
    fn validate_accepts_bot_strategy_with_identity() {
        let mut m = default_model();
        m.assignee_model.strategy = "bot".to_string();
        m.assignee_model.identity_pattern = Some("{user}-bot".to_string());
        m.assignee_model.identity_example = None;
        assert!(validate_model_inner(&m).is_ok());
    }
}
