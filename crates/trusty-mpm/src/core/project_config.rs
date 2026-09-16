//! The committed, project-level trusty-mpm config file (#5207).
//!
//! Why: trusty-mpm's settings were split across four surfaces, none of which
//! travel with the repository — `~/.trusty-mpm/config.toml`
//! ([`crate::core::config::MpmConfig`]), `~/.trusty-tools/trusty-mpm/config.yaml`
//! ([`crate::core::trusty_tools_config::TrustyToolsConfig`]), the machine-global
//! `~/.trusty-mpm/project-registry/projects.json`, and `$XDG_CONFIG_HOME`. Every
//! one is per-HOST, so a project's own conventions — "this repo launches on main,
//! never in a worktree" — had to be re-declared by every operator on every
//! machine, and could not be reviewed, versioned, or diffed. The owner ruling
//! (2026-08-08) is that configuration belongs at the PROJECT level and is
//! unitary: if a setting is configurable, every system reads the same value.
//!
//! What: [`PROJECT_CONFIG_FILE`] names one TOML file at the project root that is
//! TRACKED IN GIT — it travels with clones and shows up in PR diffs — and sits at
//! the TOP of every precedence chain it participates in.
//! [`ProjectLevelConfig::from_toml`] parses it with `deny_unknown_fields`, so a
//! misspelled key is an error rather than a silent no-op;
//! [`ProjectLevelConfig::load`] is the fallible loader and
//! [`load_or_report`] the lenient one used on the spawn path.
//!
//! `workspace_root` is deliberately NOT a member of this surface (owner
//! ruling): it decides where a project gets CLONED, so it cannot be read from
//! the project that does not exist yet. It stays host-level in
//! [`crate::core::trusty_tools_config::workspace_root`]. `auto_resume` likewise
//! stays host-level — it is a property of the operator's supervisor, not of the
//! repository.
//!
//! Test: `project_config_parses_worktree`, `project_config_parses_agent_worktree`,
//! `project_config_rejects_unknown_key`,
//! `project_config_absent_is_none`, `project_config_rejects_wrong_type`,
//! `project_config_empty_file_is_all_none` in `project_config_tests.rs`.
//!
//! [`PROJECT_CONFIG_FILE`]: crate::core::project_config::PROJECT_CONFIG_FILE
//! [`ProjectLevelConfig::from_toml`]: crate::core::project_config::ProjectLevelConfig::from_toml
//! [`ProjectLevelConfig::load`]: crate::core::project_config::ProjectLevelConfig::load
//! [`load_or_report`]: crate::core::project_config::load_or_report

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name of the committed project-level config, at the project root.
///
/// Why: this is a ROOT-LEVEL dotfile rather than a member of the existing
/// `<project>/.trusty-mpm/` directory, and that choice is load-bearing. Owner
/// ruling 4 requires the file to be tracked in git. `.trusty-mpm/` is where
/// trusty-mpm writes machine-local session state (`sessions/`, `logs/`), so
/// projects gitignore it wholesale — this very repository ignores
/// `.trusty-mpm/*`, which already makes the #4832 `framework/manifest.toml`
/// layer untrackable here. Putting a file that MUST be committed inside a
/// directory that exists to hold uncommittable state would require every
/// consumer project to carve a `!` re-include out of its own ignore rules, and
/// trusty-mpm cannot reach into a consumer's `.gitignore`. A root dotfile is
/// trackable by default everywhere, and sits beside the other
/// committed-by-convention project files (`Cargo.toml`, `.gitignore`).
/// What: `.trusty-mpm.toml`.
/// Test: `project_config_path_is_a_root_dotfile`.
pub const PROJECT_CONFIG_FILE: &str = ".trusty-mpm.toml";

