//! Well-known filesystem paths for the trusty-mpm framework installation.
//!
//! Why: the installer, the daemon, and the file watcher all need a single,
//! consistent answer for "where does the framework live?" — hard-coding
//! `~/.trusty-mpm/...` in three places invites drift.
//! What: [`FrameworkPaths`] resolves the framework directory layout rooted at
//! `~/.trusty-mpm`, plus convenience accessors for the two files the daemon
//! reads directly (the optimizer policy and the framework instructions).
//! Test: `cargo test -p trusty-mpm-core paths` asserts the resolved root
//! contains `.trusty-mpm` and that the subdirectories nest correctly.

use std::path::{Path, PathBuf};

/// Directory name (under the user's home) that holds the framework install.
pub const FRAMEWORK_DIR_NAME: &str = ".trusty-mpm";

/// Resolved paths for a trusty-mpm framework installation.
///
/// Why: groups every framework path behind one value so callers pass a single
/// `FrameworkPaths` instead of recomputing joins.
/// What: the install root and each artifact subdirectory; build with
/// [`FrameworkPaths::default`] (home-relative) or [`FrameworkPaths::under`]
/// (for tests against a temp dir).
/// Test: `default_resolves_under_trusty_mpm`, `under_nests_subdirectories`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameworkPaths {
    /// `~/.trusty-mpm`
    pub root: PathBuf,
    /// `~/.trusty-mpm/framework`
    pub framework: PathBuf,
    /// `~/.trusty-mpm/framework/agents`
    pub agents: PathBuf,
    /// `~/.trusty-mpm/framework/skills`
    pub skills: PathBuf,
    /// `~/.trusty-mpm/framework/hooks`
    pub hooks: PathBuf,
    /// `~/.trusty-mpm/framework/instructions`
    pub instructions: PathBuf,
    /// `~/.trusty-mpm/registry`
    pub registry: PathBuf,
    /// `~/.claude/agents` — where Claude Code reads composed agent files.
    pub claude_agents: PathBuf,
    /// `~/.claude/skills` — where Claude Code reads skill files.
    pub claude_skills: PathBuf,
    /// The trusty-mpm source checkout root, if one could be located.
    ///
    /// Why: the `agents/` git submodule (`agents/agents/`, `agents/skills/`)
    /// is the preferred distribution source for agents and skills. It only
    /// exists in a source checkout, so this is `None` for a binary-only
    /// install — callers then fall back to the bundled assets.
    /// What: the directory holding `.git` discovered by walking the running
    /// binary's ancestors; `None` when no such directory is found.
    pub trusty_mpm_root: Option<PathBuf>,
}

