//! Per-key presence for the config tables `extends` used to inherit wholesale
//! (#7901).
//!
//! Why: `merge_extends` started a child from its base and never read the
//! child's `[llm]` (beyond four keys), `[compress]`, `[runner_config]`,
//! `[session]`, `[plugins]`, `[rbac]` or `[workstreams]`, so a child's own
//! value in any of them was discarded with no warning. Serde collapses an
//! omitted key into its default before the merge runs, so the parsed struct
//! alone cannot say whether the child declared a value. This module records
//! that fact from the raw TOML at parse time.
//! What: [`DeclaredKeys`] is the set of `(table, key)` pairs a TOML file wrote.
//! [`inherit_tables`] applies the per-key rule: a key the child declared wins,
//! a key it omitted keeps the base's value. Every table is destructured without
//! `..`, so adding a field to one of these structs fails to compile here until
//! the field gets a merge rule; no field can be dropped silently again.
//! Test: `agents::extends::declared_tests::extends_child_inherits_every_table_it_omits`,
//! `agents::extends::declared_tests::extends_child_declared_keys_win_in_every_table`.

use std::collections::{BTreeMap, BTreeSet};

use crate::agents::AgentConfig;
use crate::agents::params::{
    AgentCompressConfig, AgentPluginsConfig, LlmParams, RbacConfig, RunnerConfig,
    SessionCompressionConfig, WorkstreamContextConfig,
};

/// The tables whose keys are recorded and merged per key.
pub const INHERITED_TABLES: &[&str] = &[
    "llm",
    "compress",
    "runner_config",
    "session",
    "plugins",
    "rbac",
    "workstreams",
];

/// Which keys of [`INHERITED_TABLES`] one TOML file declared (#7901).
///
/// Why: see the module doc. A config built in code (a `.md` agent, a
/// claude-mpm import) declares nothing, which keeps its pre-#7901 behavior:
/// every table comes from the base.
/// What: table name to the key names written under it.
/// Test: `agents::extends::declared_tests::extends_child_declared_keys_win_in_every_table`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeclaredKeys {
    tables: BTreeMap<String, BTreeSet<String>>,
}

impl DeclaredKeys {
    /// Record the keys `raw` declares under each of [`INHERITED_TABLES`].
    ///
    /// Why: the loader holds the raw text only at parse time.
    /// What: parses `raw` as a generic TOML table. A table-valued entry records
    /// its keys; a missing or non-table entry records nothing.
    /// Test: `agents::extends::declared_tests::extends_child_declared_keys_win_in_every_table`.
    ///
    /// # Errors
    /// When `raw` is not valid TOML.
    pub fn from_toml(raw: &str) -> Result<Self, toml::de::Error> {
        let document: toml::Table = toml::from_str(raw)?;
        let mut tables = BTreeMap::new();
        for name in INHERITED_TABLES {
            if let Some(toml::Value::Table(table)) = document.get(*name) {
                tables.insert(
                    (*name).to_string(),
                    table.keys().cloned().collect::<BTreeSet<_>>(),
                );
            }
        }
        Ok(Self { tables })
    }

    /// Whether this file declared `key` under `[table]`.
    pub fn has(&self, table: &str, key: &str) -> bool {
        self.tables
            .get(table)
            .is_some_and(|keys| keys.contains(key))
    }
}

/// The child's inherited tables, moved out of it by `merge_extends`.
///
/// Why: `merge_extends` has already moved other child fields by the time the
/// tables merge, so the child cannot be passed whole.
pub(super) struct ChildTables {
    pub declared: DeclaredKeys,
    pub llm: LlmParams,
    pub compress: AgentCompressConfig,
    pub runner_config: RunnerConfig,
    pub session: SessionCompressionConfig,
    pub plugins: AgentPluginsConfig,
    pub rbac: RbacConfig,
    pub workstreams: WorkstreamContextConfig,
}

