//! trusty-mpm's `~/.trusty-tools/trusty-mpm/config.yaml` settings + workspace-root
//! resolution (#1220).
//!
//! Why: #1220 introduces two related conventions for trusty-mpm. (1) the
//! cross-crate config convention `~/.trusty-tools/<crate>/config.yaml` — trusty-mpm
//! reads its slice (`~/.trusty-tools/trusty-mpm/config.yaml`) for the workspace-root
//! template, the auto-resume default, and the default model; (2) the managed-session
//! workspace-root default moves from `~/.trusty-mpm/workspaces/<project>/<id>/` to
//! `~/trusty-mpm-projects/<owner>/<repo>/<id>/`, deriving `<owner>/<repo>` from the
//! target repo's GitHub remote. Keeping the typed config AND the root resolver in one
//! module keeps the precedence rules (env > config > built-in default) in a single
//! tested place and well under the 500-SLOC cap.
//!
//! What: [`TrustyToolsConfig`] is the YAML-deserialised settings struct
//! (`workspace_root_template`, `auto_resume`, `default_model`), loaded via
//! [`TrustyToolsConfig::load`] (delegates to `trusty_common::crate_config`).
//! [`workspace_root`] resolves the absolute root for managed-session workspaces with
//! precedence **`TRUSTY_MPM_WORKSPACE_ROOT` env > config template > built-in default
//! (`~/trusty-mpm-projects`)**, expanding a leading `~`. [`workspace_subpath`] joins
//! the `<owner>/<repo>` identity onto that root.
//!
//! Test: `default_template_is_trusty_mpm_projects`, `env_overrides_config_and_default`,
//! `config_template_used_when_no_env`, `tilde_expansion`, `subpath_nests_owner_repo`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use trusty_common::github_path::GithubPath;

/// Untracked/secret file sync config shape + resolution (#2196), split into
/// its own module to keep this file under the 500-SLOC production cap. See
/// that module's doc for the full rationale.
pub mod untracked_sync;
pub use untracked_sync::{
    DEFAULT_UNTRACKED_SYNC_PATTERNS, ResolvedUntrackedSync, UntrackedSyncConfig,
    resolve_untracked_sync,
};

/// Crate name used as the `~/.trusty-tools/<crate>/` directory segment.
///
/// Why: the cross-crate convention keys each crate's config dir by its crate name;
/// a constant keeps trusty-mpm's segment from drifting.
/// What: `"trusty-mpm"`.
/// Test: `config_dir_is_trusty_mpm` indirectly via `crate_config` path tests.
pub const CRATE_NAME: &str = "trusty-mpm";

/// Sub-directory of `~/.trusty-tools/trusty-mpm/` that holds the shared,
/// tm-owned `CLAUDE_CONFIG_DIR` for every DAEMON-managed session (DOC-34).
///
/// Why: managed sessions must NOT read the target project's committed `.claude/`
/// (which ships a partial/broken agent roster and shadows the framework set,
/// #1996). Pointing the launched `claude` at a tm-owned config home under the
/// shared `~/.trusty-tools` base — the same base the #1220 crate-config
/// convention already uses — segregates the framework agents/skills/hooks/mcp
/// from whatever the project happens to commit. Naming the segment once keeps
/// the accessor and any future migration in agreement.
/// What: `"claude-config"`, joined onto `~/.trusty-tools/trusty-mpm/`.
/// Test: `managed_claude_config_dir_nests_under_trusty_tools`.
pub const MANAGED_CLAUDE_CONFIG_SUBDIR: &str = "claude-config";

/// Built-in default workspace-root directory name under `$HOME` (#1220).
///
/// Why: #1220 fixes the new default at `~/trusty-mpm-projects/`. Naming it once
/// keeps the resolver and the migration scan in agreement.
/// What: `"trusty-mpm-projects"`.
/// Test: `default_template_is_trusty_mpm_projects`.
pub const DEFAULT_WORKSPACE_DIR: &str = "trusty-mpm-projects";

/// Environment variable that overrides the resolved workspace root.
///
/// Why: operators (and tests) need an escape hatch that wins over the config file,
/// matching the pre-#1220 behaviour where this env var alone selected the root.
/// What: `"TRUSTY_MPM_WORKSPACE_ROOT"`.
/// Test: `env_overrides_config_and_default`.
pub const WORKSPACE_ROOT_ENV: &str = "TRUSTY_MPM_WORKSPACE_ROOT";

