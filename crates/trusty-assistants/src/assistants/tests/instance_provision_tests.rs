//! Provisioning the personal and CTO instances (#11649).
//!
//! Why: provisioning writes into user-owned trees, so idempotence and
//! never-overwrite are pinned, and a refused call must write nothing. The two
//! instances must each search their own index by default while the other's
//! stays queryable.
//! What: a complete instance, a second run, pre-existing files, the two refusal
//! arms, the primary-index pair, and the environment-rooted wrapper.
//! Test: this module IS the test surface.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;
use serial_test::serial;

use super::home_tests::EnvVarGuard;
use crate::agents::AgentConfig;
use crate::assistants::{
    ASSISTANTS_DIR_ENV, AssistantError, AssistantHome, AssistantHomeConfig, AssistantInstanceId,
    AssistantRole, discover_instances, inspect, instance_for_project, own_index_id,
    provision_instance, provision_instance_in,
};

/// Canonical temp layout: `homes/` (assistants root), `agents/`, two projects.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    agents: PathBuf,
    personal_dir: PathBuf,
    cto_dir: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let make = |name: &str| {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    };
    Fixture {
        root: base.join("homes"),
        agents: base.join("agents"),
        personal_dir: make("personal-project"),
        cto_dir: make("cto-project"),
        _tmp: tmp,
    }
}

fn id(raw: &str) -> AssistantInstanceId {
    AssistantInstanceId::new(raw).unwrap()
}

/// Provision both instances, each fanning out to the other.
fn provision_pair(f: &Fixture) {
    provision_instance_in(
        &f.root,
        &f.agents,
        AssistantRole::Personal,
        &id("personal"),
        &f.personal_dir,
        &[id("cto-assistant")],
    )
    .unwrap();
    provision_instance_in(
        &f.root,
        &f.agents,
        AssistantRole::Cto,
        &id("cto-assistant"),
        &f.cto_dir,
        &[id("personal")],
    )
    .unwrap();
}

/// Every file under `dirs`, path to bytes.
fn snapshot(dirs: &[&Path]) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack: Vec<PathBuf> = dirs.iter().map(|d| d.to_path_buf()).collect();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.insert(path.clone(), std::fs::read(&path).unwrap());
            }
        }
    }
    out
}

fn config(home: &AssistantHome) -> AssistantHomeConfig {
    toml::from_str(&std::fs::read_to_string(home.config_path()).unwrap()).unwrap()
}

#[test]
fn provisions_a_complete_instance() {
    let f = fixture();
    let got = provision_instance_in(
        &f.root,
        &f.agents,
        AssistantRole::Personal,
        &id("personal"),
        &f.personal_dir,
        &[id("cto-assistant")],
    )
    .unwrap();

    let home = AssistantHome::under(&f.root, id("personal"));
    assert_eq!(got.home, home);
    assert!(inspect(&home).is_healthy(), "{:?}", inspect(&home).issues);
    assert_eq!(
        home.read_instructions().unwrap(),
        AssistantRole::Personal.template()
    );
    let cfg = config(&home);
    assert_eq!(cfg.id, "personal");
    assert_eq!(cfg.project_dir.as_deref(), Some(f.personal_dir.as_path()));
    assert_eq!(cfg.memory.fan_out, vec!["cto-assistant".to_string()]);
    assert_eq!(got.agent_toml, f.agents.join("personal/agent.toml"));
    assert!(got.created.contains(&got.agent_toml));
    assert!(got.created.contains(&f.agents.join("personal/persona.md")));
    assert_eq!(
        discover_instances(std::slice::from_ref(&f.agents)),
        vec![id("personal")]
    );
    assert_eq!(
        instance_for_project(&f.root, &f.personal_dir).unwrap(),
        id("personal")
    );
}

#[test]
fn a_second_run_changes_nothing() {
    let f = fixture();
    provision_pair(&f);
    let before = snapshot(&[&f.root, &f.agents]);

    let again = provision_instance_in(
        &f.root,
        &f.agents,
        AssistantRole::Personal,
        &id("personal"),
        &f.personal_dir,
        &[id("cto-assistant")],
    )
    .unwrap();

    assert!(again.is_unchanged(), "{again:?}");
    assert_eq!(snapshot(&[&f.root, &f.agents]), before);
}

#[test]
fn existing_files_are_never_overwritten() {
    let f = fixture();
    let home = AssistantHome::under(&f.root, id("cto-assistant"));
    std::fs::create_dir_all(home.path()).unwrap();
    std::fs::write(home.instructions_path(), "keep me\n").unwrap();
    std::fs::write(
        home.config_path(),
        "id = \"cto-assistant\"\ndisplay_name = \"CTO\"\n",
    )
    .unwrap();
    let agent = f.agents.join("cto-assistant/agent.toml");
    std::fs::create_dir_all(agent.parent().unwrap()).unwrap();
    std::fs::write(&agent, "# hand-written\n").unwrap();

    let got = provision_instance_in(
        &f.root,
        &f.agents,
        AssistantRole::Cto,
        &id("cto-assistant"),
        &f.cto_dir,
        &[id("personal")],
    )
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(home.instructions_path()).unwrap(),
        "keep me\n"
    );
    assert_eq!(std::fs::read_to_string(&agent).unwrap(), "# hand-written\n");
    assert!(!got.created.contains(&agent));
    assert!(got.config_updated);
    let cfg = config(&home);
    assert_eq!(cfg.display_name.as_deref(), Some("CTO"));
    assert_eq!(cfg.project_dir.as_deref(), Some(f.cto_dir.as_path()));
    assert_eq!(cfg.memory.fan_out, vec!["personal".to_string()]);
}

