//! Assistant-owned durable memory binding and revisioned read policy (#7360).
use super::{AssistantHome, AssistantInstanceId};
use crate::knowledge::{KnowledgeError, Result, persistence};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryPolicy {
    pub assistant_id: String,
    pub namespace: String,
    pub revision: String,
    #[serde(default)]
    pub cross_palace_query: bool,
}
fn derived_namespace(name: &str) -> String {
    format!("assistant-{:x}", Sha256::digest(name.as_bytes()))[..58].into()
}

/// Why: identity cannot come from tool arguments or the active project.
/// What: preserve an exclusive legacy binding, otherwise derive from instance ID.
/// Test: `policy_persists_legacy_binding_and_rejects_stale_revision`.
pub fn resolve(name: &str) -> Result<MemoryPolicy> {
    let dirs = crate::agents::agents_dir_candidates();
    let root = super::assistants_root().map_err(|e| KnowledgeError::Unavailable(e.to_string()))?;
    resolve_at(&dirs, root, name, None)
}

/// Why: cross-palace reads require a persistent choice without permitting namespace reassignment.
/// What: compare the supplied revision under the binding lock and return the updated immutable-identity policy.
/// Test: `policy_persists_legacy_binding_and_rejects_stale_revision`.
pub fn patch(name: &str, revision: &str, cross: bool) -> Result<MemoryPolicy> {
    let dirs = crate::agents::agents_dir_candidates();
    let root = super::assistants_root().map_err(|e| KnowledgeError::Unavailable(e.to_string()))?;
    resolve_at(&dirs, root, name, Some((revision, cross)))
}

fn resolve_at(
    dirs: &[PathBuf],
    root: PathBuf,
    name: &str,
    patch: Option<(&str, bool)>,
) -> Result<MemoryPolicy> {
    let id =
        AssistantInstanceId::new(name).map_err(|e| KnowledgeError::BadRequest(e.to_string()))?;
    let instances = super::discover_instances(dirs);
    if id.as_str() != name || !instances.contains(&id) {
        return Err(KnowledgeError::BadRequest(
            "Memory belongs to a discovered assistant instance".into(),
        ));
    }
    let config = crate::agents::AgentConfig::by_name_in(dirs, name)
        .map_err(|e| KnowledgeError::Unavailable(e.to_string()))?;
    let palaces: Vec<_> = config
        .stores
        .bindings
        .iter()
        .filter_map(|b| b.palace.as_deref())
        .collect();
    if palaces.len() > 1
        || palaces
            .iter()
            .any(|p| p.is_empty() || p.contains(['/', '\\', '*']))
    {
        return Err(KnowledgeError::InvalidState(
            "Ambiguous memory binding; ask Concierge to repair it".into(),
        ));
    }
    let home = AssistantHome::under(root.clone(), id);
    let directory = home.path().join("stores/memory-policy");
    persistence::private_dir(&directory)?;
    let _guard = persistence::lock(&root.join("memory-bindings.lock"))?;
    let path = directory.join("state.json");
    let old: Option<MemoryPolicy> = persistence::read(&path)?;
    let namespace = old
        .as_ref()
        .map(|p| p.namespace.clone())
        .unwrap_or_else(|| {
            palaces
                .first()
                .map(|p| (*p).to_owned())
                .unwrap_or_else(|| derived_namespace(name))
        });
    for other in instances.iter().filter(|id| id.as_str() != name) {
        let other_home = AssistantHome::under(root.clone(), other.clone());
        let stored: Option<MemoryPolicy> =
            persistence::read(&other_home.path().join("stores/memory-policy/state.json"))?;
        let other_config = crate::agents::AgentConfig::by_name_in(dirs, other.as_str())
            .map_err(|e| KnowledgeError::Unavailable(e.to_string()))?;
        if stored.as_ref().is_some_and(|p| p.namespace == namespace)
            || other_config
                .stores
                .bindings
                .iter()
                .any(|b| b.palace.as_deref() == Some(&namespace))
            || (other_config
                .stores
                .bindings
                .iter()
                .all(|b| b.palace.is_none())
                && namespace == derived_namespace(other.as_str()))
        {
            return Err(KnowledgeError::InvalidState(format!(
                "Memory namespace {namespace} is also bound to {other}; repair ownership before using memory"
            )));
        }
    }
    let mut policy = old.clone().unwrap_or(MemoryPolicy {
        assistant_id: name.into(),
        namespace,
        revision: String::new(),
        cross_palace_query: false,
    });
    if policy.assistant_id != name
        || policy.namespace.is_empty()
        || policy.namespace.contains(['/', '\\', '*'])
        || palaces.first().is_some_and(|p| *p != policy.namespace)
    {
        return Err(KnowledgeError::InvalidState(
            "Memory identity does not match this assistant".into(),
        ));
    }
    if let Some((revision, cross)) = patch {
        if revision != policy.revision {
            return Err(KnowledgeError::Conflict);
        }
        policy.cross_palace_query = cross;
    }
    if old.is_none() || patch.is_some() {
        policy.revision = format!(
            "{:x}",
            Sha256::digest(format!(
                "{}:{}:{}:{}",
                policy.revision, name, policy.namespace, policy.cross_palace_query
            ))
        );
        let data = serde_json::to_vec_pretty(&policy)
            .map_err(|e| KnowledgeError::InvalidState(e.to_string()))?;
        persistence::write_bytes(&path, &data)?;
    }
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Vec<PathBuf>, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents");
        std::fs::create_dir(&dir).unwrap();
        let base = "[agent]\nname='assistant'\nrole='assistant'\nmodel='fixture'\ndescription='fixture'\n[llm]\nmax_tokens=128\ntemperature=0.0\n[system_prompt]\ncontent='fixture'\n";
        std::fs::write(dir.join("assistant.toml"), base).unwrap();
        for name in ["one", "two"] {
            std::fs::write(
                dir.join(format!("{name}.toml")),
                base.replace(
                    "name='assistant'",
                    &format!("name='{name}'\nextends='assistant'"),
                ),
            )
            .unwrap();
        }
        let root = tmp.path().canonicalize().unwrap().join("homes");
        (tmp, vec![dir], root)
    }
    #[test]
    fn policy_persists_legacy_binding_and_rejects_stale_revision() {
        let (_tmp, dirs, root) = fixture();
        let path = dirs[0].join("one.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            format!("{raw}\n[[stores]]\nname='one'\npalace='cto'\n"),
        )
        .unwrap();
        let initial = resolve_at(&dirs, root.clone(), "one", None).unwrap();
        assert_eq!(initial.namespace, "cto");
        assert!(!initial.cross_palace_query);
        let changed =
            resolve_at(&dirs, root.clone(), "one", Some((&initial.revision, true))).unwrap();
        assert_eq!(
            resolve_at(&dirs, root.clone(), "one", None)
                .unwrap()
                .revision,
            changed.revision
        );
        assert!(matches!(
            resolve_at(&dirs, root.clone(), "one", Some((&initial.revision, false))),
            Err(KnowledgeError::Conflict)
        ));
        let other = resolve_at(&dirs, root.clone(), "two", None).unwrap();
        assert_ne!(initial.namespace, other.namespace);
        assert!(!other.cross_palace_query);
        let path = dirs[0].join("two.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            path,
            format!("{raw}\n[[stores]]\nname='two'\npalace='cto'\n"),
        )
        .unwrap();
        assert!(resolve_at(&dirs, root, "one", None).is_err());
    }
}
