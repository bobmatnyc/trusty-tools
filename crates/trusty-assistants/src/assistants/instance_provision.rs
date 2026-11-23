//! Provisioning one named assistant instance for a project (#11649).
//!
//! Why: startup provisioning ([`super::provision`]) only fills in the layout of
//! instances that already exist. Setting up a new instance — personal, CTO —
//! also needs its role instructions, its project directory, its fan-out peers,
//! and an `agent.toml` that the roster discovers and whose first `[[stores]]`
//! binding is the instance's own OKG, index and palace.
//! What: [`provision_instance`] (resolves the roots from the environment) and
//! [`provision_instance_in`] (explicit roots). Both validate everything before
//! writing anything, then create only what is absent. They never overwrite a
//! file: an existing `config.toml` gains a missing `project_dir` or fan-out
//! peer and keeps every value it has; an existing `agent.toml` or
//! `instructions.md` is left alone. A second run changes nothing.
//! Test: `super::tests::instance_provision_tests` — the whole module.

use std::path::{Path, PathBuf};

use super::error::AssistantError;
use super::home::{AssistantHome, app_dir, assistants_root};
use super::instance::{ASSISTANT_ROLE, AssistantInstanceId};
use super::instructions::{AssistantRole, seed_role_instructions, write_new};
use super::project::{
    PROJECT_DIR_KEY, canonical_project_dir, dir_str, ensure_unclaimed, parse_document,
    read_home_config, write_config,
};
use super::roster::ASSISTANT_BASE;

/// What one [`provision_instance_in`] call did.
///
/// Test: `super::tests::instance_provision_tests::a_second_run_changes_nothing`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionedInstance {
    /// The instance's home.
    pub home: AssistantHome,
    /// The canonical project directory the instance serves.
    pub project_dir: PathBuf,
    /// The instance's `agent.toml`.
    pub agent_toml: PathBuf,
    /// Files and directories this call created.
    pub created: Vec<PathBuf>,
    /// Whether this call added a key to an existing `config.toml`.
    pub config_updated: bool,
}

impl ProvisionedInstance {
    /// Whether this call changed nothing on disk.
    /// Test: `super::tests::instance_provision_tests::a_second_run_changes_nothing`.
    pub fn is_unchanged(&self) -> bool {
        self.created.is_empty() && !self.config_updated
    }
}

/// The instance's own trusty-search index id: `<id>-okg`.
/// Test: `super::tests::instance_provision_tests::each_instance_searches_its_own_index_by_default`.
pub fn own_index_id(id: &AssistantInstanceId) -> String {
    format!("{id}-okg")
}

/// Provision `id` under the environment's roots.
///
/// What: the assistants root is [`assistants_root`]; `agent.toml` goes under
/// `$HOME/.trusty-agents/agents`, the home tier the roster searches.
/// [`AssistantError::NoUserHome`] when `$HOME` is unset. See
/// [`provision_instance_in`] for the rest.
/// Test: `super::tests::instance_provision_tests::provision_instance_uses_the_environment_roots`.
pub fn provision_instance(
    role: AssistantRole,
    id: &str,
    project_dir: &Path,
    fan_out: &[AssistantInstanceId],
) -> Result<ProvisionedInstance, AssistantError> {
    let id = AssistantInstanceId::new(id)?;
    let root = assistants_root()?;
    let user_home =
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .ok_or(AssistantError::NoUserHome {
                env: super::home::ASSISTANTS_DIR_ENV,
            })?;
    let agents_dir = app_dir(Path::new(&user_home)).join("agents");
    provision_instance_in(&root, &agents_dir, role, &id, project_dir, fan_out)
}