/// trusty-mpm's slice of the `~/.trusty-tools/<crate>/config.yaml` convention.
///
/// Why: gives operators a single declarative file for the settings #1220 calls out
/// — the workspace-root template, the auto-resume default, and the default model —
/// instead of relying solely on env vars or the legacy `~/.trusty-mpm/config.toml`
/// (which stays for its existing agent/model sections; this is the NEW, additive
/// YAML surface).
/// What: every field is optional so an absent file or absent key falls back to the
/// built-in default. `workspace_root_template` may contain a leading `~`.
/// Test: `config_template_used_when_no_env`, the `crate_config` round-trip tests.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustyToolsConfig {
    /// Template directory for managed-session workspace roots.
    ///
    /// `None` → the built-in default `~/trusty-mpm-projects`. A leading `~` is
    /// expanded to the home directory. Sessions nest as
    /// `<this>/<owner>/<repo>/<session-id>/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root_template: Option<String>,

    /// Default supervisor auto-resume preference surfaced in the console Config UI.
    ///
    /// `None` → unset (the supervisor's own default / persisted `auto_resume` file
    /// wins). This field lets the convention express the preference declaratively.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resume: Option<bool>,

    /// Default model id (or tier alias) for launched sessions.
    ///
    /// `None` → fall back to the existing `~/.trusty-mpm/config.toml` model
    /// resolution. Present here so the console Config UI can edit it via the
    /// #1220 convention without touching the legacy TOML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,

    /// `tm watch` defaults (the `watch:` YAML section).
    ///
    /// `None` → no configured defaults; the watch CLI flags supply their own
    /// built-in defaults. Present so an operator can pin a board's repo, routing
    /// label, and poll interval declaratively instead of typing them every run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<WatchConfig>,

    /// Per-project GitHub identity binding (the `github:` YAML section, #1265).
    ///
    /// `None` → no binding; every `gh` call inherits the ambient gh identity
    /// (current behaviour, no regression). When present, the resolved overrides
    /// are applied to every `gh` subprocess `tm` spawns for the active project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GithubConfig>,

    /// Statically-declared projects for the project registry (#1519 MR-1).
    ///
    /// `None` / absent → no static projects; the registry starts empty and is
    /// populated by MCP `project_register` calls and boot auto-registration from
    /// session history. When present, each entry is upserted into the registry
    /// at daemon startup, so an operator can declare frequently-used repos once
    /// in config rather than registering them via MCP on every restart.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<ProjectConfig>,

    /// Daemon-level safety toggles (the `daemon:` YAML section, #1836).
    ///
    /// `None` → every toggle defaults to its safe value (MCP-initiated spawning
    /// stays disabled).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon: Option<DaemonConfig>,

    /// Global allowlisted untracked/secret file sync settings (the
    /// `untracked_sync:` YAML section, #2196).
    ///
    /// `None` → the built-in default applies: sync is ENABLED with the
    /// built-in `.env*` pattern set (see [`resolve_untracked_sync`]). A
    /// per-project `untracked_sync` override (on [`ProjectConfig`]) takes
    /// precedence over this global section for that project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untracked_sync: Option<UntrackedSyncConfig>,

    /// Managed-tmux scrollback + mouse ergonomics (the `tmux:` YAML section,
    /// #2398).
    ///
    /// `None` → the built-in defaults apply: a 100,000-line `history-limit`,
    /// `mouse on`, and `alternate-screen on`. See [`resolve_tmux_options`] for
    /// the full precedence and [`TmuxConfig`] for the field-level docs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux: Option<TmuxConfig>,
}

