//! YAML schema types + load/discovery for the `tm issue` state model (#1246).
//!
//! Why: the issue state machine (label set, allowed transitions, assignee model)
//! is *configuration*, not harness code. This module defines the serde shape of
//! that YAML contract, embeds the Unicorn Factory default via `include_str!`, and
//! resolves which model to load (flag > CWD file > user config > embedded
//! default, RFC §6). Validation lives in the sibling `validate` module to keep
//! both files under the 500-SLOC production cap.
//! What: the [`StateModel`] root and its nested types ([`LabelConfig`],
//! [`StateDef`], [`StateLabel`], [`ExtraLabel`], [`Transition`], [`Trigger`],
//! [`AssigneeModel`]), the embedded [`DEFAULT_MODEL_YAML`], and the loader
//! ([`load_model`] / [`resolve_config_path`] / [`user_config_path`]).
//! Test: round-trip + discovery tests in this file; validation tests in
//! `validate.rs`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The embedded default state model — the exact Unicorn Factory schema (#1246).
///
/// Why: every `tm issue` invocation must work with zero on-disk config; the
/// committed example is compiled in as the always-available fallback (mirrors
/// `tm services`' `DEFAULT_MANIFEST_YAML`).
/// What: the verbatim contents of `examples/issue-state/unicorn-factory.yaml`.
/// Test: `embedded_default_parses`, `embedded_default_round_trips`.
pub(crate) const DEFAULT_MODEL_YAML: &str =
    include_str!("../../../../../examples/issue-state/unicorn-factory.yaml");

/// The only schema version this build understands.
///
/// Why: load-time version gating lets a future breaking schema change be
/// rejected with a clear error instead of mis-parsing.
/// What: the integer compared against `StateModel.version`.
/// Test: `validate_rejects_unknown_version` (in `validate.rs`).
pub(crate) const SUPPORTED_VERSION: u32 = 1;

/// Root of the issue state-model YAML.
///
/// Why: the single deserialization target for the whole contract; everything
/// `tm issue` does is driven by these fields, never by hardcoded label strings.
/// What: schema `version`, the `label_config` family prefixes, the ordered
/// `states`, the non-state `extra_labels`, the `transitions` graph, and the
/// `assignee_model`.
/// Test: `embedded_default_parses` deserializes the full factory model.
///
/// `Eq` is intentionally NOT derived: `AssigneeModel` carries opaque
/// `serde_yaml::Value` fields which are only `PartialEq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct StateModel {
    /// Schema version (must equal [`SUPPORTED_VERSION`]).
    pub(crate) version: u32,
    /// Label-family prefixes (base/approved/blast/status).
    pub(crate) label_config: LabelConfig,
    /// Ordered lifecycle states.
    pub(crate) states: Vec<StateDef>,
    /// Non-state label families seeded by bootstrap.
    #[serde(default)]
    pub(crate) extra_labels: Vec<ExtraLabel>,
    /// Allowed `from → to` transition edges.
    pub(crate) transitions: Vec<Transition>,
    /// Per-state assignee / identity model.
    pub(crate) assignee_model: AssigneeModel,
}

/// Configurable label-family prefixes.
///
/// Why: the canonical `unicorn:*` / `blast:*` namespace is configurable so other
/// consumers can rename the families without code changes.
/// What: the base/ownership label, the approval-gate label, and the blast/status
/// prefixes.
/// Test: parsed as part of `embedded_default_parses`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LabelConfig {
    /// Ownership/base label applied to every work item (e.g. `unicorn`).
    pub(crate) base: String,
    /// The approval-gate label (e.g. `unicorn:approved`).
    pub(crate) approved: String,
    /// Prefix for blast-radius labels (e.g. `blast:`).
    pub(crate) blast_prefix: String,
    /// Prefix for the lifecycle labels (e.g. `unicorn:`).
    pub(crate) status_prefix: String,
}

/// One lifecycle state and the GitHub label that represents it.
///
/// Why: a state's visible artifact is its label; bundling the name, label,
/// ordering, and terminal flag keeps the state machine and the seeding driven by
/// one source.
/// What: the machine `name` (the unique key used by `tm issue transition`), the
/// state `label`, an optional `order`, and a `terminal` flag.
/// Test: `embedded_default_parses`; terminal-edge checks live in `validate.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StateDef {
    /// Machine state name (e.g. `queued`).
    pub(crate) name: String,
    /// The GitHub label representing this state.
    pub(crate) label: StateLabel,
    /// Optional display/sort ordering (informational; does not gate transitions).
    #[serde(default)]
    pub(crate) order: Option<u32>,
    /// `true` for terminal states (no outbound edges allowed).
    #[serde(default)]
    pub(crate) terminal: bool,
}

/// A state's GitHub label (name + color + description).
///
/// Why: `seed-labels` needs the exact name/color/description to create the label.
/// What: the label `name`, 6-hex `color` (no `#`), and optional `description`.
/// Test: `embedded_default_parses`; hex validation in `validate.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StateLabel {
    /// Label name (e.g. `unicorn:queued`).
    pub(crate) name: String,
    /// 6-hex color, no `#`.
    pub(crate) color: String,
    /// Optional label description.
    #[serde(default)]
    pub(crate) description: String,
}