/// Why a project-level config could not be used.
///
/// Why: the file is COMMITTED, so a bad edit is pushed to everyone. The error
/// must name the file and carry serde's own message (which identifies the
/// offending key), so the operator who reads the log can fix it without
/// guessing.
/// What: `Io` for a file that exists but cannot be read; `Malformed` for a file
/// that is not valid TOML or carries a key the schema does not define. An ABSENT
/// file is not an error — [`ProjectLevelConfig::load`] returns `Ok(None)`.
/// Test: `project_config_rejects_unknown_key`, `project_config_rejects_wrong_type`.
#[derive(Debug, thiserror::Error)]
pub enum ProjectConfigError {
    /// The file exists but could not be read.
    #[error("could not read {}: {source}", .path.display())]
    Io {
        /// The file that could not be read.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// The file is not valid TOML, or carries a key the schema does not define.
    #[error("{} is not a valid project config: {source}", .path.display())]
    Malformed {
        /// The offending file.
        path: PathBuf,
        /// serde's parse error, which names the unrecognised or ill-typed key.
        #[source]
        source: toml::de::Error,
    },
}

/// The committed, project-level trusty-mpm configuration.
///
/// Why: one struct per project-level setting keeps the precedence chains honest
/// — each field is the TOP layer of exactly one resolution, and a field that no
/// resolver reads cannot exist unnoticed (the `default_model` orphan this issue
/// also fixes was exactly that failure at the host level).
///
/// `deny_unknown_fields` is applied HERE and deliberately not to the pre-existing
/// host-level config structs. There is no legacy corpus of `.trusty-mpm.toml`
/// files to break, and this file is reviewed in a PR before it reaches anyone
/// else, so a hard rejection is caught by the author rather than suffered by the
/// team. See [`crate::core::config_keys`] for what the host-level structs get
/// instead, and why strictness there would be a regression rather than a fix.
///
/// What: every field is `Option`, so an absent key means "this layer declines to
/// decide" and resolution falls through to the next one down. An empty file is
/// valid and overrides nothing.
/// Test: `project_config_parses_worktree`, `project_config_empty_file_is_all_none`,
/// `project_config_rejects_unknown_key`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
// #5207: a misspelled key in a COMMITTED config must fail loudly, not no-op.
#[serde(deny_unknown_fields)]
pub struct ProjectLevelConfig {
    /// Whether this project's managed sessions get a per-session git worktree.
    ///
    /// Why: the worktree decision is a property of the PROJECT's workflow — a
    /// repo with a direct-to-main flow wants every session in the live checkout
    /// — so it belongs in the repo, not in each operator's machine-global
    /// `projects.json`. This field is the highest-precedence layer of that
    /// decision; see
    /// [`crate::project::worktree_enabled_for_project`] for the full chain.
    /// What: `None` → this project does not decide; the registry layer
    /// (`projects.json`) answers, and failing that the built-in `true`.
    /// `Some(false)` → launch directly in the checkout. `Some(true)` → force
    /// worktree isolation even if an operator's local registry disabled it.
    /// Test: `project_config_parses_worktree`,
    /// `worktree_project_config_overrides_registry`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<bool>,

    /// Whether a dispatched agent in this project gets a worktree of its own.
    ///
    /// Why (#5814): ADR-0048 decision 1 grants every dispatched writer its own
    /// worktree when the session stands in a main checkout, and that grant is
    /// mechanical — it never reads the dispatch prompt, so no instruction can
    /// wave it off. Worktree isolation pays for concurrent writers and separate
    /// build state; a writing or documentation repo has neither, and pays only
    /// the costs: agent edits land in `.claude/worktrees/agent-<id>/` and never
    /// reach the checkout, a second agent has to copy them across, and the trees
    /// and branches pile up needing reclamation. This field is that project
    /// class's opt-out.
    ///
    /// What: `None` (the default) → the grant behaves exactly as it did before
    /// this key existed. `Some(false)` → dispatched agents stay in the main
    /// checkout: nothing is created and nothing needs reclaiming. `Some(true)`
    /// → the default, stated explicitly.
    ///
    /// This is a SEPARATE key from [`Self::worktree`], not a widening of it.
    /// `worktree` decides where a managed SESSION is placed and ADR-0044
    /// decision 6 narrowed its live effect to the daemon-unreachable
    /// provisioning fallback; reusing it here would give one key two unrelated
    /// meanings again — the double duty ADR-0037 called out. Setting this key
    /// exempts the dispatch from the worktree GRANT only: the ADR-0044
    /// main-checkout write boundary is untouched, so a source-file edit there is
    /// still denied.
    /// Test: `project_config_parses_agent_worktree`,
    /// `agent_worktree_opt_out_is_honoured`,
    /// `grants_nothing_when_the_project_opts_out`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_worktree: Option<bool>,

