//! Tests for one-palace-per-assistant and its opt-in fan-out (#7428).
//!
//! Why: the whole feature is a RESOLUTION RULE plus a settings file, and both
//! halves have a failure mode that is invisible at runtime — a rule applied
//! differently to this assistant than to a fan-out target reaches a palace the
//! target never writes to, and a settings write that re-serializes the struct
//! silently deletes whatever the user hand-added to their own `config.toml`.
//! What: precedence (binding > config > instance id), the fan-out resolution
//! going through the SAME rule, the entries that get dropped, and the
//! comment-and-unknown-key-preserving write.
//! Test: this module IS the test surface.

use std::path::{Path, PathBuf};

use crate::assistants::home::AssistantHome;
use crate::assistants::instance::AssistantInstanceId;
use crate::assistants::memory::{
    MemoryConfig, PalaceSource, own_palace, read_memory_config, resolve_palace_plan_in,
    write_memory_config,
};
use crate::stores::AgentStoreBinding;

fn id(raw: &str) -> AssistantInstanceId {
    AssistantInstanceId::new(raw).expect("fixture id is valid")
}

/// A binding declaring `palace`, or one declaring none.
fn binding(palace: Option<&str>) -> AgentStoreBinding {
    AgentStoreBinding {
        name: "kb".to_string(),
        tree: None,
        index: None,
        palace: palace.map(str::to_string),
        root: None,
    }
}

/// An agents dir holding one package agent with the given `[[stores]]` body.
fn agent_package(dir: &Path, name: &str, stores_toml: &str) {
    let package = dir.join(name);
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("agent.toml"),
        format!("[agent]\nname = \"{name}\"\n{stores_toml}"),
    )
    .unwrap();
}

/// A provisioned home whose `config.toml` carries `body`.
fn home_with_config(root: &Path, name: &str, body: &str) -> AssistantHome {
    let home = AssistantHome::under(root, id(name));
    std::fs::create_dir_all(home.path()).unwrap();
    std::fs::write(home.config_path(), body).unwrap();
    home
}

// ---------------------------------------------------------------------
// own_palace — the one resolution rule
// ---------------------------------------------------------------------

/// Why: the shipped `cto` and `owner-profile` bindings must keep reading the
/// palaces they always did, so the binding outranks both newer sources. This is
/// what makes #7428 a no-migration change.
#[test]
fn binding_palace_wins_over_config_and_id() {
    let config = MemoryConfig {
        palace: Some("config-palace".to_string()),
        fan_out: Vec::new(),
    };
    let (palace, source) = own_palace(Some(&binding(Some("owner-profile"))), &config, "izzie");
    assert_eq!(palace.as_deref(), Some("owner-profile"));
    assert_eq!(source, PalaceSource::Binding);
}

#[test]
fn config_palace_wins_when_the_binding_declares_none() {
    let config = MemoryConfig {
        palace: Some("config-palace".to_string()),
        fan_out: Vec::new(),
    };
    let (palace, source) = own_palace(Some(&binding(None)), &config, "izzie");
    assert_eq!(palace.as_deref(), Some("config-palace"));
    assert_eq!(source, PalaceSource::Config);
}

/// Why: this is the defect #7428 fixes. Before it, a binding with no `palace`
/// meant NO recall at all — an assistant started stateless and stayed that way.
#[test]
fn instance_id_is_the_palace_without_a_binding() {
    let (palace, source) = own_palace(
        Some(&binding(None)),
        &MemoryConfig::default(),
        "cto-assistant",
    );
    assert_eq!(palace.as_deref(), Some("cto-assistant"));
    assert_eq!(source, PalaceSource::InstanceId);

    // A blank declared palace is the same as an absent one, not a blank palace.
    let (palace, source) = own_palace(
        Some(&binding(Some("   "))),
        &MemoryConfig::default(),
        "izzie",
    );
    assert_eq!(palace.as_deref(), Some("izzie"));
    assert_eq!(source, PalaceSource::InstanceId);
}

/// Why: an agent whose name cannot be a directory name is not an assistant
/// instance, so it keeps its pre-#7428 "no palace" behaviour rather than being
/// handed a palace named after an unusable string.
#[test]
fn an_unusable_name_resolves_no_palace() {
    let (palace, source) = own_palace(None, &MemoryConfig::default(), "../escape");
    assert_eq!(palace, None);
    assert_eq!(source, PalaceSource::Unresolved);
}

// ---------------------------------------------------------------------
// config.toml reading and writing
// ---------------------------------------------------------------------

/// Why: every `config.toml` written before `[memory]` existed must still parse
/// — a settings file that suddenly reads as malformed would take the home's
/// health report down with it.
#[test]
fn config_defaults_when_the_table_is_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let home = home_with_config(tmp.path(), "izzie", "id = \"izzie\"\n");
    assert_eq!(read_memory_config(&home), MemoryConfig::default());
}