/// Copy each field the child declared onto the merged table.
///
/// Why: one exhaustive destructure per table keeps the "new field needs a merge
/// rule" check in the compiler.
macro_rules! take_declared {
    ($declared:expr, $table:literal, $merged:expr, $child:expr, $ty:ident { $($field:ident),* $(,)? }) => {{
        let $ty { $($field),* } = $child;
        $(
            if $declared.has($table, stringify!($field)) {
                $merged.$field = $field;
            }
        )*
    }};
}

/// Merge the child's inherited tables onto `merged` per key (#7901).
///
/// Why: see the module doc.
/// What: `temperature`/`max_tokens` keep their UNSET-sentinel rule (a `.md`
/// child can carry a real value without a TOML key) and `aws_profile`/
/// `aws_region` keep their `Some`-wins rule (#7878). Every other key of every
/// table is taken from the child exactly when `declared` says the child wrote
/// it.
/// Test: `agents::extends::declared_tests::extends_child_inherits_every_table_it_omits`,
/// `agents::extends::declared_tests::extends_child_declared_keys_win_in_every_table`,
/// `agents::extends::tests::extends_llm_child_overrides_temperature_only`,
/// `agents::extends::tests::extends_llm_child_overrides_aws_profile_and_region`.
pub(super) fn inherit_tables(merged: &mut AgentConfig, child: ChildTables) {
    let declared = child.declared;
    merge_llm(&mut merged.llm, child.llm, &declared);
    take_declared!(
        declared,
        "compress",
        merged.compress,
        child.compress,
        AgentCompressConfig {
            enabled,
            token_budget,
            compress_task,
            output_style,
        }
    );
    take_declared!(
        declared,
        "runner_config",
        merged.runner_config,
        child.runner_config,
        RunnerConfig { max_tool_calls }
    );
    take_declared!(
        declared,
        "session",
        merged.session,
        child.session,
        SessionCompressionConfig {
            enabled,
            compression_threshold,
            keep_recent_turns,
            compression_model,
        }
    );
    take_declared!(
        declared,
        "plugins",
        merged.plugins,
        child.plugins,
        AgentPluginsConfig { python }
    );
    take_declared!(
        declared,
        "rbac",
        merged.rbac,
        child.rbac,
        RbacConfig {
            allowed_users_env,
            default_tier,
            unauthenticated_tier,
        }
    );
    take_declared!(
        declared,
        "workstreams",
        merged.workstreams,
        child.workstreams,
        WorkstreamContextConfig {
            enabled,
            summarize_every,
            recent_window,
        }
    );
}

/// The `[llm]` half of [`inherit_tables`].
fn merge_llm(merged: &mut LlmParams, child: LlmParams, declared: &DeclaredKeys) {
    // #3052 PR A: the sentinel, not key presence, marks an omitted value.
    if !child.temperature_is_unset() {
        merged.temperature = child.temperature;
    }
    if !child.max_tokens_is_unset() {
        merged.max_tokens = child.max_tokens;
    }
    let LlmParams {
        temperature: _,
        max_tokens: _,
        aws_profile,
        aws_region,
        model_override,
        enable_prompt_caching,
        max_turns,
        persona_max_turns,
        tool_choice,
        use_finish_task,
        use_anthropic_direct,
        claude_allowed_tools,
        elevation_threshold,
        elevation_model,
        stop_sequences,
        routing_model,
        thinking_enabled,
    } = child;
    // #7878: both are `Option`, so `Some` already means declared.
    if aws_profile.is_some() {
        merged.aws_profile = aws_profile;
    }
    if aws_region.is_some() {
        merged.aws_region = aws_region;
    }
    // #7901: every remaining key follows the child when the child wrote it.
    macro_rules! llm_key {
        ($($field:ident),* $(,)?) => {
            $(
                if declared.has("llm", stringify!($field)) {
                    merged.$field = $field;
                }
            )*
        };
    }
    llm_key!(
        model_override,
        enable_prompt_caching,
        max_turns,
        persona_max_turns,
        tool_choice,
        use_finish_task,
        use_anthropic_direct,
        claude_allowed_tools,
        elevation_threshold,
        elevation_model,
        stop_sequences,
        routing_model,
        thinking_enabled,
    );
}