    /// Default model id (or tier alias) for sessions launched in this project.
    ///
    /// Why: model choice is a project economics decision (a docs repo does not
    /// need Opus), and it was previously only expressible per-host. This is the
    /// top layer of the same chain that
    /// [`crate::core::config::resolve_agent_model`] already terminates in, so
    /// setting it here reaches every launch path without a second resolver.
    /// What: `None` → fall through to the host layers. `Some(m)` → `m` becomes
    /// the effective `models.default`, still subject to an explicit `--model`
    /// flag, a per-agent override, and agent frontmatter, all of which are
    /// MORE specific than a default and therefore still win.
    /// Test: `project_default_model_tops_the_chain`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,

    /// What a session launched in this project is allowed to load beyond the
    /// framework defaults.
    ///
    /// Why (#7422): MCP servers and Claude Code plugins are declared once per
    /// HOST — the tm-managed `.claude.json` `mcpServers` map and the managed
    /// `settings.json` `enabledPlugins` map — so one `tm mcp add` or one
    /// `claude plugin install` loaded that server or plugin into every session
    /// on the machine. The owner ruling (2026-09-11) is default-deny, and the
    /// allowlist is a property of the repository rather than the operator's
    /// laptop, so it belongs on this committed surface.
    /// What: `None` (the default) → deny-all beyond the trusty-* builtins and
    /// this project's own `.mcp.json`. See [`SessionScopeConfig`].
    /// Test: `project_config_parses_session_scope`,
    /// `project_config_rejects_unknown_session_key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionScopeConfig>,

    /// Ask every prompt composed for this project for a `## Prompt feedback`
    /// addendum (#7688).
    ///
    /// Why: whether a repository wants its prompts critiqued is a property of
    /// the repository — a high-churn harness repo is exactly where the signal
    /// pays, and a stable consumer project is where the extra five lines per
    /// response are pure cost. So it belongs on this committed surface, and it
    /// is a top-level scalar for the same reason [`Self::worktree`] and
    /// [`Self::agent_worktree`] are: it decides one thing with one boolean.
    /// It is deliberately NOT a member of [`SessionScopeConfig`], which is two
    /// ALLOWLISTS and nothing else — a toggle there would give that table two
    /// unrelated meanings.
    ///
    /// What: `None` (the default) → this project does not decide, and
    /// `~/.trusty-mpm/config.toml`'s `[pm] prompt_self_improvement` answers,
    /// failing that `false`. `Some(true)` → on even where the host default is
    /// off. `Some(false)` → off even where the host default is on. Full chain:
    /// [`crate::core::prompt_self_improvement::enabled_for`].
    /// Test: `project_config_parses_prompt_self_improvement`,
    /// `project_config_prompt_self_improvement_defaults_to_none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_self_improvement: Option<bool>,

