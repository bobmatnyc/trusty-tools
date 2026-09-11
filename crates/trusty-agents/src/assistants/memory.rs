//! One trusty-memory palace per assistant, with opt-in fan-out to other
//! assistants' palaces (#7428, epic #7425; product spec §1a item 3, §5.1).
//!
//! Why: an assistant's palace used to be whatever `[[stores]] palace` said, and
//! nothing else. An assistant without that key had NO palace at all — the chat
//! path reported `NoPalaceBound` and recalled nothing, so a new assistant
//! started stateless and stayed stateless until someone hand-edited
//! `agent.toml`. There was also no way to read another assistant's palace, so
//! the only cross-assistant knowledge path was agent-to-agent chat. The owner's
//! 1.0 model is the inverse: every assistant HAS a palace, and reading another
//! one is a setting the user turns on per assistant.
//! What: [`MemoryConfig`] is the `[memory]` table in an assistant home's
//! `config.toml` — `palace` (an explicit id for this assistant) and `fan_out`
//! (other assistant INSTANCE ids whose palaces are also recalled from).
//! [`own_palace`] is the ONE resolution rule, applied identically to this
//! assistant and to every fan-out target: the `[[stores]]` binding's `palace`
//! when it declares one, else `[memory] palace`, else the instance id itself.
//! The binding wins so the shipped `cto` and `owner-profile` bindings keep
//! reading the same palaces they always did — this change migrates no data.
//! [`resolve_palace_plan_in`] turns that rule plus the fan-out list into the
//! [`PalacePlan`] the chat path recalls against.
//!
//! Writes are deliberately NOT part of the plan's fan-out: a turn is persisted
//! to [`PalacePlan::own`] only, because fan-out is a READ grant. Two assistants
//! writing each other's memory would make the owning assistant's palace
//! unownable, which is the opposite of "one palace per assistant".
//!
//! Test: `super::tests::memory_tests` — the whole module.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::AssistantError;
use super::home::{AssistantHome, AssistantHomeConfig};
use super::instance::AssistantInstanceId;
use crate::stores::AgentStoreBinding;

/// The `[memory]` table of an assistant home's `config.toml` (#7428).
///
/// Why: the palace an assistant reads and the OTHER assistants it may read from
/// are per-instance user settings, so they belong in the instance's own
/// `config.toml` rather than in the shared agent package. Both fields are
/// `#[serde(default)]`, so every `config.toml` written before this existed
/// still parses — an absent `[memory]` table is the same value as an empty one.
/// What: `palace` names this assistant's palace explicitly (used only when the
/// `[[stores]]` binding declares none — see [`own_palace`]); `fan_out` lists
/// OTHER assistant instance ids whose palaces this assistant also recalls from.
/// A fan-out entry is an INSTANCE id, never a palace id: the palace behind it is
/// resolved by the same rule, so renaming a palace never strands the setting.
/// Test: `super::tests::memory_tests::config_defaults_when_the_table_is_absent`,
/// `super::tests::memory_tests::fan_out_ids_resolve_through_the_same_rule`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct MemoryConfig {
    /// This assistant's palace id, when the user pins one. Absent = derived.
    #[serde(default)]
    pub palace: Option<String>,
    /// Other assistant instance ids whose palaces are also recalled from.
    #[serde(default)]
    pub fan_out: Vec<String>,
}

/// Which rule produced an assistant's own palace id.
///
/// Why: the GUI and the persona's introspection block both have to say WHERE
/// the palace came from — a binding-pinned palace is operator-declared and
/// pre-existing, while a derived one is app-generated and created on first use.
/// Collapsing the two would make the create-on-first-use path either skip a
/// palace that needs creating or rewrite one it does not own.
/// Test: `super::tests::memory_tests::binding_palace_wins_over_config_and_id`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PalaceSource {
    /// `[[stores]] palace` in `agent.toml` — the pre-#7428 spelling.
    Binding,
    /// `[memory] palace` in the assistant home's `config.toml`.
    Config,
    /// Derived from the assistant instance id (the #7428 default).
    InstanceId,
    /// Nothing resolved: the agent name is not a usable instance id.
    #[default]
    Unresolved,
}