/// Provision `id` under explicit roots.
///
/// Why/What: see this module's doc comment. Checks run first, so a refused
/// call writes nothing: the project directory must be valid
/// ([`canonical_project_dir`]), unclaimed by another home
/// ([`AssistantError::ProjectDirConflict`]), and not contradict a
/// `project_dir` the home already names
/// ([`AssistantError::ProjectDirMismatch`]). Then: the role's
/// `instructions.md`, `config.toml` (`id`, `project_dir`, `[memory] fan_out`),
/// the rest of the layout through [`AssistantHome::ensure`], and
/// `<agents_dir>/<id>/agent.toml`.
/// Test: `super::tests::instance_provision_tests::provisions_a_complete_instance`,
/// `super::tests::instance_provision_tests::a_second_run_changes_nothing`,
/// `super::tests::instance_provision_tests::existing_files_are_never_overwritten`,
/// `super::tests::instance_provision_tests::a_conflicting_project_dir_writes_nothing`,
/// `super::tests::instance_provision_tests::a_different_existing_project_dir_is_refused`.
pub fn provision_instance_in(
    root: &Path,
    agents_dir: &Path,
    role: AssistantRole,
    id: &AssistantInstanceId,
    project_dir: &Path,
    fan_out: &[AssistantInstanceId],
) -> Result<ProvisionedInstance, AssistantError> {
    let dir = canonical_project_dir(project_dir)?;
    let home = AssistantHome::under(root, id.clone());
    ensure_unclaimed(root, id, &dir)?;
    if let Some(existing) = read_home_config(&home)?.and_then(|c| c.project_dir)
        && canonical_project_dir(&existing).ok().as_deref() != Some(dir.as_path())
    {
        return Err(AssistantError::ProjectDirMismatch {
            id: id.to_string(),
            existing,
            requested: dir,
        });
    }

    let mut created = Vec::new();
    if !home.exists() {
        created.push(home.path().to_path_buf());
    }
    if seed_role_instructions(&home, role)? {
        created.push(home.instructions_path());
    }
    let config_updated = write_instance_config(&home, &dir, fan_out, &mut created)?;
    for path in home.ensure()?.paths {
        if !created.contains(&path) {
            created.push(path);
        }
    }
    let agent_toml = agents_dir.join(id.as_str()).join("agent.toml");
    if !agent_toml.exists() {
        let parent = agents_dir.join(id.as_str());
        std::fs::create_dir_all(&parent).map_err(|source| AssistantError::Io {
            path: parent,
            source,
        })?;
        if write_new(&agent_toml, agent_toml_body(role, id).as_bytes())? {
            created.push(agent_toml.clone());
        }
    }
    Ok(ProvisionedInstance {
        home,
        project_dir: dir,
        agent_toml,
        created,
        config_updated,
    })
}

/// Seed `config.toml`, or add the missing `project_dir` / fan-out peers to an
/// existing one. Returns whether an existing file was changed.
fn write_instance_config(
    home: &AssistantHome,
    dir: &Path,
    fan_out: &[AssistantInstanceId],
    created: &mut Vec<PathBuf>,
) -> Result<bool, AssistantError> {
    let path = home.config_path();
    let existed = path.exists();
    let raw = if existed {
        std::fs::read_to_string(&path).map_err(|source| AssistantError::Io {
            path: path.clone(),
            source,
        })?
    } else {
        home.seed_config_body()
    };
    let mut doc = parse_document(&path, &raw)?;
    let mut changed = false;
    if !doc.contains_key(PROJECT_DIR_KEY) {
        doc[PROJECT_DIR_KEY] = toml_edit::value(dir_str(dir)?);
        changed = true;
    }
    if !doc.contains_key("memory") {
        doc["memory"] = toml_edit::table();
    }
    let memory = doc["memory"]
        .as_table_mut()
        .ok_or_else(|| AssistantError::InvalidConfig {
            path: path.clone(),
            reason: "`memory` is not a table".into(),
        })?;
    if !memory.contains_key("fan_out") {
        memory["fan_out"] = toml_edit::value(toml_edit::Array::new());
    }
    let peers = memory["fan_out"]
        .as_array_mut()
        .ok_or_else(|| AssistantError::InvalidConfig {
            path: path.clone(),
            reason: "`memory.fan_out` is not an array".into(),
        })?;
    for peer in fan_out.iter().filter(|p| *p != home.id()) {
        if !peers.iter().any(|v| v.as_str() == Some(peer.as_str())) {
            peers.push(peer.as_str());
            changed = true;
        }
    }
    if !existed {
        write_new(&path, doc.to_string().as_bytes())?;
        created.push(path);
        return Ok(false);
    }
    if changed {
        write_config(&path, &doc)?;
    }
    Ok(changed)
}

/// The `agent.toml` for a new instance: an `extends = "assistant"` overlay
/// whose first `[[stores]]` binding is the instance's own OKG and index.
fn agent_toml_body(role: AssistantRole, id: &AssistantInstanceId) -> String {
    let index = own_index_id(id);
    format!(
        "# Generated by trusty-assistants (#11649) for the `{id}` instance.\n\
         # Safe to edit by hand; it is never rewritten.\n\
         [agent]\n\
         name = \"{id}\"\n\
         role = \"{ASSISTANT_ROLE}\"\n\
         extends = \"{ASSISTANT_BASE}\"\n\
         description = \"{role} assistant instance `{id}`\"\n\
         \n\
         # The first binding is this instance's primary OKG and search index.\n\
         [[stores]]\n\
         name = \"{id}\"\n\
         tree = \"okg://{id}\"\n\
         index = \"{index}\"\n\
         palace = \"{id}\"\n\
         root = \"okg\"\n",
        role = role.as_str(),
    )
}
