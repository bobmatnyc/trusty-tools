//! Resolving `okg://<agent>` to a filesystem tree, and a tree back to its
//! bound search index (#3892).
//!
//! Why: #3892's third finding was that `okg://<agent>` is an OPAQUE LABEL —
//! nothing in the repo ever stripped the scheme or joined it onto a directory,
//! so the fact that a binding's tree tail and the `Roots`-derived KB directory
//! name coincided was a naming coincidence, not an enforced invariant. Feeding
//! ingestion into the bound index (remediation (a)) is only sound if that
//! coincidence becomes a CHECKED correspondence: pushing a tree's entities into
//! an index bound to some OTHER tree would trade a silent-empty failure for a
//! silent-wrong one. This module is the resolution the binding always implied.
//!
//! What: [`okg_tree_path`] is the one canonical `okg://X` →
//! `<knowledge_dir>/<slug(X)>` mapping, matching `trusty_kb::roots::Roots`'
//! agent mapping exactly. [`bound_index_for_tree`] answers "which index does
//! this KB tree feed?" — from a named agent when the caller has one, or by
//! searching the agent roster for the single agent whose binding claims that
//! tree. Every failure returns a human-readable reason instead of an error
//! type, because the caller's job is to REPORT the reason, never to fail the
//! ingest over it.
//!
//! Test: the `mod tests` at the bottom of this file.

use std::path::{Path, PathBuf};

use trusty_kb::entity::slugify;

use super::config::{OKG_SCHEME, StoresConfig};

/// The agent + index a KB tree feeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundIndex {
    /// The agent whose `agent.toml` carries the binding.
    pub agent: String,
    /// The store binding's name.
    pub store: String,
    /// The trusty-search index id to push into.
    pub index: String,
}

/// Resolve an `okg://<agent>` tree URI to its directory under `knowledge_dir`.
///
/// Why: see the module doc — this is the mapping the URI always implied and
/// nothing ever performed. Slugging matches `Roots::resolve`'s agent mapping so
/// a tree addressed by an OKG tool and a tree named by a `[[stores]]` binding
/// resolve to the SAME directory or the mismatch is visible.
/// What: `Some(path)` for a well-formed `okg://` URI with a non-blank tail;
/// `None` for anything else (a `https://` tree, a blank tail) — those are
/// already reported by [`super::config::AgentStoreBinding::validate`].
/// Test: `okg_uri_resolves_under_the_knowledge_dir`,
/// `non_okg_uri_does_not_resolve`.
pub fn okg_tree_path(knowledge_dir: &Path, tree_uri: &str) -> Option<PathBuf> {
    let tail = tree_uri.strip_prefix(OKG_SCHEME)?.trim();
    if tail.is_empty() {
        return None;
    }
    let slug = slugify(tail);
    if slug.is_empty() {
        return None;
    }
    Some(knowledge_dir.join(slug))
}