/// The `daemon:` section of `~/.trusty-tools/trusty-mpm/config.yaml` (#1836).
///
/// Why: the ARIA incident showed that an MCP-triggered `session_new` call can
/// silently provision real infrastructure (clone + worktree + tmux + harness)
/// for any repo an LLM caller names, with no operator confirmation. The fix is
/// an explicit, declarative opt-in an operator sets once — rather than a
/// per-call flag a careless/compromised caller could always supply.
/// What: currently one toggle, `allow_mcp_spawn`; resolution precedence (env >
/// config > safe default) lives in
/// [`crate::daemon::managed_routes::mcp_spawn_gate::mcp_spawn_enabled`], not
/// here — this is purely the on-disk shape.
/// Test: `daemon_config_yaml_round_trip`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Whether MCP-initiated session spawns (`session_new` and friends) are
    /// permitted to provision new infrastructure.
    ///
    /// `None`/`Some(false)` → disabled (the safe default, #1836): an MCP
    /// caller's `session_new` is refused before any workspace is touched.
    /// `Some(true)` → opts in to MCP-initiated spawning (still subject to the
    /// #1837 registry allowlist gate). The `tm` CLI's own spawn path (`tm
    /// launch`/`tm connect`/`tm ticket`) is NEVER gated by this flag — it
    /// always spawns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_mcp_spawn: Option<bool>,
}

/// Built-in default tmux `history-limit`/`mouse` (#2398) and
/// `alternate-screen` (#5151), now the single shared source of truth in
/// `trusty_common::tmux` (#3004) so trusty-agents' independent tmux
/// implementation uses the identical defaults. Re-exported here so every
/// existing reference in this module keeps compiling unchanged.
/// Test: `tmux_options_default_when_no_config`.
pub use trusty_common::tmux::{
    DEFAULT_TMUX_ALTERNATE_SCREEN, DEFAULT_TMUX_HISTORY_LIMIT, DEFAULT_TMUX_MOUSE,
};

/// Minimum tmux `history-limit` accepted from config (#2398 QA finding).
///
/// Why: `history-limit 0` does NOT mean "unlimited" in tmux — it means "keep
/// no scrollback beyond the visible screen", the exact opposite of what an
/// operator setting `tmux.history_limit: 0` almost certainly intended (this
/// is a common mental-model mismatch with tools where `0` conventionally
/// means unbounded). Rather than silently honouring a self-defeating value
/// that reintroduces the unscrollable-session problem #2398 exists to fix, a
/// configured value below this floor is clamped UP to it.
/// What: `1_000`. Deliberately below [`DEFAULT_TMUX_HISTORY_LIMIT`] so an
/// operator who intentionally wants a SMALL (but still usable) scrollback
/// can still dial it down — only the pathological `0` (or another
/// near-zero value) is corrected.
/// Test: `tmux_options_clamps_zero_history_limit`,
/// `tmux_options_clamps_below_minimum`.
pub const MIN_TMUX_HISTORY_LIMIT: u32 = 1_000;

/// The `tmux:` section of `~/.trusty-tools/trusty-mpm/config.yaml` (#2398,
/// extended by #5151).
///
/// Why: managed sessions inherited tmux's tiny default `history-limit`
/// (2000 lines), making long PM sessions unscrollable. Every field is
/// optional so an absent section (or a partly-filled one) falls back to the
/// built-in defaults — no regression for existing configs.
/// What: `history_limit` (scrollback lines retained per pane), `mouse`
/// (server-wide wheel-scroll/copy-mode toggle), and `alternate_screen`
/// (whether panes may use the terminal alternate screen buffer). Resolution
/// precedence (config > built-in default, clamped to
/// [`MIN_TMUX_HISTORY_LIMIT`]) lives in [`resolve_tmux_options`], not here —
/// this is purely the on-disk shape.
/// Test: `tmux_config_yaml_round_trip`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TmuxConfig {
    /// Scrollback lines retained per pane (tmux `history-limit`).
    ///
    /// `None` → [`DEFAULT_TMUX_HISTORY_LIMIT`]. A configured value below
    /// [`MIN_TMUX_HISTORY_LIMIT`] (including `0`, which tmux treats as "no
    /// scrollback", not "unlimited") is clamped up to it — see
    /// [`resolve_tmux_options`]. Only affects panes created AFTER this is
    /// applied — tmux captures `history-limit` into a pane's ring buffer at
    /// creation time, so an already-running pane's scrollback is never
    /// retroactively grown (a tmux limitation, not a trusty-mpm one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_limit: Option<u32>,

    /// Whether mouse-wheel scrolling / copy-mode is enabled (tmux `mouse`).
    ///
    /// `None` → [`DEFAULT_TMUX_MOUSE`] (`true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouse: Option<bool>,

    /// Whether programs in a pane may use the terminal alternate screen
    /// buffer (tmux `alternate-screen`). `None` →
    /// [`DEFAULT_TMUX_ALTERNATE_SCREEN`] (`true`), which is tmux's own
    /// factory default — set `alternate_screen: false` to turn it off (#5151).
    ///
    /// **Why you would set this to `false`.** A full-screen TUI (Claude Code
    /// included) draws into the alternate screen, and tmux discards that
    /// buffer instead of appending it to the pane's scrollback. That is why a
    /// `tm` session can hold tens of thousands of lines of `history-limit` and
    /// still show you nothing to scroll back through: the history is not
    /// missing, it was never written to the pane. `false` makes the TUI draw
    /// into the normal buffer, so its output lands in scrollback.
    ///
    /// **What that costs — read before setting it.** The effect is
    /// server-wide, and every trusty-* managed session shares ONE tmux server,
    /// so this changes every pane on it, not just the ones running an agent.
    /// `vim`, `less`, `htop` and `man` all stop restoring the screen they
    /// covered: each leaves its final frame behind, smeared into the
    /// scrollback on exit, sometimes with redraw garbage from partial
    /// repaints. It is also not retroactive — anything already written while
    /// the alternate screen was up stays unrecoverable, and only panes created
    /// after the option lands are affected at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternate_screen: Option<bool>,
}