/// The palaces one chat turn reads from, in recall order.
///
/// Why/What/Test: see this module's doc comment; `own` is always recalled first
/// and is the only palace written to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PalacePlan {
    /// This assistant's own palace. `None` only when the agent name is not a
    /// usable [`AssistantInstanceId`] and its binding pins no palace.
    pub own: Option<String>,
    /// Which rule produced [`Self::own`].
    pub source: PalaceSource,
    /// Resolved palace ids of the opted-in fan-out targets, in config order.
    /// Never contains [`Self::own`], and never repeats a palace.
    pub fan_out: Vec<String>,
}

impl PalacePlan {
    /// Whether [`Self::own`] is an operator-declared binding palace.
    ///
    /// Why: a binding palace already exists (it is why the binding names it), so
    /// the chat path must not issue `palace_create` against it. A derived palace
    /// is this crate's to create.
    /// Test: `super::tests::memory_tests::binding_palace_wins_over_config_and_id`.
    pub fn own_is_bound(&self) -> bool {
        self.source == PalaceSource::Binding
    }
}

/// The palace `name` reads and writes — the ONE rule, used everywhere.
///
/// Why: applying a different rule to this assistant than to a fan-out target
/// would let a recall reach a palace the target itself never writes to. One
/// function, called from both positions, is what makes that impossible.
/// What: the binding's non-blank `palace`, else the config's non-blank `palace`,
/// else `name` when it is a usable instance id. `None` (with
/// [`PalaceSource::Unresolved`]) only in that last failure, which is how a
/// non-assistant agent keeps its pre-#7428 "no palace" behaviour.
/// Test: `super::tests::memory_tests::binding_palace_wins_over_config_and_id`,
/// `super::tests::memory_tests::instance_id_is_the_palace_without_a_binding`,
/// `super::tests::memory_tests::an_unusable_name_resolves_no_palace`.
pub fn own_palace(
    binding: Option<&AgentStoreBinding>,
    config: &MemoryConfig,
    name: &str,
) -> (Option<String>, PalaceSource) {
    if let Some(palace) = binding
        .and_then(|b| b.palace.as_deref())
        .and_then(non_blank)
    {
        return (Some(palace), PalaceSource::Binding);
    }
    if let Some(palace) = config.palace.as_deref().and_then(non_blank) {
        return (Some(palace), PalaceSource::Config);
    }
    match AssistantInstanceId::new(name) {
        Ok(id) => (Some(id.to_string()), PalaceSource::InstanceId),
        Err(_) => (None, PalaceSource::Unresolved),
    }
}