/// A non-state label family member (ownership/blast/PR-tier/approval).
///
/// Why: bootstrap seeds these alongside the state labels even though they are not
/// part of the transition graph.
/// What: the label `name`, `color`, and optional `description`.
/// Test: `embedded_default_parses`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExtraLabel {
    /// Label name (e.g. `blast:high`, `T2`, `approval:level-1`).
    pub(crate) name: String,
    /// 6-hex color, no `#`.
    pub(crate) color: String,
    /// Optional label description.
    #[serde(default)]
    pub(crate) description: String,
}

/// One allowed `from → to` edge in the state machine.
///
/// Why: enumerating the legal edges is what lets `tm issue transition` reject
/// illegal moves before any `gh` mutation.
/// What: `from` (a state name, or `None` for the creation edge), `to` (a state
/// name), the `trigger` annotation, and an optional human `description`.
/// Test: `embedded_default_parses`; edge checks in `state.rs`/`validate.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Transition {
    /// Source state name, or `None` for the `null → <entry>` creation edge.
    #[serde(default)]
    pub(crate) from: Option<String>,
    /// Destination state name.
    pub(crate) to: String,
    /// What drives this edge.
    pub(crate) trigger: Trigger,
    /// Optional human description.
    #[serde(default)]
    pub(crate) description: String,
}

/// What drives a transition edge.
///
/// Why: typing the trigger as an enum rejects unknown trigger strings at load
/// time and documents who/what performs each edge.
/// What: the five recognised triggers from the factory model.
/// Test: `embedded_default_parses`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Trigger {
    /// Issue creation (`null → entry`).
    IssueCreated,
    /// A human applies a label.
    HumanLabel,
    /// The executor starts work.
    ExecutorStart,
    /// The executor completes successfully.
    ExecutorComplete,
    /// The executor fails.
    ExecutorFailure,
}

/// The assignee / identity model.
///
/// Why: who gets assigned (and how the bot identity is derived for git
/// attribution) is part of the externalized contract.
/// What: the `strategy`, the `identity_pattern`/`identity_example`, the
/// `git_attribution` map, and the `per_state` assignee rules. `git_attribution`
/// is kept as opaque YAML (`serde_yaml::Value`) because `tm issue` does not act
/// on it — it is a git-config concern owned by the consuming harness.
/// Test: `embedded_default_parses`; strategy validation in `validate.rs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AssigneeModel {
    /// Assignment strategy (e.g. `bot_identity`).
    pub(crate) strategy: String,
    /// How the bot identity is derived (free-form template).
    #[serde(default)]
    pub(crate) identity_pattern: Option<String>,
    /// An example identity (informational).
    #[serde(default)]
    pub(crate) identity_example: Option<String>,
    /// Git attribution block (opaque to `tm issue`; consumed by the harness).
    #[serde(default)]
    pub(crate) git_attribution: Option<serde_yaml::Value>,
    /// Per-state assignee rules (opaque values: `unchanged` or a template).
    #[serde(default)]
    pub(crate) per_state: std::collections::BTreeMap<String, serde_yaml::Value>,
}

/// The on-disk basename used for both the CWD and user-config locations.
///
/// Why: the project file and the user-config file share a name and differ only
/// by location (RFC §6); naming it once keeps the two in sync.
/// What: `issue-state.yaml`.
/// Test: `user_config_path_uses_basename`.
pub(crate) const CONFIG_BASENAME: &str = "issue-state.yaml";

/// The user-config path: `~/.trusty-tools/trusty-mpm/issue-state.yaml`.
///
/// Why: aligns with the #1220 `~/.trusty-tools/<crate>/config.yaml` convention.
/// What: joins `dirs::home_dir()` with the trusty-tools/trusty-mpm subpath.
/// Test: `user_config_path_uses_basename`.
pub(crate) fn user_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| {
        h.join(".trusty-tools")
            .join("trusty-mpm")
            .join(CONFIG_BASENAME)
    })
}

/// Resolve which config path to load, by precedence (RFC §6).
///
/// Why: a single, testable precedence resolver keeps the discovery rule in one
/// place: `--config` flag > `./issue-state.yaml` > user config > (None ⇒
/// embedded default).
/// What: returns `Some(path)` for the first location that exists, or `None` to
/// signal "use the embedded default". An explicit `--config` path is returned
/// even if missing, so the loader can surface a clear not-found error.
/// Test: `resolve_prefers_flag`, `resolve_prefers_cwd`, `resolve_none_means_default`.
pub(crate) fn resolve_config_path(
    flag: Option<&Path>,
    cwd_exists: bool,
    user_path: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(f) = flag {
        return Some(f.to_path_buf());
    }
    if cwd_exists {
        return Some(PathBuf::from(CONFIG_BASENAME));
    }
    if let Some(u) = user_path
        && u.exists()
    {
        return Some(u.to_path_buf());
    }
    None
}