/// Resolved tmux scrollback + mouse ergonomics, after applying config
/// overrides on top of the built-in defaults (#2398).
///
/// Why: [`crate::daemon::tmux::TmuxDriver::apply_scrollback_options`] needs
/// concrete `u32`/`bool` values, not the optional on-disk shape; resolving
/// once here keeps the precedence logic in a single tested place.
/// What: `history_limit`, `mouse` and `alternate_screen`, all always
/// populated.
/// Test: `tmux_options_default_when_no_config`, `tmux_options_config_override`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedTmuxOptions {
    /// Scrollback lines retained per pane.
    pub history_limit: u32,
    /// Whether mouse-wheel scrolling / copy-mode is enabled.
    pub mouse: bool,
    /// Whether panes may use the terminal alternate screen buffer (#5151).
    /// See [`TmuxConfig::alternate_screen`] for what turning it off costs.
    pub alternate_screen: bool,
}

/// Resolve the effective tmux scrollback + mouse options for managed
/// sessions (#2398).
///
/// Why: gives every tmux-session-creating call site (normal spawn, resume/
/// restart, GUI spawn-mode, config-restart, and the CLI/TUI paths that create
/// sessions directly) a single answer, so a future config change cannot land
/// in one call site and be forgotten in another.
/// What: applies precedence **config override > built-in default**
/// ([`DEFAULT_TMUX_HISTORY_LIMIT`] / [`DEFAULT_TMUX_MOUSE`] /
/// [`DEFAULT_TMUX_ALTERNATE_SCREEN`]) for each field independently, then
/// clamps `history_limit` up to [`MIN_TMUX_HISTORY_LIMIT`] — a configured `0`
/// (or any other near-zero value) does NOT mean "unlimited" in tmux, so it is
/// corrected rather than honoured verbatim (#2398 QA finding).
/// `alternate_screen` is NOT clamped or corrected: both values are legitimate
/// and the default matches tmux's own, so an absent config leaves the
/// operator's terminal behaving exactly as it does today (#5151).
/// Test: `tmux_options_default_when_no_config`, `tmux_options_config_override`,
/// `tmux_options_clamps_zero_history_limit`, `tmux_options_clamps_below_minimum`,
/// `tmux_options_alternate_screen_defaults_on`,
/// `tmux_options_alternate_screen_config_override`.
pub fn resolve_tmux_options(config: &TrustyToolsConfig) -> ResolvedTmuxOptions {
    let section = config.tmux.as_ref();
    let history_limit = section
        .and_then(|t| t.history_limit)
        .unwrap_or(DEFAULT_TMUX_HISTORY_LIMIT)
        .max(MIN_TMUX_HISTORY_LIMIT);
    ResolvedTmuxOptions {
        history_limit,
        mouse: section.and_then(|t| t.mouse).unwrap_or(DEFAULT_TMUX_MOUSE),
        // #5151: opt-in knob — absent config keeps tmux's factory `on`.
        alternate_screen: section
            .and_then(|t| t.alternate_screen)
            .unwrap_or(DEFAULT_TMUX_ALTERNATE_SCREEN),
    }
}