/// `Some(trimmed)` for a value carrying text, `None` for blank.
fn non_blank(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Read `[memory]` out of an assistant home's `config.toml`, fail-soft.
///
/// Why: the home is user-editable by design (#4325), so an unreadable or
/// malformed `config.toml` must degrade to "no memory settings", never to a
/// failed chat turn. `super::health::inspect` is the surface that REPORTS the
/// malformed file; this one only has to keep working.
/// What: the parsed `[memory]` table, or the default when the file is absent,
/// unreadable, or does not parse.
/// Test: `super::tests::memory_tests::config_defaults_when_the_table_is_absent`,
/// `super::tests::memory_tests::malformed_config_reads_as_no_settings`.
pub fn read_memory_config(home: &AssistantHome) -> MemoryConfig {
    std::fs::read_to_string(home.config_path())
        .ok()
        .and_then(|raw| toml::from_str::<AssistantHomeConfig>(&raw).ok())
        .map(|config| config.memory)
        .unwrap_or_default()
}

/// Persist `[memory]` into an assistant home's `config.toml`.
///
/// Why: the file is the USER's — it carries their comments and any key they
/// hand-added — so this rewrites the `[memory]` table through `toml_edit` and
/// leaves every other byte alone. Serializing [`AssistantHomeConfig`] back out
/// would silently delete anything the struct does not model, which is the exact
/// external-change intolerance #4325 rules out.
/// What: parses the current document (an absent file starts empty), replaces
/// `[memory]`, and writes it back atomically. `fan_out` is always written, even
/// when empty, so clearing the list is a durable edit rather than a no-op.
/// Test: `super::tests::memory_tests::writing_memory_preserves_other_keys`,
/// `super::tests::memory_tests::writing_an_empty_fan_out_clears_it`.
pub fn write_memory_config(
    home: &AssistantHome,
    memory: &MemoryConfig,
) -> Result<(), AssistantError> {
    let path = home.config_path();
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let mut document = raw
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| AssistantError::Io {
            path: path.clone(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
        })?;
    let mut table = toml_edit::Table::new();
    if let Some(palace) = memory.palace.as_deref().and_then(non_blank) {
        table["palace"] = toml_edit::value(palace);
    }
    let mut ids = toml_edit::Array::new();
    for id in &memory.fan_out {
        ids.push(id.as_str());
    }
    table["fan_out"] = toml_edit::value(ids);
    document["memory"] = toml_edit::Item::Table(table);
    crate::state_writer::atomic_write(&path, document.to_string().as_bytes()).map_err(|e| {
        AssistantError::Io {
            path,
            source: std::io::Error::other(e.to_string()),
        }
    })
}

/// The palace `id` owns, resolved from ITS binding and ITS home config.
///
/// Why: a fan-out entry names an assistant, not a palace, so the reader has to
/// resolve the target the same way the target resolves itself — see
/// [`own_palace`].
/// What: the target's `[[stores]]` binding (parsed partially, so a package
/// `agent.toml` with no `[llm]` table still resolves) and its `[memory]` table,
/// fed through [`own_palace`]. Falls back to the id itself, which is what
/// [`own_palace`] returns for a valid id anyway.
/// Test: `super::tests::memory_tests::fan_out_ids_resolve_through_the_same_rule`.
pub fn palace_for_instance(dirs: &[PathBuf], root: &Path, id: &AssistantInstanceId) -> String {
    let stores = crate::stores::binding::load_stores(dirs, id.as_str()).unwrap_or_default();
    let home = AssistantHome::under(root, id.clone());
    let config = read_memory_config(&home);
    own_palace(stores.primary(), &config, id.as_str())
        .0
        .unwrap_or_else(|| id.to_string())
}

/// The palaces `name` recalls from this turn, against explicit roots.
///
/// Why: the `_in` suffix is this crate's injected-dependency convention — the
/// production wrapper [`resolve_palace_plan`] resolves the real agent dirs and
/// assistants root, and tests pass tempdirs instead of touching `$HOME`.
/// What: own palace by [`own_palace`], then one entry per `fan_out` id that is a
/// usable instance id, is not `name` itself, and does not resolve to a palace
/// already in the plan. Unusable, self-referential and duplicate entries are
/// DROPPED rather than refused: this is a read path and a stale setting must not
/// break a chat turn — `PUT /api/assistants/{id}/memory` is where a bad entry is
/// rejected, at the moment the user writes it.
/// Test: `super::tests::memory_tests::fan_out_ids_resolve_through_the_same_rule`,
/// `super::tests::memory_tests::fan_out_drops_self_duplicates_and_bad_ids`.
pub fn resolve_palace_plan_in(
    dirs: &[PathBuf],
    root: &Path,
    name: &str,
    binding: Option<&AgentStoreBinding>,
) -> PalacePlan {
    let config = match AssistantInstanceId::new(name) {
        Ok(id) => read_memory_config(&AssistantHome::under(root, id)),
        Err(_) => MemoryConfig::default(),
    };
    let (own, source) = own_palace(binding, &config, name);
    let mut fan_out: Vec<String> = Vec::new();
    for raw in &config.fan_out {
        let Ok(id) = AssistantInstanceId::new(raw) else {
            continue;
        };
        if id.as_str() == name {
            continue;
        }
        let palace = palace_for_instance(dirs, root, &id);
        if Some(&palace) == own.as_ref() || fan_out.contains(&palace) {
            continue;
        }
        fan_out.push(palace);
    }
    PalacePlan {
        own,
        source,
        fan_out,
    }
}

/// [`resolve_palace_plan_in`] against the live agent dirs and assistants root.
///
/// Why/What: see [`resolve_palace_plan_in`]. With no resolvable assistants root
/// (no `$HOME`), the binding and the instance id still produce an own palace —
/// only the fan-out list, which lives in the home, is unavailable.
/// Test: covered through `resolve_palace_plan_in`; the wrapper adds only the two
/// live lookups.
pub fn resolve_palace_plan(name: &str, binding: Option<&AgentStoreBinding>) -> PalacePlan {
    let Ok(root) = super::home::assistants_root() else {
        let (own, source) = own_palace(binding, &MemoryConfig::default(), name);
        return PalacePlan {
            own,
            source,
            fan_out: Vec::new(),
        };
    };
    resolve_palace_plan_in(
        &crate::agents::agents_dir_candidates(),
        &root,
        name,
        binding,
    )
}