    /// This repository holds documents, so ADR-0044's source class is empty here
    /// (#7905).
    ///
    /// Why: ADR-0044 decision 1 restricts a main checkout to documents and
    /// configuration, and decides "source" by EXTENSION
    /// ([`crate::core::project_config`]'s consumer,
    /// `pm_guard::is_source_code_path`). A prose repository whose own CLAUDE.md
    /// forbids worktrees has no second place to write, so a `.py` helper beside
    /// an article is unwritable and uncommittable by both routes at once: the
    /// reported `git mv …/make-graphics.py …/archive/` could be landed from
    /// neither the checkout nor a worktree the project does not permit. The
    /// boundary's PURPOSE — keeping source changes out of a tree other sessions
    /// share — has no subject in a repository that ships no source, so the
    /// project says so once, here, rather than the guard guessing from a file
    /// census.
    ///
    /// What: `None` (the default) and `Some(false)` → the boundary is exactly
    /// what it was; `Some(true)` → [`crate::core::project_config::documents_only_at`]
    /// answers yes for this checkout and the two ADR-0044/ADR-0049 rules that
    /// consult it treat every staged or written path as a document. It is a
    /// SEPARATE key from [`Self::agent_worktree`] and does not imply it: opting
    /// dispatched agents out of worktrees says where they stand, while this says
    /// what the repository contains, and a source repository may well want the
    /// first without the second.
    ///
    /// It relaxes ONE question — is this path source — and nothing else. The
    /// live-writer check on a commit (ADR-0049 decision 3), the single-command
    /// rule (decision 8), the destructive-git rules and every secret-file rule
    /// are untouched.
    /// Test: `project_config_parses_documents_only`,
    /// `documents_only_at_reads_a_committed_declaration`,
    /// `documents_only_at_ignores_an_uncommitted_declaration`,
    /// `allows_a_source_write_in_a_documents_only_checkout`,
    /// `classify_staged_commit_permits_a_rename_in_a_documents_only_project`,
    /// `classify_staged_commit_denies_landing_a_documents_only_declaration`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documents_only: Option<bool>,
}

/// The `[session]` table: this project's MCP-server and plugin allowlists.
///
/// Why: `plugins` is an ALLOWLIST, never a deny-list: an absent key denies, and
/// the list takes effect only once `tm project trust --dir <path>` has recorded a
/// grant for the directory. `mcp_servers` was its MCP counterpart until #7892
/// retired it — see that field's own doc.
/// What: `plugins` names Claude Code plugins, either as the full
/// `<plugin>@<marketplace>` key or the bare `<plugin>` half.
///
/// ```toml
/// [session]
/// plugins = ["aws-core"]
/// ```
/// Test: `project_config_parses_session_scope`,
/// `project_config_session_defaults_to_none`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
// #7422: a misspelled key here silently denies a server the operator meant to
// allow, which reads as a tm bug rather than a typo. Fail loudly instead.
#[serde(deny_unknown_fields)]
pub struct SessionScopeConfig {
    /// RETIRED by #7892; parsed so an existing file still loads, never read.
    ///
    /// Why: this was the `[session]` half that opted a user-scope MCP server
    /// into one project. Under the Claude Code standard every user-scope server
    /// loads in every session with no opt-in, so the key grants nothing. It
    /// stays in the schema because the table is `deny_unknown_fields`: removing
    /// it would turn an existing `.trusty-mpm.toml` into a parse error.
    /// What: accepted and ignored. Delete it from your config when convenient.
    /// Test: `project_config_parses_session_scope`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<String>>,