/// The `github:` section of `~/.trusty-tools/trusty-mpm/config.yaml` (#1265).
///
/// Why: a host may have several GitHub identities (a personal account, a work
/// account, a bot, a GitHub Enterprise host) and `tm`'s many `gh` calls must use
/// the RIGHT one for the active project rather than whichever identity happens to
/// be ambient. Binding the identity per-project — alongside the `watch:` section
/// that already keys defaults to the active board — lets an operator pin
/// "this project's gh actions use THIS account/token/host" declaratively. Every
/// field is optional so an absent section (or a partly-filled one) cleanly falls
/// back to the ambient gh identity, guaranteeing no regression for existing setups.
/// What: optional `config_dir` (a private gh config home), `token_env` (the NAME
/// of an env var or keychain reference to resolve a token from at call time —
/// never a plaintext token), `account` (a gh username), and `host` (the gh host,
/// default `github.com`, also used for clone-URL synthesis). The resolution
/// precedence and the env overrides each field maps to live in the
/// `gh_identity` module, not here — this is purely the on-disk shape.
/// Test: round-trips via the `crate_config` tests; resolution in `gh_identity`;
/// the no-plaintext-token guarantee is asserted by `github_config_stores_only_env_name`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubConfig {
    /// A private gh config home directory → exported as `GH_CONFIG_DIR`.
    ///
    /// `None` → not set. When set, `gh` reads its auth/config from this directory
    /// instead of the user's default `~/.config/gh`, isolating the identity
    /// WITHOUT mutating any global gh state. This is the highest-precedence,
    /// least-invasive strategy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<PathBuf>,

    /// The NAME of an env var to resolve a token from at runtime → `GH_TOKEN`.
    ///
    /// `None` → not set. This is intentionally the env-var NAME, never a token:
    /// the plaintext secret never lives in the config file or this struct. At
    /// call time `std::env::var(<this name>)` resolves the actual token; if the
    /// named var is absent the strategy is skipped (precedence falls through).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,

    /// A gh username to select as the active account.
    ///
    /// `None` → not set. The least-precise strategy: `gh` has no universal
    /// per-invocation `--user` flag, so binding by account alone has caveats
    /// (documented in `gh_identity`). Prefer `config_dir` or `token_env`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,

    /// The gh host (e.g. `github.example.com` for GitHub Enterprise).
    ///
    /// `None` → the built-in default `github.com`. When set it is exported as
    /// `GH_HOST` (independent of the identity strategy) AND used to synthesise
    /// clone URLs as `https://<host>/<owner>/<repo>` (closes the GHE part of
    /// #1261).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// Built-in default GitHub host used for clone-URL synthesis and `GH_HOST`.
///
/// Why: when no `github.host` is configured, clone URLs and gh fall back to the
/// public host. Naming it once keeps the resolver and the URL synthesiser aligned.
/// What: `"github.com"`.
/// Test: `default_host_is_github_com` in `gh_identity`.
pub const DEFAULT_GITHUB_HOST: &str = "github.com";

/// The `watch:` section of `~/.trusty-tools/trusty-mpm/config.yaml`.
///
/// Why: `tm watch poll|listen` routes board issues to managed sessions; an
/// operator running it repeatedly (or from cron) wants to pin the board repo,
/// the routing label, and the poll interval once rather than passing them on
/// every invocation. Every field is optional so an absent section or key falls
/// back to the CLI flag's built-in default; CLI flags always override config.
/// What: optional `repo` (`owner/repo`), `label` (routing label), and
/// `interval_secs` (listen-mode poll cadence). Resolution precedence lives in
/// the `watch::args` module, not here — this is purely the on-disk shape.
/// Test: round-trips via the `crate_config` tests; precedence in `watch::args`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchConfig {
    /// Default board repository as `owner/repo` (e.g. `bobmatnyc/trusty-tools`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,

    /// Default routing label; only issues carrying it are picked up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Default poll interval (seconds) for `listen` mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_secs: Option<u64>,
}