/// Two paths name the same directory, tolerating one or both not existing yet.
///
/// Why: a first ingest creates the tree, so the comparison must work BEFORE the
/// directory exists — canonicalising unconditionally would report "different"
/// for every first run. What: canonicalises when possible (so a symlinked or
/// `/tmp` vs `/private/tmp` spelling still matches) and falls back to a literal
/// comparison otherwise.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Which search index, if any, the KB tree at `root` feeds.
///
/// Why: the index push must target the index bound to THIS tree. Passing the
/// agent name explicitly is the common case (every OKG tool takes one); when it
/// is absent the roster is searched instead, and an ambiguous answer is refused
/// rather than guessed — pushing a tree's contents into the wrong agent's index
/// is worse than not pushing at all.
/// What: `Ok(BoundIndex)` when exactly one agent binds this tree and its
/// binding validates; `Err(reason)` otherwise, phrased for a tool result.
/// `agent_hint`'s binding must actually claim `root`, or the mismatch is the
/// reason — that check is the whole point of this module.
/// Test: `resolves_index_from_the_named_agent`,
/// `reports_a_binding_that_names_a_different_tree`,
/// `finds_the_only_agent_binding_a_tree`, `refuses_an_ambiguous_tree`,
/// `reports_an_agent_with_no_binding`.
pub fn bound_index_for_tree(
    agent_dirs: &[PathBuf],
    knowledge_dir: &Path,
    root: &Path,
    agent_hint: Option<&str>,
) -> Result<BoundIndex, String> {
    if let Some(agent) = agent_hint {
        let Some(stores) = load_stores(agent_dirs, agent) else {
            return Err(format!(
                "agent `{agent}` was not found in {}",
                describe_dirs(agent_dirs)
            ));
        };
        return binding_for(knowledge_dir, root, agent, &stores);
    }

    let mut matches: Vec<BoundIndex> = Vec::new();
    for agent in roster(agent_dirs) {
        let Some(stores) = load_stores(agent_dirs, &agent) else {
            continue;
        };
        if let Ok(found) = binding_for(knowledge_dir, root, &agent, &stores) {
            matches.push(found);
        }
    }
    match matches.len() {
        0 => Err(format!(
            "no agent binds the tree at {} — add a `[[stores]]` entry naming \
             `{OKG_SCHEME}<agent>` and its search index, or pass `agent`",
            root.display()
        )),
        1 => Ok(matches.remove(0)),
        _ => Err(format!(
            "{} agents bind the tree at {} ({}); pass `agent` to disambiguate",
            matches.len(),
            root.display(),
            matches
                .iter()
                .map(|m| m.agent.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The primary binding of `agent`, verified to claim the tree at `root`.
fn binding_for(
    knowledge_dir: &Path,
    root: &Path,
    agent: &str,
    stores: &StoresConfig,
) -> Result<BoundIndex, String> {
    let Some(binding) = stores.primary() else {
        return Err(format!(
            "agent `{agent}` binds no OKG store, so there is no index to feed"
        ));
    };
    if let Some(issue) = binding.validate() {
        return Err(issue);
    }
    let tree_uri = binding.resolved_tree(agent);
    let Some(tree_path) = okg_tree_path(knowledge_dir, &tree_uri) else {
        return Err(format!(
            "store `{}` declares tree `{tree_uri}`, which does not resolve to a \
             knowledge-tree directory",
            binding.name
        ));
    };
    if !same_dir(&tree_path, root) {
        return Err(format!(
            "agent `{agent}` binds tree `{tree_uri}` ({}), but this ingest wrote to {} \
             — refusing to feed an index that describes a different tree",
            tree_path.display(),
            root.display()
        ));
    }
    Ok(BoundIndex {
        agent: agent.to_string(),
        store: binding.name.clone(),
        index: binding.resolved_index().to_string(),
    })
}

/// Parse just the `[[stores]]` table out of an agent's config.
///
/// Why: reading the whole file through `AgentConfig` would require `[llm]` /
/// `[system_prompt]` to be present and valid, which a directory-package
/// `agent.toml` deliberately omits — the same partial-read precedent as
/// `api::server::agent_stores::parse_stores`. A malformed file degrades to "no
/// bindings", which the caller reports as a reason.
fn load_stores(dirs: &[PathBuf], agent: &str) -> Option<StoresConfig> {
    if agent.is_empty() || agent.contains(['/', '\\']) || agent == "." || agent == ".." {
        return None;
    }
    let path = dirs.iter().find_map(|dir| {
        let package = dir.join(agent).join("agent.toml");
        if package.is_file() {
            return Some(package);
        }
        let flat = dir.join(format!("{agent}.toml"));
        flat.is_file().then_some(flat)
    })?;
    let raw = std::fs::read_to_string(path).ok()?;
    #[derive(serde::Deserialize)]
    struct Partial {
        #[serde(default)]
        stores: StoresConfig,
    }
    Some(
        toml::from_str::<Partial>(&raw)
            .map(|p| p.stores)
            .unwrap_or_default(),
    )
}

/// Every agent name discoverable in `dirs`, package or flat form.
fn roster(dirs: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = if path.is_dir() && path.join("agent.toml").is_file() {
                path.file_name().map(|n| n.to_string_lossy().to_string())
            } else if path.extension().is_some_and(|e| e == "toml") {
                path.file_stem().map(|n| n.to_string_lossy().to_string())
            } else {
                None
            };
            if let Some(name) = name
                && !names.contains(&name)
            {
                names.push(name);
            }
        }
    }
    names.sort();
    names
}

/// Render the searched directories for an error message.
fn describe_dirs(dirs: &[PathBuf]) -> String {
    if dirs.is_empty() {
        return "(no agent directories configured)".to_string();
    }
    dirs.iter()
        .map(|d| d.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An agents dir holding one package agent with the given `[[stores]]`.
    fn agent_dir(tmp: &Path, name: &str, stores_toml: &str) {
        let dir = tmp.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            format!("[agent]\nname = \"{name}\"\n{stores_toml}"),
        )
        .unwrap();
    }

    #[test]
    fn okg_uri_resolves_under_the_knowledge_dir() {
        let kdir = Path::new("/kb/knowledge");
        assert_eq!(
            okg_tree_path(kdir, "okg://cto-assistant"),
            Some(PathBuf::from("/kb/knowledge/cto-assistant"))
        );
        // Slugged exactly like `Roots::resolve`'s agent mapping, so a name with
        // a space cannot inject a path separator.
        assert_eq!(
            okg_tree_path(kdir, "okg://CTO Bot"),
            Some(PathBuf::from("/kb/knowledge/cto-bot"))
        );
    }

    #[test]
    fn non_okg_uri_does_not_resolve() {
        let kdir = Path::new("/kb/knowledge");
        assert_eq!(okg_tree_path(kdir, "https://example.com/kb"), None);
        assert_eq!(okg_tree_path(kdir, "okg://"), None);
        assert_eq!(okg_tree_path(kdir, "okg://   "), None);
    }

    #[test]
    fn resolves_index_from_the_named_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        let kdir = tmp.path().join("knowledge");
        agent_dir(
            &agents,
            "cto-assistant",
            "[[stores]]\nname = \"cto-assistant-kb\"\ntree = \"okg://cto-assistant\"\nindex = \"cto-assistant\"\n",
        );
        let found = bound_index_for_tree(
            &[agents],
            &kdir,
            &kdir.join("cto-assistant"),
            Some("cto-assistant"),
        )
        .unwrap();
        assert_eq!(found.index, "cto-assistant");
        assert_eq!(found.store, "cto-assistant-kb");
    }

    /// Why: this is #3892's third finding turned into a guard — the tree tail
    /// and the KB directory coinciding was a coincidence, so a binding that
    /// names a DIFFERENT tree must be refused rather than fed.
    #[test]
    fn reports_a_binding_that_names_a_different_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        let kdir = tmp.path().join("knowledge");
        agent_dir(
            &agents,
            "izzie",
            "[[stores]]\nname = \"bob-kb\"\ntree = \"okg://izzie\"\nindex = \"bob-kb\"\n",
        );
        let err = bound_index_for_tree(&[agents], &kdir, &kdir.join("someone-else"), Some("izzie"))
            .unwrap_err();
        assert!(err.contains("different tree"), "reason was: {err}");
    }

    #[test]
    fn finds_the_only_agent_binding_a_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        let kdir = tmp.path().join("knowledge");
        agent_dir(&agents, "izzie", "[[stores]]\nname = \"bob-kb\"\n");
        agent_dir(
            &agents,
            "cto-assistant",
            "[[stores]]\nname = \"cto-assistant-kb\"\nindex = \"cto-assistant\"\n",
        );
        let found =
            bound_index_for_tree(&[agents], &kdir, &kdir.join("cto-assistant"), None).unwrap();
        assert_eq!(found.agent, "cto-assistant");
        assert_eq!(found.index, "cto-assistant");
    }

    #[test]
    fn refuses_an_ambiguous_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        let kdir = tmp.path().join("knowledge");
        // Two agents both explicitly claiming the same tree.
        agent_dir(
            &agents,
            "one",
            "[[stores]]\nname = \"a\"\ntree = \"okg://shared\"\nindex = \"a\"\n",
        );
        agent_dir(
            &agents,
            "two",
            "[[stores]]\nname = \"b\"\ntree = \"okg://shared\"\nindex = \"b\"\n",
        );
        let err = bound_index_for_tree(&[agents], &kdir, &kdir.join("shared"), None).unwrap_err();
        assert!(err.contains("disambiguate"), "reason was: {err}");
    }

    #[test]
    fn reports_an_agent_with_no_binding() {
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        let kdir = tmp.path().join("knowledge");
        agent_dir(&agents, "plain", "");
        let err =
            bound_index_for_tree(&[agents], &kdir, &kdir.join("plain"), Some("plain")).unwrap_err();
        assert!(err.contains("binds no OKG store"), "reason was: {err}");
    }

    #[test]
    fn reports_an_unknown_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let kdir = tmp.path().join("knowledge");
        let err =
            bound_index_for_tree(&[agents], &kdir, &kdir.join("ghost"), Some("ghost")).unwrap_err();
        assert!(err.contains("was not found"), "reason was: {err}");
    }

    /// Why: an unbound tree is a normal state (a hand-built KB with no agent),
    /// and it must produce a REASON, never a match against some unrelated
    /// agent's index.
    #[test]
    fn reports_a_tree_no_agent_binds() {
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        let kdir = tmp.path().join("knowledge");
        agent_dir(&agents, "izzie", "[[stores]]\nname = \"bob-kb\"\n");
        let err = bound_index_for_tree(&[agents], &kdir, &kdir.join("orphan"), None).unwrap_err();
        assert!(err.contains("no agent binds"), "reason was: {err}");
    }
}