    /// Claude Code plugin names this project's sessions may load.
    ///
    /// Why: see [`ProjectLevelConfig::session`].
    /// What: `None` or an empty list → every plugin the managed config dir
    /// knows about is written `false` into the project's
    /// `.claude/settings.json`.
    /// Test: `project_config_parses_session_scope`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugins: Option<Vec<String>>,
}

impl ProjectLevelConfig {
    /// Parse a project config from TOML text.
    ///
    /// Why: separated from the file read so the schema contract — above all the
    /// `deny_unknown_fields` rejection — is assertable without touching a
    /// filesystem.
    /// What: `Ok(cfg)` for valid TOML whose every key is defined by this struct;
    /// `Err(Malformed)` for a syntax error, a wrongly-typed value, or an
    /// unrecognised key. `path` is carried into the error for the message only.
    /// Test: `project_config_parses_worktree`, `project_config_rejects_unknown_key`,
    /// `project_config_rejects_wrong_type`.
    pub fn from_toml(raw: &str, path: &Path) -> Result<Self, ProjectConfigError> {
        toml::from_str(raw).map_err(|source| ProjectConfigError::Malformed {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Load `<project_dir>/.trusty-mpm.toml`, if it exists.
    ///
    /// Why: the fallible entry point, for callers that want to surface a bad
    /// config to a human (`tm doctor`, a future `tm config check`) rather than
    /// degrade past it.
    /// What: `Ok(None)` when the file is absent — the overwhelmingly common
    /// case, and not an error. `Ok(Some(cfg))` when it parses. `Err` when it
    /// exists but cannot be read or does not parse.
    ///
    /// The file is read from `project_dir` AS GIVEN, not from the harness root
    /// [`crate::core::harness_root::harness_root_for`] would resolve. That is
    /// deliberate: unlike the machine-local `.trusty-mpm/` state #4832 hoisted
    /// to the owning checkout, this file is TRACKED, so it is part of the
    /// branch's working set exactly like `Cargo.toml`. A worktree testing a
    /// branch that changes this file must see the changed file, or the PR that
    /// changes it could never be reviewed by running it.
    /// Test: `project_config_absent_is_none`, `project_config_reads_from_disk`.
    pub fn load(project_dir: &Path) -> Result<Option<Self>, ProjectConfigError> {
        let path = project_dir.join(PROJECT_CONFIG_FILE);
        match std::fs::read_to_string(&path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(ProjectConfigError::Io { path, source }),
            Ok(raw) => Self::from_toml(&raw, &path).map(Some),
        }
    }
}

/// Load a project config for a runtime path that must not be blocked by a bad file.
///
/// Why: the spawn path cannot abort on a malformed config. This file is
/// COMMITTED, so one bad push would otherwise break every session for every
/// operator on the project at once — a far worse outcome than the setting not
/// applying. The error contract therefore matches the harness manifest's
/// (HR-2, [`crate::core::manifest::resolve`](crate::core::catchup::resolve)): a broken layer is skipped, never
/// fatal.
///
/// Skipped is not silent. The file's values are REJECTED WHOLESALE — nothing in
/// a file that failed `deny_unknown_fields` is trusted, because a typo means the
/// author's intent is unknown, not partially known — and the parse error is
/// logged at `error` level (not `warn`), naming the file and the offending key.
/// What: `Some(cfg)` when the file parses; `None` when it is absent (silent) or
/// unusable (logged).
/// Test: `load_or_report_returns_none_for_unknown_key`,
/// `load_or_report_returns_none_when_absent`.
pub fn load_or_report(project_dir: &Path) -> Option<ProjectLevelConfig> {
    match ProjectLevelConfig::load(project_dir) {
        Ok(cfg) => cfg,
        Err(err) => {
            tracing::error!(
                "project config ignored: {err}; \
                 no setting from this file is applied until it parses"
            );
            None
        }
    }
}

/// Has the checkout rooted at `root` declared itself documents-only (#7905)?
///
/// Why: the ONE place the ADR-0044 write boundary and the ADR-0049 commit gate
/// ask the question, so the two cannot drift into disagreeing about whether a
/// file may be written but not committed — the exact incoherence ADR-0049
/// decision 2 exists to remove.
///
/// What: `true` only when the blob at `HEAD:.trusty-mpm.toml` PARSES and carries
/// `documents_only = true`. Every other outcome is `false`, and `false` leaves
/// the caller's deny exactly as it was: no such path at `HEAD`, an unborn `HEAD`,
/// git unavailable, a blob that fails `deny_unknown_fields`, and an absent or
/// `false` key all keep the boundary. That direction is the whole safety case
/// for the key — a declaration that cannot be read never widens anything, which
/// is
/// [ADR-0045](../../../docs/adr/0045-distinguish-absent-from-undeterminable-on-destructive-paths.md)
/// applied to a relaxation rather than to a destructive path. Callers ask it
/// only once a deny is otherwise certain, so ordinary traffic pays no
/// subprocess.
///
/// #7905 review, CRITICAL 1: it reads `HEAD`, never the WORKING TREE. The
/// working-tree file is not source under `is_source_code_path`, so writing it
/// is admitted by the very boundary it would then switch off — `Write
/// .trusty-mpm.toml` followed by `Write src/lib.rs` defeated ADR-0044 in two
/// tool calls on the built binary. Reading the committed blob is what makes the
/// declaration cost a review: an untracked file, a staged-but-uncommitted one,
/// and an uncommitted edit to a tracked one all grant nothing, and the only way
/// to move `HEAD` in a main checkout is the ADR-0044 route through a worktree
/// branch and a pull request — which
/// [`staged_declaration_changes_documents_only`] is what enforces.
///
/// `root` is the checkout root the caller already resolved
/// ([`crate::core::project_aliases::main_checkout_root`]), never the `cwd`: the
/// declaration belongs to the repository, and reading it from a subdirectory
/// would let a nested directory answer for a project that never declared
/// anything.
/// Test: `documents_only_at_reads_a_committed_declaration`,
/// `documents_only_at_ignores_an_uncommitted_declaration`,
/// `documents_only_at_is_false_for_an_undeclared_or_unreadable_checkout`.
pub fn documents_only_at(root: &Path) -> bool {
    declared_documents_only_at_rev(root, "HEAD") == Some(true)
}

/// Would committing the staged index CHANGE the `documents_only` declaration
/// (#7905)?
///
/// Why (#7905 review, CRITICAL 2): `.trusty-mpm.toml` is configuration, so the
/// ADR-0049 commit gate read `git add .trusty-mpm.toml && git commit` as an
/// ordinary documents commit. That landed the declaration at `HEAD` from the
/// main checkout in one command, after which [`documents_only_at`] answered
/// `true` and every source commit in that tree read as documents. Closing
/// CRITICAL 1 alone would not have helped: the two holes each reopen the other,
/// so the declaration has to be BOTH committed to count and source-class to
/// commit.
///
/// What: compares the `documents_only` value the index would land against the
/// one `HEAD` already carries, both read through
/// [`declared_documents_only_at_rev`] — the same lens [`documents_only_at`]
/// uses, so the gate cannot disagree with the grant about what a revision
/// declares. `true` when they differ, in EITHER direction: introducing the key,
/// flipping it, and retracting it are all changes to a security-relevant
/// declaration and all belong in a reviewed pull request.
///
/// It fails CLOSED by construction rather than by a carve-out. A blob that will
/// not parse, a path absent at a revision, and git being unavailable each read
/// as `None` — "this revision grants nothing" — so the only way to answer
/// "unchanged" is for both sides to be readable and equal. A staged edit that
/// leaves the key alone is therefore still an ordinary documents commit.
/// Test: `staged_declaration_change_is_detected_in_both_directions`,
/// `staged_declaration_edit_that_leaves_the_key_alone_is_not_a_change`.
pub fn staged_declaration_changes_documents_only(root: &Path) -> bool {
    // `:<path>` is the INDEX copy — what this commit would actually land.
    declared_documents_only_at_rev(root, "") != declared_documents_only_at_rev(root, "HEAD")
}

/// The `documents_only` value the project declares at one git revision (#7905).
///
/// Why: the single lens both #7905 rules look through, so "what does this
/// revision declare" has one answer built one way.
/// What: `git show <rev>:.trusty-mpm.toml` at `root`, parsed with the same
/// `deny_unknown_fields` reader every other caller uses. `rev` is `"HEAD"` for
/// the committed declaration and `""` for the index copy (`git show
/// :.trusty-mpm.toml`). `None` for every failure — no such path, unborn `HEAD`,
/// git unavailable, a blob that does not parse — and `None` for a file that
/// parses without the key, which are the same thing to both callers: this
/// revision grants nothing.
/// Test: `documents_only_at_reads_a_committed_declaration`,
/// `staged_declaration_change_is_detected_in_both_directions`.
fn declared_documents_only_at_rev(root: &Path, rev: &str) -> Option<bool> {
    let spec = format!("{rev}:{PROJECT_CONFIG_FILE}");
    // The hardened `git` invocation every guard path shares: ambient config
    // cannot steer which blob a security boundary reads.
    let raw = crate::session_manager::worktree_safety::git_stdout(root, &["show", &spec]).ok()?;
    ProjectLevelConfig::from_toml(&raw, &root.join(PROJECT_CONFIG_FILE))
        .ok()?
        .documents_only
}

#[cfg(test)]
#[path = "project_config_tests.rs"]
mod tests;