/// A single project entry declared in the `projects:` YAML section (#1519 MR-1).
///
/// Why: operators who use a fixed set of repos want to declare them once in
/// `config.yaml` so the project registry is pre-populated on daemon startup
/// without requiring MCP `project_register` calls. Every field beyond `name`
/// and `repo_url` is optional so a minimal two-line entry is valid.
/// What: on-disk shape for one project declaration. The daemon reads these at
/// startup and upserts them into the `ProjectRegistry` via `seed_from_config`.
/// Test: `registry_seed_from_config` in the `project::registry` module.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// Short name used as the registry key (e.g. `trusty-tools`).
    pub name: String,

    /// Full repository URL (e.g. `https://github.com/owner/trusty-tools`).
    pub repo_url: String,

    /// Default branch; falls back to `"main"` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,

    /// Optional technology-stack hint (e.g. `"rust"`, `"python"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_hint: Option<String>,

    /// Classification tags (e.g. `["backend", "production"]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,

    /// Free-form description of the project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Preferred GitHub account login for `gh` operations on this project (#2081).
    ///
    /// `None` → no preference declared; `gh_user` is left unset on the seeded
    /// [`crate::project::record::Project`], so `gh` calls for this project use
    /// whatever identity is ambient. See `Project::gh_user` for the full
    /// rationale and the non-mutating resolution mechanism
    /// ([`crate::core::gh_account::resolve_gh_account_env`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gh_user: Option<String>,

    /// GitHub account login pinned for this project's spawned sessions (#3025).
    /// `None` → no pinning (no regression). `Some(login)` → mirrored onto the
    /// seeded [`crate::project::record::Project::gh_account`] by
    /// `seed_from_config`; resolved into `GH_TOKEN`/`GH_USER` spawn-env
    /// overrides via [`crate::core::gh_account::resolve_gh_account_env`].
    /// Test: `project_config_gh_user_yaml_round_trip` (extended).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gh_account: Option<String>,

    /// Per-project `gh`-CLI identity binding (#2184), overriding the global
    /// `github:` section for this project only.
    ///
    /// Why: #1265's `github:` section is a single global binding, but a host
    /// working on several projects may need a DIFFERENT `gh` identity per
    /// project (e.g. a personal account for an OSS repo, a work account for a
    /// client repo). This is a SEPARATE concern from `gh_user` above: `gh_user`
    /// only documents a preferred login for the non-mutating
    /// `ensure_gh_account_for_project` enforcement path (#2081), while `github`
    /// carries the full `config_dir`/`token_env`/`account`/`host` binding that
    /// [`crate::core::gh_identity::resolve_gh_env`] resolves into concrete env
    /// overrides for every `gh` subprocess `tm` spawns for this project.
    /// What: `None` → this project has no override; resolution falls back to
    /// the global `github:` section, then to the ambient gh identity (no
    /// regression). `Some(cfg)` → `cfg` is resolved in place of the global
    /// section for every `gh` call scoped to this project. Mirrored onto the
    /// seeded [`crate::project::record::Project::github`] by `seed_from_config`.
    /// Test: `project_config_github_and_commit_identity_yaml_round_trip`;
    /// resolution precedence in `core::gh_identity`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GithubConfig>,

    /// Preferred git commit author name for commits made on behalf of this
    /// project (#2184).
    ///
    /// Why: `tm`'s managed/provisioner git operations previously never set a
    /// commit identity, inheriting whatever `user.name` happened to be
    /// globally configured (or none, causing a hard commit failure). A
    /// per-project override lets an operator pin the right author for
    /// projects that require a specific bot/service identity, independent of
    /// the `gh`-CLI identity above (commit authorship and `gh` API auth are
    /// distinct git/GitHub concerns).
    /// What: `None` → no override; git uses whatever `user.name` is already
    /// configured (no regression). `Some(name)` → applied as `-c
    /// user.name=<name>` on every git invocation the provisioner makes for
    /// this project. Mirrored onto
    /// [`crate::project::record::Project::commit_name`] by `seed_from_config`.
    /// Test: `project_config_github_and_commit_identity_yaml_round_trip`;
    /// applied via `core::git_identity::GitIdentity::commit_config_args`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_name: Option<String>,

    /// Preferred git commit author email for commits made on behalf of this
    /// project (#2184). See [`Self::commit_name`] for the full rationale;
    /// applied as `-c user.email=<email>` alongside it.
    /// Test: `project_config_github_and_commit_identity_yaml_round_trip`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_email: Option<String>,

    /// Per-project untracked/secret file sync override (#2196), overriding
    /// the global `untracked_sync:` section for this project only.
    ///
    /// `None` → no override; resolution falls through to the global section,
    /// then the built-in default. See [`resolve_untracked_sync`] for the
    /// full precedence chain.
    /// Test: `untracked_sync_config_yaml_round_trip`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untracked_sync: Option<UntrackedSyncConfig>,

    /// Per-project "launch on main" opt-out (#3455), mirrored onto
    /// [`crate::project::record::Project::worktree`] by `seed_from_config`.
    ///
    /// `None`/`Some(true)` → default, no regression: this project's managed
    /// sessions each get their own `.worktrees/<session>` git worktree.
    /// `Some(false)` → sessions run directly in the resolved local checkout
    /// instead — no clone, no worktree, no dedicated branch. See
    /// [`crate::project::record::Project::worktree_enabled`] for the
    /// resolved default and `tm projects config <name> set worktree
    /// true|false` for the runtime-registry equivalent (the registry, not
    /// this static config, is what `spawn_managed_routed` actually consults
    /// at spawn time — this field only seeds it).
    /// Test: covered by `registry::seed_from_config` round-trip coverage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<bool>,
}

