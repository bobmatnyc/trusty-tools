//! YAML schema types + load/discovery for the `tm issue` state model (#1246).
//!
//! Why: the issue state machine (label set, allowed transitions, assignee model)
//! is *configuration*, not harness code. This module defines the serde shape of
//! that YAML contract, embeds the Unicorn Factory default via `include_str!`, and
//! resolves which model to load (flag > CWD file > `agents.ticketing.
//! lifecycle_model` (#6918) > user config > embedded default, RFC §6).
//! Validation lives in the sibling `validate` module to keep
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
    /// The GitHub label representing this state, or `None` for a *label-less*
    /// state — one whose only artifact is the ABSENCE of every state label
    /// (e.g. trusty-tools' `open`, and `closed` which is GitHub's own state).
    #[serde(default)]
    pub(crate) label: Option<StateLabel>,
    /// Whether this state means the GitHub issue is open or closed.
    #[serde(default)]
    pub(crate) gh_state: GhState,
    /// Optional display/sort ordering (informational; does not gate transitions).
    #[serde(default)]
    pub(crate) order: Option<u32>,
    /// `true` for terminal states (no outbound edges allowed).
    #[serde(default)]
    pub(crate) terminal: bool,
}

/// Whether a state corresponds to an open or a closed GitHub issue.
///
/// Why: a lifecycle can end by CLOSING the issue rather than by labelling it
/// (trusty-tools' `status:tested → closed`). Recording that on the state lets
/// `tm issue transition` close the issue, and lets a label-less state be
/// resolved from the issue's own open/closed flag rather than from a label.
/// What: `Open` (the default, so every pre-existing model keeps its meaning)
/// and `Closed`.
/// Test: `sm_closes_issue`, `sm_resolve_falls_back_to_labelless`,
/// `project_close_requires_evidence_and_then_closes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GhState {
    /// The issue is open (every labelled lifecycle state).
    #[default]
    Open,
    /// The issue is closed.
    Closed,
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
    /// When `true`, `tm issue transition` refuses this edge unless `--note` is
    /// given — the note is the evidence the edge exists to record.
    #[serde(default)]
    pub(crate) requires_note: bool,
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

/// The repo's own `issue-state.yaml`, searched from `start` up to the git
/// toplevel (#7580).
///
/// Why: resolution used to test `./issue-state.yaml` against the process cwd and
/// nothing else, so `tm issue transition N status:merged` run from
/// `crates/trusty-mpm` could not see the model committed at the repo root and
/// fell through to the embedded default — whose error then read as a bad state
/// name. `git` itself resolves repo-relative configuration by walking upward, so
/// this walks the same way and stops at the same boundary.
/// What: checks `start` and each ancestor for [`CONFIG_BASENAME`], returning the
/// first hit as an absolute path. The walk STOPS after inspecting a directory
/// that contains `.git` — that directory is the git toplevel (or a linked
/// worktree's pointer file), and a model above it belongs to a different repo.
/// A `start` outside any repository is walked to the filesystem root, which is
/// the pre-#7580 "nothing found" outcome for every such caller.
/// Test: `discover_finds_the_repo_root_model_from_a_crate_subdirectory_7580`,
/// `discover_stops_at_the_git_toplevel_7580`, `discover_finds_nothing_when_absent`.
pub(crate) fn discover_upward(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(current) = dir {
        let candidate = current.join(CONFIG_BASENAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        // The toplevel is the last directory searched: a sibling repository's
        // model is not this repository's configuration.
        if current.join(".git").exists() {
            return None;
        }
        dir = current.parent();
    }
    None
}

/// Where the effective state model came from (#7580).
///
/// Why: "no model was found, so the built-in one is in force" and "the project's
/// model is in force" produce identical-looking verb output, and the difference
/// is the whole of #7580's confusion — an unknown-state error listing the
/// built-in states reads as a typo rather than as a missing config.
/// What: the resolved file, or the embedded default plus the directory the
/// upward search started from.
/// Test: `describe_source_names_the_embedded_default_7580`,
/// `describe_source_names_the_file`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModelSource {
    /// The model was read from this file.
    File(PathBuf),
    /// No file was found or configured; the compiled-in default is in force.
    EmbeddedDefault {
        /// The directory [`discover_upward`] searched from.
        searched_from: PathBuf,
    },
}

/// One line naming the model in force, for a verb's output or error (#7580).
///
/// Why: see [`ModelSource`]. The embedded-default arm names the search root and
/// both escapes (`--config`, running from the repo root) because an operator who
/// hits this is, by construction, looking at state names that are not theirs.
/// What: a single line, no trailing newline.
/// Test: `describe_source_names_the_embedded_default_7580`,
/// `describe_source_names_the_file`.
pub(crate) fn describe_source(source: &ModelSource) -> String {
    match source {
        ModelSource::File(path) => format!("state model: {}", path.display()),
        ModelSource::EmbeddedDefault { searched_from } => format!(
            "state model: the BUILT-IN default — no `{CONFIG_BASENAME}` was found from {} up to \
             the git toplevel, and none is configured, so the states above are the built-in ones \
             and not this project's. Run from the repository root or pass \
             `--config <repo-root>/{CONFIG_BASENAME}` (#7580)",
            searched_from.display()
        ),
    }
}