/// Locate the trusty-mpm source checkout root.
///
/// Why: the `agents/` submodule lives at `<root>/agents/` and is only present
/// in a source checkout; resolving it requires finding that checkout from the
/// running binary's location.
/// What: walks the ancestors of the current executable's directory, returning
/// the first that contains a `.git` entry (the repository root). Returns `None`
/// when the executable path is unresolvable or no ancestor has `.git`.
/// Test: `locate_root_finds_git_ancestor`, `locate_root_none_without_git`.
fn locate_trusty_mpm_root(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        if ancestor.join(".git").exists() {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

impl FrameworkPaths {
    /// Resolve the framework layout rooted at the user's home directory.
    ///
    /// Why: production callers want `~/.trusty-mpm` without each one resolving
    /// the home directory itself.
    /// What: locates the home directory via the `dirs` crate, falling back to
    /// the current directory if it cannot be determined (e.g. a stripped CI
    /// environment) so the type is always constructible. This function takes
    /// NO path argument and never consults `std::env::current_dir()`, so it
    /// resolves to exactly one global path per machine/user regardless of
    /// which project workspace the caller happens to be running inside
    /// (owner-confirmed requirement for the #1905 one-time skill-cleanup
    /// migration, which must run once per framework install, never once per
    /// project). Do NOT confuse this with the unrelated, genuinely
    /// project-relative `<project_dir>/.trusty-mpm/` stash directory that
    /// `session_launch::mod.rs` writes `last-instructions.md` into — that is
    /// a bare `project_dir.join(".trusty-mpm")`, not a `FrameworkPaths` value
    /// at all.
    /// Test: `default_resolves_under_trusty_mpm`,
    /// `default_is_a_single_global_path_never_project_relative`.
    #[allow(clippy::should_implement_trait)] // Intentional: no meaningful Default without I/O.
    pub fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self::under(home)
    }

    /// Resolve the framework layout under an arbitrary base directory.
    ///
    /// Why: tests must exercise install / reload logic without touching the
    /// real `~/.trusty-mpm`; pointing `base` at a `tempfile::TempDir` keeps
    /// them hermetic.
    /// What: joins `<base>/.trusty-mpm` and derives every subdirectory from it.
    /// Test: `under_nests_subdirectories`.
    pub fn under(base: impl AsRef<Path>) -> Self {
        let base = base.as_ref();
        let root = base.join(FRAMEWORK_DIR_NAME);
        let framework = root.join("framework");
        let trusty_mpm_root = std::env::current_exe()
            .ok()
            .and_then(|exe| locate_trusty_mpm_root(&exe));
        Self {
            agents: framework.join("agents"),
            skills: framework.join("skills"),
            hooks: framework.join("hooks"),
            instructions: framework.join("instructions"),
            registry: root.join("registry"),
            claude_agents: base.join(".claude").join("agents"),
            claude_skills: base.join(".claude").join("skills"),
            trusty_mpm_root,
            framework,
            root,
        }
    }

    /// Resolve the framework layout from a known framework ROOT (`…/.trusty-mpm`).
    ///
    /// Why: the daemon holds only the resolved framework root (`base/.trusty-mpm`),
    /// not the base/home it was derived from. The HR-3 `/health` staleness check
    /// must rebuild a `FrameworkPaths` whose `.root` equals that exact root — so
    /// the deployed-content (`<base>/.claude/...`) and catalog paths line up with
    /// what the launcher used. Passing the root to [`under`](Self::under) directly
    /// would double-nest (`…/.trusty-mpm/.trusty-mpm`); this constructor inverts
    /// the `base.join(".trusty-mpm")` `under` performs by taking the root's parent
    /// as the base, reproducing the original layout exactly.
    /// What: when `root` ends in `.trusty-mpm`, delegates to
    /// [`under`](Self::under)`(root.parent())`; otherwise (a test root that is not
    /// home-nested) treats `root` itself as the base so the type is still
    /// constructible. Either way the resulting `.root` is the requested directory
    /// when it is `.trusty-mpm`-named.
    /// Test: `from_root_reproduces_under`, `from_root_handles_non_nested`.
    pub fn from_root(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        if root.file_name().and_then(|n| n.to_str()) == Some(FRAMEWORK_DIR_NAME)
            && let Some(base) = root.parent()
        {
            return Self::under(base);
        }
        // The root is not `.trusty-mpm`-nested (e.g. a bare test tempdir). Treat
        // it as the base; subdirectories nest under `<root>/.trusty-mpm` as usual.
        Self::under(root)
    }

    /// Resolve the framework layout for a standalone/managed-driver project
    /// session, targeting the project-local `.claude/` deploy destination.
    ///
    /// Why (issue #1927, DOC-24 SPEC-STANDALONE-MPM-04): the standalone `tm
    /// register/load/run` driver must never deploy content into the user's
    /// REAL global `~/.claude/agents` or `~/.claude/skills` — but
    /// [`FrameworkPaths::default`] always resolves `claude_agents`/
    /// `claude_skills` under the real home directory, which is correct for the
    /// ordinary (non-managed) `tm session start` launch path but violates the
    /// standalone isolation invariant when reused there. The managed driver
    /// already exposes two isolated destinations for deployed content: the
    /// tm-global `CLAUDE_CONFIG_DIR` (`<managed_root>/claude-config/{agents,skills}`,
    /// populated separately by
    /// `core::standalone::global_config::ensure_global_config_dir` before
    /// `load_alias` runs) and the per-alias **project-local**
    /// `repo/.claude/{agents,skills}` (SPEC-STANDALONE-MPM-03 project-local half
    /// / SPEC-STANDALONE-MPM-04 item 1: "`deploy_agents_filtered` target →
    /// `repo/.claude/agents/` (project-local, standard discovery)"). This
    /// constructor targets the latter so `core::standalone::load::load_alias`'s
    /// call into `prepare_session_with_repo_url` writes the project-local half
    /// only, leaving the tm-global half to `ensure_global_config_dir` (no
    /// redundant re-deploy of the SAME content into the SAME destination — the
    /// two calls target two distinct, intentionally-separate trees per the
    /// "effective config = tm-global ⊕ project-local" merge model).
    /// What: starts from [`Self::from_root`]`(managed_root)` so every framework
    /// SOURCE path (`agents`, `skills`, `hooks`, `instructions`, `registry`,
    /// `trusty_mpm_root`, and therefore `agent_source_dir()`/`skill_source_dir()`)
    /// still resolves from the shared framework install — `managed_root`, i.e.
    /// `~/.trusty-mpm` by default or the operator's `--root`/`TRUSTY_MPM_ROOT`
    /// override — exactly as before. Only `claude_agents` and `claude_skills`
    /// (the DEPLOY destinations, and therefore `claude_home_dir()` too) are
    /// overridden to `<project_dir>/.claude/{agents,skills}`.
    /// Test: `for_managed_project_targets_project_local_claude_dirs`,
    /// `for_managed_project_keeps_framework_source_at_managed_root`.
    pub fn for_managed_project(
        managed_root: impl AsRef<Path>,
        project_dir: impl AsRef<Path>,
    ) -> Self {
        let project_dir = project_dir.as_ref();
        let mut paths = Self::from_root(managed_root);
        paths.claude_agents = project_dir.join(".claude").join("agents");
        paths.claude_skills = project_dir.join(".claude").join("skills");
        paths
    }

    /// Resolve the framework layout for a DAEMON-provisioned managed session
    /// workspace (clone-based or in-project worktree), targeting the
    /// project-local `.claude/` deploy destination.
    ///
    /// Why (issue #1931, follow-up to #1927/#1930): #1930 fixed the
    /// **standalone** `tm register/load/run` driver (`core::standalone::load`)
    /// to deploy project-locally via [`for_managed_project`](Self::for_managed_project),
    /// but the **daemon**'s own managed-spawn code paths — the ones bare `tm`,
    /// `tm launch`, and `tm session new` actually exercise — were never
    /// updated and kept calling [`default`](Self::default), which resolves
    /// `claude_agents`/`claude_skills` under the REAL `$HOME/.claude`. Since
    /// the harness cwd for a managed session is the provisioned workspace or
    /// worktree (never the real home), every managed spawn silently deployed
    /// agents/skills somewhere Claude Code's project-skill discovery would
    /// never look, so `/tm-*` slash commands were invisible in the spawned
    /// session. The daemon always resolves its framework root the same way —
    /// home-relative, with no `--root`/`TRUSTY_MPM_ROOT` override support
    /// (unlike the standalone driver) — so this constructor hard-codes
    /// [`default`](Self::default)`().root` as the `managed_root` input to
    /// [`for_managed_project`](Self::for_managed_project) rather than
    /// requiring every daemon call site to compute and pass it.
    /// What: `Self::for_managed_project(Self::default().root, workspace_dir)`
    /// — framework SOURCE paths still resolve from the shared `~/.trusty-mpm`
    /// install; only the deploy destination moves to
    /// `<workspace_dir>/.claude/{agents,skills}`.
    /// Test: `for_managed_workspace_targets_workspace_local_claude_dirs`,
    /// `for_managed_workspace_keeps_framework_source_at_default_root`.
    pub fn for_managed_workspace(workspace_dir: impl AsRef<Path>) -> Self {
        Self::for_managed_project(Self::default().root, workspace_dir)
    }

    /// Path of the token-optimizer policy file (`hooks/optimizer.toml`).
    ///
    /// Why: the daemon reads this at startup and on file-change to build its
    /// `OptimizerConfig`.
    /// What: `hooks/optimizer.toml` under the framework root.
    /// Test: `optimizer_config_path_is_under_hooks`.
    pub fn optimizer_config(&self) -> PathBuf {
        self.hooks.join("optimizer.toml")
    }

    /// Path of the session-overseer policy file (`hooks/overseer.toml`).
    ///
    /// Why: the daemon reads this at startup to build its `OverseerConfig`;
    /// keeping the path next to [`optimizer_config`](Self::optimizer_config)
    /// means both framework hook policies resolve consistently.
    /// What: `hooks/overseer.toml` under the framework root.
    /// Test: `overseer_config_path_is_under_hooks`.
    pub fn overseer_config(&self) -> PathBuf {
        self.hooks.join("overseer.toml")
    }

    /// Path of the user-facing configuration file (`config.toml`).
    ///
    /// Why: all code that needs to read `~/.trusty-mpm/config.toml` should
    /// resolve the path through [`FrameworkPaths`] so the location stays
    /// canonical and tests can redirect it to a temp directory.
    /// What: `<root>/config.toml` — the top-level TOML file the user edits.
    /// Test: `config_toml_path_is_under_root`.
    pub fn config_toml(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// Path of the framework launch instructions (`instructions/INSTRUCTIONS.md`).
    ///
    /// Why: launchers point new Claude Code sessions at this file; it is the
    /// framework artifact owned and overwritten by trusty-mpm on every install.
    /// What: `instructions/INSTRUCTIONS.md` under the framework root.
    /// Test: `instructions_path_is_under_instructions`.
    pub fn framework_instructions(&self) -> PathBuf {
        self.instructions.join("INSTRUCTIONS.md")
    }

    /// Path of the framework launch instructions — explicit-name alias.
    ///
    /// Why: the instruction merge pipeline refers to this file as
    /// `framework_instructions_path`; providing the alias keeps call sites
    /// readable without renaming the established [`framework_instructions`]
    /// accessor.
    /// What: delegates to [`framework_instructions`](Self::framework_instructions).
    /// Test: `framework_instructions_path_matches_accessor`.
    pub fn framework_instructions_path(&self) -> PathBuf {
        self.framework_instructions()
    }

    /// Path of the user-editable instruction stub (`instructions/CLAUDE.md`).
    ///
    /// Why: the installer seeds this stub once for project-specific notes;
    /// distinguishing it from `framework_instructions()` lets the installer
    /// avoid clobbering user edits on re-install.
    /// What: `instructions/CLAUDE.md` under the framework root.
    /// Test: `claude_stub_path_is_under_instructions`.
    pub fn claude_stub(&self) -> PathBuf {
        self.instructions.join("CLAUDE.md")
    }

    /// Directory holding the trusty-mpm agent *source* files.
    ///
    /// Why: the agent build pipeline reads `extends:`-bearing source agents
    /// from here and composes them before deployment. When the `agents/` git
    /// submodule is populated it is the authoritative, version-controlled
    /// source; only a binary-only install falls back to the bundled assets.
    /// What: prefers `<trusty-mpm-root>/agents/agents/` when that directory
    /// exists, otherwise returns `framework/agents` under the framework root.
    /// Test: `agent_source_dir_is_framework_agents`,
    /// `agent_source_dir_prefers_submodule`.
    pub fn agent_source_dir(&self) -> PathBuf {
        if let Some(root) = &self.trusty_mpm_root {
            let submodule = root.join("agents").join("agents");
            if submodule.is_dir() {
                return submodule;
            }
        }
        self.agents.clone()
    }

    /// Directory holding the trusty-mpm skill *source* files.
    ///
    /// Why: the skill deploy step reads `.md` skill files from here and copies
    /// them into `~/.claude/skills/`. As with agents, the `agents/` submodule
    /// is the authoritative source when present.
    /// What: prefers `<trusty-mpm-root>/agents/skills/` when that directory
    /// exists, otherwise returns `framework/skills` under the framework root.
    /// Test: `skill_source_dir_is_framework_skills`,
    /// `skill_source_dir_prefers_submodule`.
    pub fn skill_source_dir(&self) -> PathBuf {
        if let Some(root) = &self.trusty_mpm_root {
            let submodule = root.join("agents").join("skills");
            if submodule.is_dir() {
                return submodule;
            }
        }
        self.skills.clone()
    }

    /// Directory Claude Code reads composed agent files from (`~/.claude/agents`).
    ///
    /// Why: the deploy step writes inheritance-flattened agents here so Claude
    /// Code sees self-contained files with no `extends:` to interpret.
    /// What: `.claude/agents` under the same base this `FrameworkPaths` was
    /// resolved against (the user's home for [`default`](Self::default), the
    /// temp dir for [`under`](Self::under)).
    /// Test: `claude_agents_dir_is_dotclaude_agents`.
    pub fn claude_agents_dir(&self) -> PathBuf {
        self.claude_agents.clone()
    }

    /// Directory Claude Code reads skill files from (`~/.claude/skills`).
    ///
    /// Why: the skill deploy step writes `.md` skill files here so Claude Code
    /// can resolve them at session start.
    /// What: `.claude/skills` under the same base this `FrameworkPaths` was
    /// resolved against.
    /// Test: `claude_skills_dir_is_dotclaude_skills`.
    pub fn claude_skills_dir(&self) -> PathBuf {
        self.claude_skills.clone()
    }

    /// The base directory `.claude/{agents,skills}` nest under — the real home
    /// for [`default`](Self::default), the temp dir for [`under`](Self::under).
    ///
    /// Why (issue #1860): settings-file operations that need `~/.claude`
    /// directly (e.g. deploying the bundled output-style definition) must
    /// honor the same base this `FrameworkPaths` was resolved against. Calling
    /// `dirs::home_dir()` directly at those call sites ignores test isolation:
    /// `FrameworkPaths::under(tempdir)` is supposed to confine ALL filesystem
    /// writes to the temp dir, but a stray `dirs::home_dir()` call re-escapes
    /// to the real `$HOME` and leaks state between test runs.
    /// What: derives the base by walking up two levels from `claude_agents`
    /// (`<base>/.claude/agents` -> `<base>/.claude` -> `<base>`). Falls back to
    /// `claude_agents` itself in the practically-unreachable case where it has
    /// no grandparent (e.g. a root-relative path).
    /// Test: `claude_home_dir_matches_under_base`, `claude_home_dir_matches_default_home`.
    pub fn claude_home_dir(&self) -> PathBuf {
        self.claude_agents
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.claude_agents.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_resolves_under_trusty_mpm() {
        // The home-relative resolver must always land inside a `.trusty-mpm`
        // directory regardless of which home directory the host reports.
        let paths = FrameworkPaths::default();
        assert!(
            paths.root.ends_with(FRAMEWORK_DIR_NAME),
            "root = {}",
            paths.root.display()
        );
        assert!(paths.framework.starts_with(&paths.root));
    }

    // Why (issue #2461 sweep): calls `FrameworkPaths::default()` — which
    // reads `dirs::home_dir()` — twice and asserts the two results are
    // identical. If any other test in this binary redirects `HOME` to a temp
    // dir between the two calls, the assertion spuriously fails. Several
    // such tests exist elsewhere in this crate (e.g.
    // `core::home_trust_seed::tests::is_idempotent`) and are marked
    // `#[serial_test::serial]`; this test must join the same serial group so
    // it can never observe a mid-flight `HOME` mutation from them.
    #[serial_test::serial]
    #[test]
    fn default_is_a_single_global_path_never_project_relative() {
        // Regression guard (owner feedback on #1905): the #1905 stale-skill
        // migration resolves its marker file and `~/.claude/skills/` target
        // via `FrameworkPaths::default()`. That function takes NO path
        // argument — it resolves purely from `dirs::home_dir()` — so it is
        // structurally incapable of varying between invocations from
        // different project workspaces (e.g. `duettoresearch/apex` vs
        // `trusty-tools`): there is no "which project am I in" input for it
        // to consult in the first place. Calling it twice must yield the
        // identical root, proving it is a pure, argument-free resolution.
        let a = FrameworkPaths::default();
        let b = FrameworkPaths::default();
        assert_eq!(
            a.root, b.root,
            "FrameworkPaths::default() must resolve to one identical global \
             root regardless of invocation context — it takes no project or \
             cwd argument to vary by"
        );
        assert_eq!(a.claude_skills, b.claude_skills);

        // Contrast with the genuinely project-relative resolver used for
        // per-workspace state (e.g. session_launch::mod.rs's
        // `<project>/.trusty-mpm/last-instructions.md` stash, which is NOT
        // FrameworkPaths at all but a bare `project_dir.join(".trusty-mpm")`).
        // `FrameworkPaths::under(base)` IS project-relative when a caller
        // explicitly passes a project directory as `base` — proving the two
        // resolvers are genuinely different, not the same function called
        // two different ways. The #1905 migration must only ever call
        // `default()`, never `under(project_dir)`.
        let project_a = FrameworkPaths::under("/tmp/fake-project-a");
        let project_b = FrameworkPaths::under("/tmp/fake-project-b");
        assert_ne!(
            project_a.root, project_b.root,
            "under(base) IS expected to vary by base — this is the contrast case"
        );
        assert_ne!(
            a.root, project_a.root,
            "the global default() root must never coincide with a per-project under() root"
        );
    }

    #[test]
    fn under_nests_subdirectories() {
        // Given an explicit base, every subdirectory must nest under the
        // framework root with the documented layout.
        let paths = FrameworkPaths::under("/base");
        assert_eq!(paths.root, PathBuf::from("/base/.trusty-mpm"));
        assert_eq!(
            paths.framework,
            PathBuf::from("/base/.trusty-mpm/framework")
        );
        assert_eq!(
            paths.agents,
            PathBuf::from("/base/.trusty-mpm/framework/agents")
        );
        assert_eq!(
            paths.skills,
            PathBuf::from("/base/.trusty-mpm/framework/skills")
        );
        assert_eq!(
            paths.hooks,
            PathBuf::from("/base/.trusty-mpm/framework/hooks")
        );
        assert_eq!(
            paths.instructions,
            PathBuf::from("/base/.trusty-mpm/framework/instructions")
        );
        assert_eq!(paths.registry, PathBuf::from("/base/.trusty-mpm/registry"));
    }

    #[test]
    fn from_root_reproduces_under() {
        // A `.trusty-mpm`-named root must rebuild the exact layout `under` would
        // have produced for the parent base (no double-nesting).
        let under = FrameworkPaths::under("/base");
        let from_root = FrameworkPaths::from_root("/base/.trusty-mpm");
        assert_eq!(from_root.root, under.root);
        assert_eq!(from_root.claude_agents, under.claude_agents);
        assert_eq!(from_root.agents, under.agents);
    }

    #[test]
    fn from_root_handles_non_nested() {
        // A bare (non-`.trusty-mpm`) root is treated as the base, so subdirs nest
        // under `<root>/.trusty-mpm` — the type is always constructible.
        let from_root = FrameworkPaths::from_root("/tmp/testroot");
        assert_eq!(from_root.root, PathBuf::from("/tmp/testroot/.trusty-mpm"));
    }

    // Issue #1927: the standalone driver's project-local deploy destination
    // must be `<project_dir>/.claude/{agents,skills}`, never the real home.
    #[test]
    fn for_managed_project_targets_project_local_claude_dirs() {
        let paths = FrameworkPaths::for_managed_project("/managed-root", "/some/project/repo");
        assert_eq!(
            paths.claude_agents_dir(),
            PathBuf::from("/some/project/repo/.claude/agents"),
            "agents must deploy under the project dir, not the framework base"
        );
        assert_eq!(
            paths.claude_skills_dir(),
            PathBuf::from("/some/project/repo/.claude/skills"),
            "skills must deploy under the project dir, not the framework base"
        );
        assert_eq!(
            paths.claude_home_dir(),
            PathBuf::from("/some/project/repo"),
            "claude_home_dir() must follow the overridden claude_agents base"
        );
    }

    // Issue #1927: only the deploy DESTINATION moves project-local — the
    // framework SOURCE paths (agent/skill templates, hooks, instructions,
    // registry) must keep resolving from the shared managed_root exactly as
    // `from_root` would produce, so the same framework install is reused
    // across every alias.
    #[test]
    fn for_managed_project_keeps_framework_source_at_managed_root() {
        let managed_root = "/managed-root/.trusty-mpm";
        let from_root = FrameworkPaths::from_root(managed_root);
        let managed_project = FrameworkPaths::for_managed_project(managed_root, "/proj/repo");

        assert_eq!(managed_project.root, from_root.root);
        assert_eq!(managed_project.agents, from_root.agents);
        assert_eq!(managed_project.skills, from_root.skills);
        assert_eq!(managed_project.hooks, from_root.hooks);
        assert_eq!(managed_project.instructions, from_root.instructions);
        assert_eq!(managed_project.registry, from_root.registry);
        assert_eq!(
            managed_project.agent_source_dir(),
            from_root.agent_source_dir()
        );
        assert_eq!(
            managed_project.skill_source_dir(),
            from_root.skill_source_dir()
        );

        // Only the deploy destinations diverge.
        assert_ne!(managed_project.claude_agents, from_root.claude_agents);
        assert_ne!(managed_project.claude_skills, from_root.claude_skills);
    }

    // Issue #1931: the daemon's managed-spawn deploy destination must be the
    // provisioned workspace/worktree directory, never the real home — mirrors
    // `for_managed_project_targets_project_local_claude_dirs` but exercises
    // the daemon-facing convenience constructor instead of the standalone one.
    #[test]
    fn for_managed_workspace_targets_workspace_local_claude_dirs() {
        let paths = FrameworkPaths::for_managed_workspace("/some/workspace/or/worktree");
        assert_eq!(
            paths.claude_agents_dir(),
            PathBuf::from("/some/workspace/or/worktree/.claude/agents"),
            "agents must deploy under the workspace dir, not the real home"
        );
        assert_eq!(
            paths.claude_skills_dir(),
            PathBuf::from("/some/workspace/or/worktree/.claude/skills"),
            "skills must deploy under the workspace dir, not the real home"
        );
        assert_eq!(
            paths.claude_home_dir(),
            PathBuf::from("/some/workspace/or/worktree"),
            "claude_home_dir() must follow the overridden claude_agents base"
        );
    }

    // Issue #1931: only the deploy DESTINATION moves workspace-local — the
    // framework SOURCE paths must keep resolving from `FrameworkPaths::default()`'s
    // root (the daemon's single home-relative framework install), matching
    // `for_managed_project_keeps_framework_source_at_managed_root`'s guarantee
    // for the sibling standalone constructor.
    //
    // Why serial (issue #2461 sweep): `FrameworkPaths::default()` is called
    // directly AND indirectly (via `for_managed_workspace` -> `default().root`)
    // — two separate `dirs::home_dir()` reads that must agree. Serialized
    // against other `HOME`-redirecting tests in this binary; reproduced
    // failing under `--test-threads=32` before this fix (left: temp-dir root,
    // right: real `$HOME` root).
    #[serial_test::serial]
    #[test]
    fn for_managed_workspace_keeps_framework_source_at_default_root() {
        let default_paths = FrameworkPaths::default();
        let workspace_paths = FrameworkPaths::for_managed_workspace("/some/workspace");

        assert_eq!(workspace_paths.root, default_paths.root);
        assert_eq!(workspace_paths.agents, default_paths.agents);
        assert_eq!(workspace_paths.skills, default_paths.skills);
        assert_eq!(workspace_paths.hooks, default_paths.hooks);
        assert_eq!(workspace_paths.instructions, default_paths.instructions);
        assert_eq!(workspace_paths.registry, default_paths.registry);

        // Only the deploy destinations diverge from the real-home default.
        assert_ne!(workspace_paths.claude_agents, default_paths.claude_agents);
        assert_ne!(workspace_paths.claude_skills, default_paths.claude_skills);
    }

    #[test]
    fn locate_root_finds_git_ancestor() {
        // A `.git` directory in an ancestor must be reported as the root.
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        let nested = tmp.path().join("crates").join("trusty-mpm-core");
        std::fs::create_dir_all(&nested).unwrap();
        let found = locate_trusty_mpm_root(&nested.join("dummy-exe"));
        assert_eq!(found.as_deref(), Some(tmp.path()));
    }

    #[test]
    fn locate_root_none_without_git() {
        // With no `.git` anywhere above, no root can be located.
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(locate_trusty_mpm_root(&nested.join("exe")), None);
    }

    #[test]
    fn agent_source_dir_prefers_submodule() {
        // When the `agents/agents/` submodule directory exists under the
        // located root, it must win over the bundled `framework/agents` path.
        let tmp = tempfile::TempDir::new().unwrap();
        let submodule = tmp.path().join("agents").join("agents");
        std::fs::create_dir_all(&submodule).unwrap();
        let mut paths = FrameworkPaths::under("/base");
        paths.trusty_mpm_root = Some(tmp.path().to_path_buf());
        assert_eq!(paths.agent_source_dir(), submodule);
    }

    #[test]
    fn skill_source_dir_prefers_submodule() {
        // When the `agents/skills/` submodule directory exists under the
        // located root, it must win over the bundled `framework/skills` path.
        let tmp = tempfile::TempDir::new().unwrap();
        let submodule = tmp.path().join("agents").join("skills");
        std::fs::create_dir_all(&submodule).unwrap();
        let mut paths = FrameworkPaths::under("/base");
        paths.trusty_mpm_root = Some(tmp.path().to_path_buf());
        assert_eq!(paths.skill_source_dir(), submodule);
    }

    #[test]
    fn skill_source_dir_is_framework_skills() {
        // With no submodule, skill sources fall back to `framework/skills`.
        let mut paths = FrameworkPaths::under("/base");
        paths.trusty_mpm_root = None;
        assert_eq!(
            paths.skill_source_dir(),
            PathBuf::from("/base/.trusty-mpm/framework/skills")
        );
    }

    #[test]
    fn claude_skills_dir_is_dotclaude_skills() {
        // Skills must deploy to `.claude/skills` under the base.
        let paths = FrameworkPaths::under("/base");
        assert_eq!(
            paths.claude_skills_dir(),
            PathBuf::from("/base/.claude/skills")
        );
    }

    #[test]
    fn claude_home_dir_matches_under_base() {
        // Issue #1860: `claude_home_dir()` must recover the exact `base` passed
        // to `under()`, not some hardcoded or re-resolved home directory — this
        // is what lets output-style deploy honor test isolation.
        let paths = FrameworkPaths::under("/base");
        assert_eq!(paths.claude_home_dir(), PathBuf::from("/base"));
    }

    // Why (issue #2461 sweep): reads `dirs::home_dir()` twice — once inside
    // `FrameworkPaths::default()`, once directly — and compares the results.
    // Serialized against other `HOME`-redirecting tests in this binary for
    // the same reason as `default_is_a_single_global_path_never_project_relative`.
    #[serial_test::serial]
    #[test]
    fn claude_home_dir_matches_default_home() {
        // The home-relative resolver's `claude_home_dir()` must equal the real
        // home directory `dirs::home_dir()` reports (falling back to "." like
        // `default()` does), keeping the two resolution paths consistent.
        let paths = FrameworkPaths::default();
        let expected = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        assert_eq!(paths.claude_home_dir(), expected);
    }

    #[test]
    fn optimizer_config_path_is_under_hooks() {
        let paths = FrameworkPaths::under("/base");
        assert_eq!(
            paths.optimizer_config(),
            PathBuf::from("/base/.trusty-mpm/framework/hooks/optimizer.toml")
        );
    }

    #[test]
    fn overseer_config_path_is_under_hooks() {
        let paths = FrameworkPaths::under("/base");
        assert_eq!(
            paths.overseer_config(),
            PathBuf::from("/base/.trusty-mpm/framework/hooks/overseer.toml")
        );
    }

    #[test]
    fn instructions_path_is_under_instructions() {
        let paths = FrameworkPaths::under("/base");
        assert_eq!(
            paths.framework_instructions(),
            PathBuf::from("/base/.trusty-mpm/framework/instructions/INSTRUCTIONS.md")
        );
    }

    #[test]
    fn framework_instructions_path_matches_accessor() {
        // The explicit-name alias must resolve identically to the original.
        let paths = FrameworkPaths::under("/base");
        assert_eq!(
            paths.framework_instructions_path(),
            paths.framework_instructions()
        );
    }

    #[test]
    fn claude_stub_path_is_under_instructions() {
        // The user stub lives alongside the framework instructions but under
        // the `CLAUDE.md` name Claude Code reads by convention.
        let paths = FrameworkPaths::under("/base");
        assert_eq!(
            paths.claude_stub(),
            PathBuf::from("/base/.trusty-mpm/framework/instructions/CLAUDE.md")
        );
    }

    #[test]
    fn agent_source_dir_is_framework_agents() {
        // With no submodule, agent sources fall back to `framework/agents`.
        let mut paths = FrameworkPaths::under("/base");
        paths.trusty_mpm_root = None;
        assert_eq!(
            paths.agent_source_dir(),
            PathBuf::from("/base/.trusty-mpm/framework/agents")
        );
    }

    #[test]
    fn claude_agents_dir_is_dotclaude_agents() {
        // Composed agents must deploy to `.claude/agents` under the base —
        // sibling to `.trusty-mpm`, not nested within it.
        let paths = FrameworkPaths::under("/base");
        assert_eq!(
            paths.claude_agents_dir(),
            PathBuf::from("/base/.claude/agents")
        );
    }

    #[test]
    fn framework_instructions_and_stub_are_distinct() {
        // The framework artifact and the user stub must never resolve to the
        // same path, or the installer would overwrite user edits.
        let paths = FrameworkPaths::under("/base");
        assert_ne!(paths.framework_instructions(), paths.claude_stub());
    }

    #[test]
    fn config_toml_path_is_under_root() {
        // The user config file must live directly under the framework root, not
        // nested in a subdirectory, so it is easy to locate and edit.
        let paths = FrameworkPaths::under("/base");
        assert_eq!(
            paths.config_toml(),
            PathBuf::from("/base/.trusty-mpm/config.toml")
        );
    }
}