impl TrustyToolsConfig {
    /// Load trusty-mpm's config from `~/.trusty-tools/trusty-mpm/config.yaml`.
    ///
    /// Why: the daemon and CLI read this once at startup; collapsing absent /
    /// home-unknown / malformed to defaults (with a logged warning for malformed)
    /// means a bad file never aborts startup.
    /// What: delegates to [`trusty_common::crate_config::load_or_default`] for
    /// [`CRATE_NAME`].
    /// Test: `config_template_used_when_no_env` (round-trips via `crate_config`).
    pub fn load() -> Self {
        trusty_common::crate_config::load_or_default::<Self>(CRATE_NAME)
    }
}

/// Resolve the shared tm-owned `CLAUDE_CONFIG_DIR` for daemon-managed sessions.
///
/// Why: the daemon managed-spawn path must launch `claude` with a
/// `CLAUDE_CONFIG_DIR` that supplies the framework agent roster, skills, hooks,
/// and MCP servers from a tm-controlled location — NOT from the target
/// project's committed `.claude/` and NOT from the operator's `~/.claude`
/// (DOC-34 / #1996). This accessor is the single source of truth for that path
/// so the provisioner and the spawn-command builder can never disagree.
/// What: `~/.trusty-tools/trusty-mpm/claude-config/`, delegating to
/// [`trusty_common::crate_config::crate_config_dir`] for the `~/.trusty-tools/<crate>`
/// base and joining [`MANAGED_CLAUDE_CONFIG_SUBDIR`]. Returns `None` only when
/// the home directory cannot be resolved (a stripped CI environment).
/// Test: `managed_claude_config_dir_nests_under_trusty_tools`.
pub fn managed_claude_config_dir() -> Option<PathBuf> {
    trusty_common::crate_config::crate_config_dir(CRATE_NAME)
        .map(|dir| dir.join(MANAGED_CLAUDE_CONFIG_SUBDIR))
}

/// Resolve the managed `CLAUDE_CONFIG_DIR` under an explicit base (hermetic).
///
/// Why: the production accessor calls `dirs::home_dir()`, which unit tests must
/// not depend on. Pointing `base` at a `tempfile::TempDir` keeps the layout
/// assertion hermetic and parallel-safe.
/// What: `<base>/.trusty-tools/trusty-mpm/claude-config/`.
/// Test: `managed_claude_config_dir_nests_under_trusty_tools`.
pub fn managed_claude_config_dir_at(base: &Path) -> PathBuf {
    trusty_common::crate_config::crate_config_dir_at(base, CRATE_NAME)
        .join(MANAGED_CLAUDE_CONFIG_SUBDIR)
}

