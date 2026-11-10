//! The project registry joined with the session-alias store (#10895).
//!
//! Why: a watch-list entry is a registry key (plan D2), but its path and its
//! tmux session name come from `project-paths.json`, which is keyed by alias.
//! The dashboard already joins the two (`dashboard_projects.py:434-475`); the
//! CLI must give the same answer for the same files.
//! What: [`Registry`] holds both stores, read from disk under the managed root
//! so the commands work with the daemon down, and answers the join: the alias
//! entry of a project, its path, and the key a directory or a typed name
//! names.
//! Test: `registry_join_matches_alias_exact_then_case_then_path`,
//! `registry_resolves_a_checkout_directory_to_its_key`,
//! `registry_load_reads_both_stores_under_the_root`.

use std::path::{Path, PathBuf};

use anyhow::Context;
use trusty_mpm::core::project_aliases::{ProjectAliasEntry, ProjectAliasStore};
use trusty_mpm::project::store::ProjectStore;
use trusty_mpm::project::{Project, local_checkout_for, registry_data_dir_under};

/// The registry (`projects.json`) and the alias store (`project-paths.json`).
#[derive(Debug, Default)]
pub(crate) struct Registry {
    projects: Vec<Project>,
    aliases: Vec<ProjectAliasEntry>,
    /// Where the registry is read from; refusals name it.
    source: PathBuf,
    /// The registry file was absent when [`Registry::load`] read it.
    absent: bool,
}

impl Registry {
    /// Read both stores under the managed `root`, without the daemon.
    ///
    /// Why: plan D2 and the issue scope: the registry is read from disk.
    /// What: `ProjectStore` over `<root>/project-registry`, `ProjectAliasStore`
    /// over `<root>/project-paths.json`; an absent file is an empty store, an
    /// unreadable or malformed one is an error. An absent `projects.json` is
    /// recorded ([`Registry::is_absent`]) so the import can refuse.
    /// Test: `registry_load_reads_both_stores_under_the_root`.
    pub(crate) async fn load(root: &Path) -> anyhow::Result<Self> {
        let data_dir = registry_data_dir_under(root);
        let mut store = ProjectStore::load(&data_dir).await.with_context(|| {
            format!("cannot read the project registry in {}", data_dir.display())
        })?;
        let projects = store.all().await?;
        let aliases = ProjectAliasStore::load(root)?.list();
        let source = data_dir.join("projects.json");
        let absent = !source
            .try_exists()
            .with_context(|| format!("cannot read {}", source.display()))?;
        Ok(Self {
            absent,
            ..Self::new(projects, aliases, source)
        })
    }

    /// A registry over the given rows; `source` is named in refusals.
    pub(crate) fn new(
        projects: Vec<Project>,
        aliases: Vec<ProjectAliasEntry>,
        source: PathBuf,
    ) -> Self {
        Self {
            projects,
            aliases,
            source,
            absent: false,
        }
    }

    /// The registry file the rows came from.
    pub(crate) fn source(&self) -> &Path {
        &self.source
    }

    /// Whether the registry file did not exist (wrong root, fresh host).
    pub(crate) fn is_absent(&self) -> bool {
        self.absent
    }

    /// The project registered under exactly `key`.
    pub(crate) fn get(&self, key: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.name == key)
    }

    /// The alias entry of `project`: alias equal to the key, else equal
    /// ignoring ASCII case, else the entry whose path is the project's managed
    /// checkout — the dashboard's order.
    pub(crate) fn alias_for(&self, project: &Project) -> Option<&ProjectAliasEntry> {
        let key = project.name.as_str();
        self.aliases
            .iter()
            .find(|a| a.alias == key)
            .or_else(|| {
                self.aliases
                    .iter()
                    .find(|a| a.alias.eq_ignore_ascii_case(key))
            })
            .or_else(|| {
                let checkout = local_checkout_for(project)?;
                self.aliases.iter().find(|a| same_path(&a.path, &checkout))
            })
    }

    /// The key equal to `name`, else equal ignoring ASCII case.
    pub(crate) fn key_named(&self, name: &str) -> Option<&str> {
        self.get(name)
            .or_else(|| {
                self.projects
                    .iter()
                    .find(|p| p.name.eq_ignore_ascii_case(name))
            })
            .map(|p| p.name.as_str())
    }

    /// The project's directory: its alias entry's path, else its checkout.
    pub(crate) fn path_for(&self, project: &Project) -> Option<PathBuf> {
        self.alias_for(project)
            .map(|a| a.path.clone())
            .or_else(|| local_checkout_for(project))
    }

    /// The key of the project whose directory is `path`.
    pub(crate) fn key_for_path(&self, path: &Path) -> Option<&str> {
        self.projects
            .iter()
            .find(|p| self.path_for(p).is_some_and(|own| same_path(&own, path)))
            .map(|p| p.name.as_str())
    }

    /// The key `target` names: a registry key, else a checkout directory.
    ///
    /// Why: `add <key|dir>`; a directory resolves by the same path match the
    /// join uses, so `add .` from a checkout finds its key.
    /// What: an exact key wins; otherwise `target` is made absolute and
    /// matched against each project's path. `None` when neither matches.
    /// Test: `registry_resolves_a_checkout_directory_to_its_key`.
    pub(crate) fn resolve(&self, target: &str) -> Option<&str> {
        if let Some(project) = self.get(target) {
            return Some(project.name.as_str());
        }
        let path = std::path::absolute(target).ok()?;
        self.key_for_path(&path)
    }
}

/// True when `a` and `b` name one directory: equal as written, or equal
/// once both are canonicalized (`/tmp` and `/private/tmp` on macOS).
pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    let trim = |p: &Path| p.components().collect::<PathBuf>();
    if trim(a) == trim(b) {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}