#[test]
fn a_conflicting_project_dir_writes_nothing() {
    let f = fixture();
    provision_pair(&f);

    let err = provision_instance_in(
        &f.root,
        &f.agents,
        AssistantRole::Personal,
        &id("izzie"),
        &f.cto_dir,
        &[],
    )
    .unwrap_err();

    assert!(
        matches!(&err, AssistantError::ProjectDirConflict { first, second, .. }
            if first == "cto-assistant" && second == "izzie"),
        "{err}"
    );
    assert!(!f.root.join("izzie").exists());
    assert!(!f.agents.join("izzie").exists());
}

#[test]
fn a_different_existing_project_dir_is_refused() {
    let f = fixture();
    provision_pair(&f);
    let home = AssistantHome::under(&f.root, id("personal"));
    let before = std::fs::read(home.config_path()).unwrap();
    let other = f.root.parent().unwrap().join("other");
    std::fs::create_dir_all(&other).unwrap();

    let err = provision_instance_in(
        &f.root,
        &f.agents,
        AssistantRole::Personal,
        &id("personal"),
        &other,
        &[],
    )
    .unwrap_err();

    assert!(
        matches!(&err, AssistantError::ProjectDirMismatch { existing, .. }
            if existing == &f.personal_dir),
        "{err}"
    );
    assert_eq!(std::fs::read(home.config_path()).unwrap(), before);
}

/// Point the assistants root at `dir` under `ENV_LOCK`, the same guard the
/// search-binding tests use for this variable.
struct AssistantsRoot {
    _env: EnvVarGuard,
    _lock: std::sync::MutexGuard<'static, ()>,
}

fn assistants_root_at(dir: &Path) -> AssistantsRoot {
    let lock = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    AssistantsRoot {
        _env: EnvVarGuard::set(ASSISTANTS_DIR_ENV, dir),
        _lock: lock,
    }
}

/// Requirement 5: the primary index is the instance's own; an attached
/// foreign index stays queryable under enforcement.
#[test]
#[serial] // sets `TA_ASSISTANTS_DIR`; shares the key of its other mutators
fn each_instance_searches_its_own_index_by_default() {
    let f = fixture();
    provision_pair(&f);
    let _root = assistants_root_at(&f.root);
    let dirs = [f.agents.clone(), crate::test_env::bundled_agents_dir()];

    for (me, other) in [("personal", "cto-assistant"), ("cto-assistant", "personal")] {
        let mut cfg = AgentConfig::by_name_in(&dirs, me).unwrap();
        let own = own_index_id(&id(me));
        let foreign = own_index_id(&id(other));
        assert_eq!(cfg.stores.default_search_index(), Some(own.as_str()));
        assert_eq!(
            cfg.stores.primary().and_then(|b| b.palace.as_deref()),
            Some(me)
        );

        cfg.tools.enforce_search_indexes = Some(true);
        let tool = crate::knowledge::search_binding::tool(me, &cfg, vec![foreign.clone()]);
        assert_eq!(tool.authorized_index_id(&json!({})).unwrap(), Some(own));
        assert_eq!(
            tool.authorized_index_id(&json!({ "index_id": foreign }))
                .unwrap(),
            Some(foreign)
        );
        assert!(
            tool.authorized_index_id(&json!({ "index_id": "unattached" }))
                .is_err()
        );
    }
}

#[test]
#[serial]
fn provision_instance_uses_the_environment_roots() {
    let _home_lock = crate::test_env::HOME_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let f = fixture();
    let user_home = f.root.parent().unwrap().join("user-home");
    std::fs::create_dir_all(&user_home).unwrap();
    let _home = EnvVarGuard::set("HOME", &user_home);
    let _root = EnvVarGuard::set(ASSISTANTS_DIR_ENV, &f.root);

    let got = provision_instance(AssistantRole::Cto, "cto-assistant", &f.cto_dir, &[]).unwrap();

    assert_eq!(got.home.path(), f.root.join("cto-assistant"));
    assert_eq!(
        got.agent_toml,
        user_home.join(".trusty-agents/agents/cto-assistant/agent.toml")
    );
    assert!(got.agent_toml.is_file());
    let err = provision_instance(AssistantRole::Cto, "../x", &f.cto_dir, &[]).unwrap_err();
    assert!(
        matches!(err, AssistantError::InvalidInstanceId { .. }),
        "{err}"
    );
}