/// Expand a leading `~` in a path template to the home directory.
///
/// Why: config templates and the built-in default are written home-relative
/// (`~/trusty-mpm-projects`); the resolver must turn them into absolute paths.
/// What: replaces a leading `~` (optionally `~/`) with `home`; other paths pass
/// through unchanged. Absolute paths and templates without `~` are returned as-is.
/// Test: `tilde_expansion`.
fn expand_tilde(template: &str, home: &Path) -> PathBuf {
    if let Some(rest) = template.strip_prefix("~/") {
        home.join(rest)
    } else if template == "~" {
        home.to_path_buf()
    } else {
        PathBuf::from(template)
    }
}

/// Resolve the absolute workspace root for managed sessions.
///
/// Why: the spawn path needs ONE answer for "where do session workspaces live?"
/// with #1220's new default and an env/config override, in a tested place so the
/// HTTP route and the MCP tool cannot diverge.
/// What: applies the precedence **`TRUSTY_MPM_WORKSPACE_ROOT` env > config
/// `workspace_root_template` > built-in `~/trusty-mpm-projects`**, expanding a
/// leading `~`. Falls back to `/tmp/trusty-mpm-projects` only when the home
/// directory is unresolvable AND nothing absolute was supplied.
/// Test: `default_template_is_trusty_mpm_projects`, `env_overrides_config_and_default`,
/// `config_template_used_when_no_env`.
pub fn workspace_root(config: &TrustyToolsConfig) -> PathBuf {
    let home = dirs::home_dir();

    // 1. Env override wins (back-compat with the pre-#1220 behaviour).
    if let Ok(raw) = std::env::var(WORKSPACE_ROOT_ENV) {
        let raw = raw.trim();
        if !raw.is_empty() {
            return match &home {
                Some(h) => expand_tilde(raw, h),
                None => PathBuf::from(raw),
            };
        }
    }

    // 2. Config template (from ~/.trusty-tools/trusty-mpm/config.yaml).
    if let Some(template) = config
        .workspace_root_template
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return match &home {
            Some(h) => expand_tilde(template, h),
            None => PathBuf::from(template),
        };
    }

    // 3. Built-in default: ~/trusty-mpm-projects.
    match home {
        Some(h) => h.join(DEFAULT_WORKSPACE_DIR),
        None => PathBuf::from("/tmp").join(DEFAULT_WORKSPACE_DIR),
    }
}

/// Join a project's `<owner>/<repo>` identity onto the workspace root.
///
/// Why: #1220 nests sessions under `<root>/<owner>/<repo>/` so multiple sessions
/// on the same repo group together and the path mirrors the GitHub identity.
/// What: returns `<root>/<owner>/<repo>` for the given [`GithubPath`]. The session
/// id is appended by the provisioner, not here, so this stays the "project home".
/// Test: `subpath_nests_owner_repo`.
pub fn workspace_subpath(config: &TrustyToolsConfig, gh: &GithubPath) -> PathBuf {
    workspace_root(config).join(&gh.owner).join(&gh.repo)
}

/// Process-global lock serialising every test that mutates the workspace-root /
/// repos-root environment variables.
///
/// Why: `cargo test` runs the whole crate's tests in parallel threads sharing one
/// process env. Both this module's env tests AND
/// `crate::daemon::managed_routes::inproject`'s `TRUSTY_MPM_REPOS_ROOT` test mutate
/// `TRUSTY_MPM_WORKSPACE_ROOT` / `TRUSTY_MPM_REPOS_ROOT`; without a SHARED lock a
/// per-module mutex lets them race (one test's `remove_var` clobbers another's
/// `set_var`), causing flaky failures. Exposing ONE crate-wide lock guarantees
/// mutual exclusion across modules.
/// What: returns a guard over a single `static` mutex; poisoning is recovered so a
/// panicking test cannot deadlock the rest.
/// Test: used by the env-precedence tests here and in `inproject`.
#[cfg(test)]
pub(crate) fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
#[path = "trusty_tools_config_tests.rs"]
mod trusty_tools_config_tests;