/// Resolve which config path to load, by precedence (RFC §6).
///
/// Why: a single, testable precedence resolver keeps the discovery rule in one
/// place: `--config` flag > the repo's own `issue-state.yaml` (found by
/// [`discover_upward`] since #7580) > `agents.ticketing.lifecycle_model` > user
/// config > (None ⇒ embedded default).
/// What: returns `Some(path)` for the first location that exists, or `None` to
/// signal "use the embedded default". An explicit `--config` path is returned
/// even if missing, so the loader can surface a clear not-found error.
///
/// #6918 slotted `configured` in BELOW the repo's own `issue-state.yaml`: it
/// is a host-level default for repos that ship no model, not an override of a
/// model a project committed and reviewed.
/// Test: `resolve_prefers_flag`, `resolve_prefers_cwd`, `resolve_none_means_default`,
/// `resolve_prefers_cwd_over_configured`, `resolve_uses_configured_before_user`.
pub(crate) fn resolve_config_path(
    flag: Option<&Path>,
    discovered: Option<PathBuf>,
    configured: Option<&Path>,
    user_path: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(f) = flag {
        return Some(f.to_path_buf());
    }
    if let Some(found) = discovered {
        return Some(found);
    }
    // #6918: an explicitly configured path is returned even when missing, so
    // the loader reports it by name rather than silently using the embedded
    // default — the same reasoning as the `--config` flag above.
    if let Some(c) = configured {
        return Some(c.to_path_buf());
    }
    if let Some(u) = user_path
        && u.exists()
    {
        return Some(u.to_path_buf());
    }
    None
}

/// Load + validate the effective state model, and name where it came from.
///
/// Why: the one entry point every `tm issue` verb calls to obtain a validated
/// model; folding discovery + parse + validate here keeps the verbs thin.
///
/// #6918: `configured` is `agents.ticketing.lifecycle_model` from
/// `~/.trusty-tools/trusty-mpm/config.yaml`. The lifecycle is REFERENCED from
/// that block, never embedded in it.
///
/// #7580: the dispatcher annotates a verb's failure with the model in force, and
/// only the loader knows which of the five locations answered. Returning it is
/// what lets an unknown-state error say "these are the BUILT-IN states" instead
/// of listing them as if they were the project's.
/// What: resolves the search root with [`std::env::current_dir`] and delegates
/// to [`load_model_in`], which is the testable half — the cwd read is the only
/// thing this wrapper adds.
/// Test: covered through [`load_model_in`]'s tests.
pub(crate) fn load_model_with_source(
    flag: Option<&Path>,
    configured: Option<&Path>,
) -> anyhow::Result<(StateModel, ModelSource)> {
    // A cwd the process cannot read is not a reason to silently use the
    // built-in model: the search root is part of the answer, so say so.
    let start = std::env::current_dir().map_err(|e| {
        anyhow::anyhow!(
            "cannot resolve the current directory to search for `{CONFIG_BASENAME}` from: {e}"
        )
    })?;
    load_model_in(&start, flag, configured)
}