/// Load + validate the effective state model, honouring the discovery precedence.
///
/// Why: the one entry point every `tm issue` verb calls to obtain a validated
/// model; folding discovery + parse + validate here keeps the verbs thin.
/// What: resolves the path (flag > CWD > user > embedded), reads + parses the
/// YAML (or the embedded default when no file is found), then runs
/// [`super::validate::validate_model`]; returns the validated [`StateModel`].
/// Test: `load_embedded_default_ok`, `load_explicit_missing_errors`.
pub(crate) fn load_model(flag: Option<&Path>) -> anyhow::Result<StateModel> {
    let user = user_config_path();
    let cwd_path = PathBuf::from(CONFIG_BASENAME);
    let chosen = resolve_config_path(flag, cwd_path.exists(), user.as_deref());

    let yaml = match &chosen {
        Some(path) => std::fs::read_to_string(path).map_err(|e| {
            anyhow::anyhow!(
                "failed to read issue-state config `{}`: {e}",
                path.display()
            )
        })?,
        None => DEFAULT_MODEL_YAML.to_string(),
    };

    let model: StateModel = serde_yaml::from_str(&yaml).map_err(|e| {
        let src = chosen
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "embedded default".to_string());
        anyhow::anyhow!("failed to parse issue-state model ({src}): {e}")
    })?;
    super::validate::validate_model(&model)?;
    Ok(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_default_parses() {
        let m: StateModel = serde_yaml::from_str(DEFAULT_MODEL_YAML).expect("default parses");
        assert_eq!(m.version, SUPPORTED_VERSION);
        assert_eq!(m.label_config.base, "unicorn");
        // States: the 7 unicorn:* lifecycle states; no `in-review`.
        let names: Vec<&str> = m.states.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "queued",
                "approved",
                "active-development",
                "paused",
                "blocked",
                "done",
                "failed"
            ]
        );
        assert!(
            !names.contains(&"in-review"),
            "there must be no in-review state"
        );
        // Terminal states.
        let terminals: Vec<&str> = m
            .states
            .iter()
            .filter(|s| s.terminal)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(terminals, vec!["done", "failed"]);
        // The creation edge is null → queued.
        let entry = m
            .transitions
            .iter()
            .find(|t| t.from.is_none())
            .expect("creation edge");
        assert_eq!(entry.to, "queued");
        assert_eq!(entry.trigger, Trigger::IssueCreated);
        // Assignee strategy is bot_identity (attribution-only).
        assert_eq!(m.assignee_model.strategy, "bot_identity");
        // Extra label families are present (unicorn, blast:*, T2-4, approval:*).
        assert!(m.extra_labels.iter().any(|l| l.name == "blast:high"));
        assert!(m.extra_labels.iter().any(|l| l.name == "approval:level-1"));
    }

    #[test]
    fn embedded_default_round_trips() {
        // Parse → serialize → parse must be stable (proves Serialize is complete).
        let m: StateModel = serde_yaml::from_str(DEFAULT_MODEL_YAML).expect("parse");
        let s = serde_yaml::to_string(&m).expect("serialize");
        let m2: StateModel = serde_yaml::from_str(&s).expect("reparse");
        assert_eq!(m, m2);
    }

    #[test]
    fn load_embedded_default_ok() {
        // No flag, and (in CI/clean env) no on-disk file → embedded default.
        // To make this deterministic regardless of CWD, exercise the parse path
        // the same way load_model does for the None case.
        let model: StateModel = serde_yaml::from_str(DEFAULT_MODEL_YAML).expect("default parses");
        super::super::validate::validate_model(&model).expect("default is valid");
    }

    #[test]
    fn load_explicit_missing_errors() {
        let missing = Path::new("/nonexistent/issue-state-does-not-exist.yaml");
        let err = load_model(Some(missing)).unwrap_err().to_string();
        assert!(err.contains("failed to read"), "got: {err}");
    }

    #[test]
    fn resolve_prefers_flag() {
        let flag = PathBuf::from("/tmp/custom.yaml");
        let got = resolve_config_path(Some(&flag), true, None);
        assert_eq!(got, Some(flag));
    }

    #[test]
    fn resolve_prefers_cwd() {
        // No flag, CWD file exists → the CWD basename wins over user/default.
        let got = resolve_config_path(None, true, None);
        assert_eq!(got, Some(PathBuf::from(CONFIG_BASENAME)));
    }

    #[test]
    fn resolve_none_means_default() {
        // No flag, no CWD file, no user file → None (use embedded default).
        let got = resolve_config_path(None, false, None);
        assert_eq!(got, None);
    }

    #[test]
    fn user_config_path_uses_basename() {
        if let Some(p) = user_config_path() {
            assert!(p.ends_with(CONFIG_BASENAME));
            assert!(p.to_string_lossy().contains(".trusty-tools"));
            assert!(p.to_string_lossy().contains("trusty-mpm"));
        }
    }
}