#[test]
fn malformed_config_reads_as_no_settings() {
    let tmp = tempfile::tempdir().unwrap();
    let home = home_with_config(tmp.path(), "izzie", "id = \"izzie\"\n[memory\n");
    assert_eq!(read_memory_config(&home), MemoryConfig::default());
}

/// Why: the home is the USER's. Re-serializing `AssistantHomeConfig` would drop
/// their comments and any key the struct does not model, which is the
/// external-change intolerance #4325 rules out.
#[test]
fn writing_memory_preserves_other_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let home = home_with_config(
        tmp.path(),
        "izzie",
        "# hand-written note\nid = \"izzie\"\nfavourite_colour = \"teal\"\n",
    );
    write_memory_config(
        &home,
        &MemoryConfig {
            palace: Some("izzie-memory".to_string()),
            fan_out: vec!["cto-assistant".to_string()],
        },
    )
    .unwrap();

    let raw = std::fs::read_to_string(home.config_path()).unwrap();
    assert!(
        raw.contains("# hand-written note"),
        "comment survived: {raw}"
    );
    assert!(
        raw.contains("favourite_colour"),
        "unknown key survived: {raw}"
    );
    let read_back = read_memory_config(&home);
    assert_eq!(read_back.palace.as_deref(), Some("izzie-memory"));
    assert_eq!(read_back.fan_out, vec!["cto-assistant".to_string()]);
}

/// Why: clearing the last fan-out entry has to be a durable edit. Omitting an
/// empty list would leave the previous selection in the file.
#[test]
fn writing_an_empty_fan_out_clears_it() {
    let tmp = tempfile::tempdir().unwrap();
    let home = home_with_config(
        tmp.path(),
        "izzie",
        "id = \"izzie\"\n[memory]\nfan_out = [\"cto-assistant\"]\n",
    );
    write_memory_config(&home, &MemoryConfig::default()).unwrap();
    assert!(read_memory_config(&home).fan_out.is_empty());
}

// ---------------------------------------------------------------------
// resolve_palace_plan_in
// ---------------------------------------------------------------------

/// Why: a fan-out entry names an ASSISTANT, not a palace, so the reader has to
/// resolve the target the way the target resolves itself. Applying a different
/// rule here would read a palace the target never writes to.
#[test]
fn fan_out_ids_resolve_through_the_same_rule() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("agents");
    let root = tmp.path().join("homes");
    // `cto-assistant` pins its palace in the binding; `scout` derives its own.
    agent_package(
        &agents,
        "cto-assistant",
        "[[stores]]\nname = \"cto-kb\"\npalace = \"cto\"\n",
    );
    agent_package(&agents, "scout", "[[stores]]\nname = \"scout-kb\"\n");
    home_with_config(
        &root,
        "izzie",
        "id = \"izzie\"\n[memory]\nfan_out = [\"cto-assistant\", \"scout\"]\n",
    );

    let plan = resolve_palace_plan_in(
        &[agents],
        &root,
        "izzie",
        Some(&binding(Some("owner-profile"))),
    );
    assert_eq!(plan.own.as_deref(), Some("owner-profile"));
    assert!(plan.own_is_bound());
    assert_eq!(
        plan.fan_out,
        vec!["cto".to_string(), "scout".to_string()],
        "each target resolved by its own binding, else its instance id"
    );
}

/// Why: a stale or self-referential setting must not break a chat turn — the
/// PUT route is where a bad entry is refused, at the moment the user writes it.
#[test]
fn fan_out_drops_self_duplicates_and_bad_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let agents: Vec<PathBuf> = vec![tmp.path().join("agents")];
    let root = tmp.path().join("homes");
    std::fs::create_dir_all(&agents[0]).unwrap();
    home_with_config(
        &root,
        "izzie",
        "id = \"izzie\"\n[memory]\nfan_out = [\"izzie\", \"scout\", \"scout\", \"../escape\"]\n",
    );

    let plan = resolve_palace_plan_in(&agents, &root, "izzie", Some(&binding(None)));
    assert_eq!(plan.own.as_deref(), Some("izzie"));
    assert_eq!(
        plan.fan_out,
        vec!["scout".to_string()],
        "self, the repeat and the unusable id are all dropped"
    );
}

/// Why: the default has to be exactly one palace and no fan-out — an assistant
/// nobody configured must never see another assistant's memory.
#[test]
fn an_unconfigured_assistant_has_one_palace_and_no_fan_out() {
    let tmp = tempfile::tempdir().unwrap();
    let plan = resolve_palace_plan_in(
        &[tmp.path().join("agents")],
        &tmp.path().join("homes"),
        "izzie",
        Some(&binding(None)),
    );
    assert_eq!(plan.own.as_deref(), Some("izzie"));
    assert_eq!(plan.source, PalaceSource::InstanceId);
    assert!(plan.fan_out.is_empty());
    assert!(
        !plan.own_is_bound(),
        "a derived palace is created on first use"
    );
}