/// [`load_model_with_source`] against an explicit search root (#7580).
///
/// Why: discovery walks upward from a directory, and taking that directory as a
/// parameter is what keeps the whole rule testable without mutating the
/// process's cwd — a process-global write this binary's `env_isolation_tests.rs`
/// ratchet forbids in any case.
/// What: resolves the path (flag > upward walk from `start` > `configured` >
/// user > embedded), reads and parses the YAML (or the embedded default when no
/// file is found), then runs [`super::validate::validate_model`], returning the
/// validated model and its [`ModelSource`].
/// Test: `load_in_finds_the_repo_root_model_from_a_crate_subdirectory_7580`,
/// `load_in_reports_the_embedded_default_source_7580`,
/// `load_explicit_missing_errors`.
pub(crate) fn load_model_in(
    start: &Path,
    flag: Option<&Path>,
    configured: Option<&Path>,
) -> anyhow::Result<(StateModel, ModelSource)> {
    let user = user_config_path();
    let discovered = if flag.is_some() {
        None
    } else {
        discover_upward(start)
    };
    let chosen = resolve_config_path(flag, discovered, configured, user.as_deref());

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
    let source = match chosen {
        Some(path) => ModelSource::File(path),
        None => ModelSource::EmbeddedDefault {
            searched_from: start.to_path_buf(),
        },
    };
    Ok((model, source))
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
        let err = load_model_in(Path::new("/"), Some(missing), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("failed to read"), "got: {err}");
    }

    #[test]
    fn resolve_prefers_flag() {
        let flag = PathBuf::from("/tmp/custom.yaml");
        let repo = PathBuf::from("/repo").join(CONFIG_BASENAME);
        let got = resolve_config_path(Some(&flag), Some(repo), None, None);
        assert_eq!(got, Some(flag));
    }

    #[test]
    fn resolve_prefers_cwd() {
        // No flag, the repo's own model was discovered → it wins over
        // user/default.
        let repo = PathBuf::from("/repo").join(CONFIG_BASENAME);
        let got = resolve_config_path(None, Some(repo.clone()), None, None);
        assert_eq!(got, Some(repo));
    }

    #[test]
    fn resolve_none_means_default() {
        // No flag, nothing discovered, no user file → None (embedded default).
        let got = resolve_config_path(None, None, None, None);
        assert_eq!(got, None);
    }

    #[test]
    fn resolve_prefers_cwd_over_configured() {
        // #6918: a model the project committed beats the host-level config
        // block — the block is a default for repos that ship none.
        let configured = PathBuf::from("/tmp/host-issue-state.yaml");
        let repo = PathBuf::from("/repo").join(CONFIG_BASENAME);
        let got = resolve_config_path(None, Some(repo.clone()), Some(&configured), None);
        assert_eq!(got, Some(repo));
    }

    #[test]
    fn resolve_uses_configured_before_user() {
        // #6918: with no flag and no repo model, the configured path wins over
        // the user config — and is returned even when missing, so the loader
        // names it rather than silently falling back to the embedded default.
        let configured = PathBuf::from("/tmp/host-issue-state.yaml");
        let user = PathBuf::from("/tmp/user-issue-state.yaml");
        let got = resolve_config_path(None, None, Some(&configured), Some(&user));
        assert_eq!(got, Some(configured));
    }

    /// A repo laid out like this one: `.git` and a model at the root, a crate
    /// subdirectory two levels down. Returns `(tempdir, root, subdir)`.
    fn repo_with_model() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join(".git")).expect("git dir");
        std::fs::write(root.join(CONFIG_BASENAME), DEFAULT_MODEL_YAML).expect("model");
        let sub = root.join("crates").join("trusty-mpm");
        std::fs::create_dir_all(&sub).expect("subdir");
        (tmp, root, sub)
    }

    // #7580: the reported repro — resolution from a crate subdirectory must
    // find the model committed at the repository root.
    #[test]
    fn discover_finds_the_repo_root_model_from_a_crate_subdirectory_7580() {
        let (_tmp, root, sub) = repo_with_model();
        assert_eq!(discover_upward(&sub), Some(root.join(CONFIG_BASENAME)));
    }

    // #7580: the walk stops at the git toplevel — a model in a PARENT of the
    // repository belongs to something else and must never be adopted.
    #[test]
    fn discover_stops_at_the_git_toplevel_7580() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outer = tmp.path();
        std::fs::write(outer.join(CONFIG_BASENAME), DEFAULT_MODEL_YAML).expect("outer model");
        let repo = outer.join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git dir");
        let sub = repo.join("crates");
        std::fs::create_dir_all(&sub).expect("subdir");
        assert_eq!(discover_upward(&sub), None);
    }

    #[test]
    fn discover_finds_nothing_when_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".git")).expect("git dir");
        assert_eq!(discover_upward(tmp.path()), None);
    }

    // #7580: the loader run from the crate subdirectory loads the REPO's model
    // and names it as the source — not the embedded default.
    #[test]
    fn load_in_finds_the_repo_root_model_from_a_crate_subdirectory_7580() {
        let (_tmp, root, sub) = repo_with_model();
        let (_model, source) = load_model_in(&sub, None, None).expect("loads");
        assert_eq!(source, ModelSource::File(root.join(CONFIG_BASENAME)));
    }

    // #7580: when nothing is found the source says so, carrying the directory
    // the search started from.
    #[test]
    fn load_in_reports_the_embedded_default_source_7580() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".git")).expect("git dir");
        // A user config on the developer's host would outrank the default, so
        // this only asserts the source when the fallback is actually reached.
        let (_model, source) = load_model_in(tmp.path(), None, None).expect("loads");
        if let ModelSource::EmbeddedDefault { searched_from } = source {
            assert_eq!(searched_from, tmp.path());
        }
    }

    // #7580: the fallback notice names the built-in model and both escapes.
    #[test]
    fn describe_source_names_the_embedded_default_7580() {
        let note = describe_source(&ModelSource::EmbeddedDefault {
            searched_from: PathBuf::from("/repo/crates/trusty-mpm"),
        });
        assert!(note.contains("BUILT-IN default"), "{note}");
        assert!(note.contains("/repo/crates/trusty-mpm"), "{note}");
        assert!(note.contains("--config"), "{note}");
    }

    #[test]
    fn describe_source_names_the_file() {
        let note = describe_source(&ModelSource::File(PathBuf::from("/repo/issue-state.yaml")));
        assert_eq!(note, "state model: /repo/issue-state.yaml");
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
