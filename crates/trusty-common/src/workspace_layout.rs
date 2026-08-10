//! The one resolver for trusty-mpm's managed workspace layout (#5203, #5204).
//!
//! Why: the managed workspace root and the per-repo worktree base name are
//! shared across four crates that cannot depend on `trusty-mpm` — `trusty-code`
//! browses the workspace to build its project picker, `trusty-search` excludes
//! session worktrees from auto-discovery, and `trusty-memory` derives
//! `creator:workstream=` tags from a worktree path segment. Each had hardcoded
//! its own literal (`~/trusty-mpm-projects`, `.worktrees`), so an operator who
//! retargeted either value via `TRUSTY_MPM_WORKSPACE_ROOT` or
//! `~/.trusty-tools/trusty-mpm/config.yaml` got silent disagreement: a picker
//! reporting live checkouts as missing, an indexer registering every throwaway
//! worktree, and workstream attribution that stopped resolving. Per CLAUDE.md's
//! "Common entry point, clean domain demarcation", the capability gets exactly
//! one implementation, here in the crate all four already depend on.
//!
//! What: [`resolve_workspace_root`] and [`resolve_worktrees_dirname`] hold the
//! precedence chains (**env > config > built-in default**); [`workspace_root`]
//! and [`worktrees_dirname`] are the zero-argument forms that load
//! [`WorkspaceLayoutConfig`] from trusty-mpm's `config.yaml` themselves.
//! [`WorktreeDirNames`] resolves once and matches many times, for scan paths
//! that must not re-read the config per directory entry.
//!
//! **Creation resolves one name; detection matches a superset.** A configured
//! base retargets where new worktrees are *created*, but every detection site
//! ([`WorktreeDirNames::matches`], [`is_worktrees_dirname`]) also keeps matching
//! the built-in `.worktrees`, so worktrees already on disk when the operator
//! retargets stay excluded from indexing, stay prunable, and keep resolving
//! their workstream tag. Narrowing detection to the configured name alone would
//! silently orphan them.
//!
//! **Both settings are HOST-level, not project-level, and that is deliberate.**
//! The owner ruling (2026-08-08) puts configuration in the committed
//! project-level file (`trusty_mpm::core::project_config`), with
//! `workspace_root` recorded there as the explicit carve-out: it decides where a
//! project gets CLONED, so it cannot be read from the project that does not
//! exist yet. `worktrees_dirname` is carved out for the SAME structural reason,
//! one step removed: the detection sites are handed an opaque path and must
//! decide whether a segment is a worktree base — but locating the project whose
//! config would answer that requires already knowing which segment is the base.
//! `trusty-memory`'s `resolve_workstream_name` is the clearest case: it scans a
//! caller-reported cwd string for the marker, and the marker is what identifies
//! the project root. Reading the answer from the project would be circular, so
//! the value lives beside `workspace_root` in the host config both are reachable
//! from without a project in hand.
//!
//! That argument covers detection only. The CREATION sites
//! (`inproject::worktree_path_for`, `provisioner::workspace::provision_in`) DO
//! hold the project checkout and could read its `.trusty-mpm.toml` — but a base
//! that creation and detection disagree about is worse than either choice alone:
//! worktrees would be created somewhere the remover, the pruner, the indexer,
//! and the attribution scan do not look. Since detection cannot be
//! project-scoped, creation must not be either.
//!
//! Scope: this module governs trusty-mpm's OWN session provisioning base
//! (`<repo>/.worktrees/<session-id>`, owner ruling 2026-08-05 — `.base` is
//! retired and nothing writes it). It is deliberately unrelated to
//! `.claude/worktrees/`, the Claude Code agent worktree root (ADR-0020,
//! ADR-0036), which stays a fixed literal at its own call sites. The two must
//! not be collapsed: retargeting tm's session base must never move, hide, or
//! reclassify an agent worktree.
//!
//! Test: `default_root_is_trusty_mpm_projects`, `env_overrides_config_and_default`,
//! `config_template_used_when_no_env`, `config_file_drives_both_zero_argument_resolvers`,
//! `tilde_expansion`, `default_worktrees_dirname`,
//! `configured_worktrees_dirname_is_honoured`,
//! `invalid_worktrees_dirname_falls_back_to_default`,
//! `reserved_worktrees_dirname_falls_back_to_default`,
//! `reserved_name_via_env_cannot_claim_the_claude_agent_store`,
//! `detection_matches_configured_and_builtin`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The `~/.trusty-tools/<crate>/` segment holding the config this module reads.
pub const MPM_CRATE_NAME: &str = "trusty-mpm";

/// Built-in default workspace-root directory name under `$HOME` (#1220).
pub const DEFAULT_WORKSPACE_DIR: &str = "trusty-mpm-projects";

/// Environment variable that overrides the resolved workspace root.
pub const WORKSPACE_ROOT_ENV: &str = "TRUSTY_MPM_WORKSPACE_ROOT";

/// Built-in default worktree base directory name under a project checkout.
pub const DEFAULT_WORKTREES_DIRNAME: &str = ".worktrees";

/// Environment variable that overrides the resolved worktree base name.
pub const WORKTREES_DIRNAME_ENV: &str = "TRUSTY_MPM_WORKTREES_DIRNAME";

/// The slice of `~/.trusty-tools/trusty-mpm/config.yaml` this resolver reads.
///
/// Why: `trusty-code`, `trusty-search`, and `trusty-memory` have no dependency
/// on `trusty-mpm`, so they cannot name `TrustyToolsConfig`. This narrow shape
/// lets them read the SAME two keys from the SAME file through the SAME
/// precedence chain. `TrustyToolsConfig` carries both fields too and delegates
/// its resolution here, so there is one precedence chain, not two.
/// What: both fields optional; every field absent means "built-in defaults".
/// Deserialisation is lenient (no `deny_unknown_fields`) because this struct
/// reads only a slice of a file whose other keys belong to trusty-mpm — see
/// `trusty_mpm::core::config_keys` for the typo reporting that covers them.
/// Test: `config_template_used_when_no_env`, `configured_worktrees_dirname_is_honoured`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WorkspaceLayoutConfig {
    /// Template directory for managed-session workspace roots; may lead with `~`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root_template: Option<String>,
    /// Directory name under a project checkout that holds session worktrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktrees_dirname: Option<String>,
}

#[cfg(feature = "crate-config")]
impl WorkspaceLayoutConfig {
    /// Load the layout slice of trusty-mpm's config, falling back to defaults.
    ///
    /// Why: the zero-argument resolvers need the file's values without making
    /// every caller wire up `crate_config` themselves.
    /// What: delegates to [`crate::crate_config::load_or_default`] for
    /// [`MPM_CRATE_NAME`]; an absent, unreadable, or malformed file yields
    /// `Default` (all built-in defaults) rather than an error.
    ///
    /// Feature-gated on `crate-config` because that is what supplies the YAML
    /// reader. A consumer that wants the zero-argument resolvers must enable it
    /// — deliberately a COMPILE error rather than a silent fallback to defaults,
    /// which would reintroduce exactly the "config quietly ignored" defect
    /// #5203 exists to fix.
    /// Test: `config_template_used_when_no_env`.
    pub fn load() -> Self {
        crate::crate_config::load_or_default::<Self>(MPM_CRATE_NAME)
    }
}

/// Expand a leading `~` in a path template to the home directory.
///
/// Why: config templates and the built-in default are written home-relative
/// (`~/trusty-mpm-projects`); the resolver must turn them into absolute paths.
/// What: replaces a leading `~` (optionally `~/`) with `home`. Absolute paths
/// and templates without `~` are returned as-is.
/// Test: `tilde_expansion`.
pub fn expand_tilde(template: &str, home: &Path) -> PathBuf {
    if let Some(rest) = template.strip_prefix("~/") {
        home.join(rest)
    } else if template == "~" {
        home.to_path_buf()
    } else {
        PathBuf::from(template)
    }
}

/// Resolve the absolute workspace root, given an already-loaded config template.
///
/// Why: the single precedence chain every consumer routes through. `trusty-mpm`
/// passes its `TrustyToolsConfig.workspace_root_template` here rather than
/// re-implementing the ordering (#5203).
/// What: applies **`TRUSTY_MPM_WORKSPACE_ROOT` env > `template` > built-in
/// `~/trusty-mpm-projects`**, expanding a leading `~`. Falls back to
/// `/tmp/trusty-mpm-projects` only when the home directory is unresolvable AND
/// nothing absolute was supplied.
/// Test: `default_root_is_trusty_mpm_projects`, `env_overrides_config_and_default`,
/// `config_template_used_when_no_env`.
pub fn resolve_workspace_root(template: Option<&str>) -> PathBuf {
    let home = dirs::home_dir();

    let from_env = std::env::var(WORKSPACE_ROOT_ENV)
        .ok()
        .filter(|raw| !raw.trim().is_empty());
    let chosen = from_env
        .as_deref()
        .or(template)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    match (chosen, &home) {
        (Some(raw), Some(h)) => expand_tilde(raw, h),
        (Some(raw), None) => PathBuf::from(raw),
        (None, Some(h)) => h.join(DEFAULT_WORKSPACE_DIR),
        (None, None) => PathBuf::from("/tmp").join(DEFAULT_WORKSPACE_DIR),
    }
}

/// Resolve the absolute workspace root, loading trusty-mpm's config first.
///
/// Why: the entry point for the three crates that have no `TrustyToolsConfig`
/// to hand (#5203) — `trusty-code`'s project picker calls exactly this.
/// What: [`WorkspaceLayoutConfig::load`] then [`resolve_workspace_root`].
/// Test: `config_template_used_when_no_env`.
#[cfg(feature = "crate-config")]
pub fn workspace_root() -> PathBuf {
    resolve_workspace_root(
        WorkspaceLayoutConfig::load()
            .workspace_root_template
            .as_deref(),
    )
}

/// Resolve the worktree base directory name, given an already-loaded config value.
///
/// Why: the single precedence chain for #5204, mirroring
/// [`resolve_workspace_root`] so both knobs behave identically.
/// What: applies **`TRUSTY_MPM_WORKTREES_DIRNAME` env > `configured` > built-in
/// `.worktrees`**. A candidate that is not a single, non-relative path component
/// (empty, contains a separator, or is `.`/`..`) is REJECTED back to the
/// built-in default with a `warn` — a multi-segment value would let
/// `<base>/<name>` escape the checkout it is supposed to nest inside.
/// Test: `default_worktrees_dirname`, `configured_worktrees_dirname_is_honoured`,
/// `invalid_worktrees_dirname_falls_back_to_default`.
pub fn resolve_worktrees_dirname(configured: Option<&str>) -> String {
    let from_env = std::env::var(WORKTREES_DIRNAME_ENV)
        .ok()
        .filter(|raw| !raw.trim().is_empty());
    let Some(candidate) = from_env
        .as_deref()
        .or(configured)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return DEFAULT_WORKTREES_DIRNAME.to_string();
    };

    if !is_single_component(candidate) {
        tracing::warn!(
            candidate,
            default = DEFAULT_WORKTREES_DIRNAME,
            "worktrees_dirname must be a single path component — using the default"
        );
        return DEFAULT_WORKTREES_DIRNAME.to_string();
    }
    if RESERVED_WORKTREES_DIRNAMES.contains(&candidate) {
        tracing::warn!(
            candidate,
            default = DEFAULT_WORKTREES_DIRNAME,
            "worktrees_dirname collides with a reserved directory name — using the default"
        );
        return DEFAULT_WORKTREES_DIRNAME.to_string();
    }
    candidate.to_string()
}

/// Directory names a configured worktree base may never take.
///
/// Why: detection is a superset (configured name OR `.worktrees`), and
/// trusty-mpm's ownership predicate `is_session_worktree` asks only whether a
/// path's PARENT is a worktree base. Configuring the dotless `worktrees` would
/// therefore make `.claude/worktrees/<agent>` — Claude Code's own agent
/// worktree store, out of scope per ADR-0020 — match that predicate, and
/// `worktree_reclaim::tm_provisioned` applies "exactly the predicate the remover
/// applies". The classifier and the remover would then BOTH call an agent
/// worktree tm-owned, with no second opinion left to catch it (#2919 removed
/// that asymmetry deliberately). Rejecting the name is what keeps
/// `worktree_reclaim`'s stated invariant true under every configuration.
/// What: `worktrees` (the `.claude/worktrees` leaf), plus the git, harness, and
/// legacy-base directories a worktree base must never shadow. A reserved value
/// falls back to `.worktrees` with a warning — never to a sanitized variant.
/// Test: `reserved_worktrees_dirname_falls_back_to_default`.
const RESERVED_WORKTREES_DIRNAMES: &[&str] = &["worktrees", ".git", ".claude", ".base"];

/// True iff `name` is one plain path component that cannot escape its parent.
///
/// Why: guards [`resolve_worktrees_dirname`] against a configured value that
/// would turn `<checkout>/<name>` into a path outside the checkout.
/// What: rejects `.`, `..`, and anything containing a path separator (`/` on
/// every platform, plus `\` so a Windows-style value never slips through a
/// Unix build) or a NUL byte.
/// Test: `invalid_worktrees_dirname_falls_back_to_default`.
fn is_single_component(name: &str) -> bool {
    !matches!(name, "." | "..")
        && !name.contains(['/', '\\', '\0'])
        && Path::new(name).components().count() == 1
}

/// Resolve the worktree base directory name, loading trusty-mpm's config first.
///
/// Why: the entry point for a caller that resolves once and does not hold a
/// config (#5204).
/// What: [`WorkspaceLayoutConfig::load`] then [`resolve_worktrees_dirname`].
/// Test: `configured_worktrees_dirname_is_honoured`.
#[cfg(feature = "crate-config")]
pub fn worktrees_dirname() -> String {
    resolve_worktrees_dirname(WorkspaceLayoutConfig::load().worktrees_dirname.as_deref())
}

/// True iff `name` names a worktree base — the configured one OR the built-in.
///
/// Why: the one-shot detection predicate for callers that check a single name
/// and are not in a hot loop (`trusty-memory`'s workstream-tag derivation).
/// Scan paths should hold a [`WorktreeDirNames`] instead so the config file is
/// read once, not once per path component.
/// What: [`worktrees_dirname`] then [`WorktreeDirNames::matches`] — so a
/// retarget never stops recognising worktrees already on disk (see module docs).
/// Test: `detection_matches_configured_and_builtin`.
#[cfg(feature = "crate-config")]
pub fn is_worktrees_dirname(name: &str) -> bool {
    WorktreeDirNames::resolve().matches(name)
}

/// A resolved worktree-base name, matched many times without re-reading config.
///
/// Why: `trusty-search`'s auto-discovery tests one name per directory entry
/// during a recursive scan; resolving inside that loop would read
/// `config.yaml` per entry. Resolving once at the top of a scan and passing
/// this value down keeps the walk allocation- and syscall-free (#5204).
/// What: holds the resolved creation name; [`matches`](Self::matches) accepts
/// it OR [`DEFAULT_WORKTREES_DIRNAME`].
/// Test: `detection_matches_configured_and_builtin`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeDirNames {
    configured: String,
}

impl WorktreeDirNames {
    /// Resolve from env + trusty-mpm's config, once.
    #[cfg(feature = "crate-config")]
    pub fn resolve() -> Self {
        Self {
            configured: worktrees_dirname(),
        }
    }

    /// Resolve from an already-loaded config value (hermetic core of [`Self::resolve`]).
    pub fn from_configured(configured: Option<&str>) -> Self {
        Self {
            configured: resolve_worktrees_dirname(configured),
        }
    }

    /// The single name under which NEW worktrees are created.
    pub fn creation_name(&self) -> &str {
        &self.configured
    }

    /// True iff `name` is the configured base or the built-in `.worktrees`.
    ///
    /// Why: detection is deliberately a superset of creation — see module docs.
    /// What: compares against [`Self::creation_name`] then
    /// [`DEFAULT_WORKTREES_DIRNAME`].
    /// Test: `detection_matches_configured_and_builtin`.
    pub fn matches(&self, name: &str) -> bool {
        name == self.configured || name == DEFAULT_WORKTREES_DIRNAME
    }
}

impl Default for WorktreeDirNames {
    fn default() -> Self {
        Self {
            configured: DEFAULT_WORKTREES_DIRNAME.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Serialise every test that mutates the two layout env vars.
    ///
    /// Why: `cargo test` runs a crate's tests on parallel threads sharing one
    /// process environment, so an unguarded `set_var` in one test is visible to
    /// another mid-assertion.
    /// What: a process-global mutex each env-mutating test locks first.
    /// Test: used by every test below that touches env.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Clear both env vars so a test observes only what it sets.
    fn clear_env() {
        // SAFETY: every env-touching test in this module holds `env_lock()`.
        unsafe {
            std::env::remove_var(WORKSPACE_ROOT_ENV);
            std::env::remove_var(WORKTREES_DIRNAME_ENV);
        }
    }

    /// Why: #1220 fixed the default at `~/trusty-mpm-projects`; #5203's fix must
    /// preserve it exactly when nothing is configured.
    /// What: no env, no template -> `<home>/trusty-mpm-projects`.
    /// Test: this test.
    #[test]
    fn default_root_is_trusty_mpm_projects() {
        let _guard = env_lock();
        clear_env();
        let root = resolve_workspace_root(None);
        assert!(
            root.ends_with(DEFAULT_WORKSPACE_DIR),
            "expected default root to end with {DEFAULT_WORKSPACE_DIR}, got {}",
            root.display()
        );
    }

    /// Why: the env var is the operator's escape hatch and must outrank config.
    /// What: env set + template set -> env wins.
    /// Test: this test.
    #[test]
    fn env_overrides_config_and_default() {
        let _guard = env_lock();
        clear_env();
        // SAFETY: guarded by `env_lock()`.
        unsafe { std::env::set_var(WORKSPACE_ROOT_ENV, "/tmp/from-env") };
        assert_eq!(
            resolve_workspace_root(Some("/tmp/from-config")),
            PathBuf::from("/tmp/from-env")
        );
        clear_env();
    }

    /// Why: #5203's whole point — a CONFIGURED root must be honoured, not the
    /// hardcoded default.
    /// What: no env + template set -> template wins over the built-in default.
    /// Test: this test.
    #[test]
    fn config_template_used_when_no_env() {
        let _guard = env_lock();
        clear_env();
        assert_eq!(
            resolve_workspace_root(Some("/tmp/from-config")),
            PathBuf::from("/tmp/from-config")
        );
    }

    /// Why: templates are written home-relative; the resolver owns the expansion.
    /// What: `~/x` -> `<home>/x`; `~` -> `<home>`; absolute passes through.
    /// Test: this test.
    #[test]
    fn tilde_expansion() {
        let home = Path::new("/home/bob");
        assert_eq!(
            expand_tilde("~/code", home),
            PathBuf::from("/home/bob/code")
        );
        assert_eq!(expand_tilde("~", home), PathBuf::from("/home/bob"));
        assert_eq!(expand_tilde("/abs/path", home), PathBuf::from("/abs/path"));
    }

    /// Why: #5204 must not change behaviour for an operator who configures nothing.
    /// What: no env, no config -> `.worktrees`.
    /// Test: this test.
    #[test]
    fn default_worktrees_dirname() {
        let _guard = env_lock();
        clear_env();
        assert_eq!(resolve_worktrees_dirname(None), DEFAULT_WORKTREES_DIRNAME);
    }

    /// Why: #5204's ask — a configured base must actually be the creation name,
    /// via either precedence step.
    /// What: config value honoured; env outranks it.
    /// Test: this test.
    #[test]
    fn configured_worktrees_dirname_is_honoured() {
        let _guard = env_lock();
        clear_env();
        assert_eq!(resolve_worktrees_dirname(Some(".sessions")), ".sessions");
        // SAFETY: guarded by `env_lock()`.
        unsafe { std::env::set_var(WORKTREES_DIRNAME_ENV, ".from-env") };
        assert_eq!(resolve_worktrees_dirname(Some(".sessions")), ".from-env");
        clear_env();
    }

    /// Why: a multi-segment or dot value would let `<checkout>/<name>` escape the
    /// checkout the worktree must nest inside.
    /// What: every rejected shape falls back to `.worktrees`, never to a
    /// sanitized substitute.
    /// Test: this test.
    #[test]
    fn invalid_worktrees_dirname_falls_back_to_default() {
        let _guard = env_lock();
        clear_env();
        for bad in ["..", ".", "a/b", "../escape", "a\\b", "/abs"] {
            assert_eq!(
                resolve_worktrees_dirname(Some(bad)),
                DEFAULT_WORKTREES_DIRNAME,
                "{bad:?} must be rejected back to the built-in default"
            );
        }
    }

    /// Why: every other test here drives the env var or passes a template
    /// directly, so the CONFIG-FILE leg — the one the changelog advertises —
    /// had no coverage at all: stubbing `WorkspaceLayoutConfig::load()` to
    /// `Self::default()` left the suite fully green. This is the test that
    /// fails when the config read is removed.
    /// What: points `$HOME` at a tempdir, writes a real `config.yaml` at the
    /// canonical `crate_config` location for [`MPM_CRATE_NAME`], and asserts
    /// BOTH zero-argument resolvers read it — covering the crate name, the two
    /// YAML key spellings, and the `crate-config` wiring together.
    ///
    /// Gated on `crate-config` because everything it drives is: `workspace_root`,
    /// `worktrees_dirname`, and `crate_config::crate_config_path_at` all compile
    /// only under that feature, so an ungated test breaks the crate's `lib test`
    /// target for any build that omits it (#5288). The gate does not park the
    /// coverage: CI's `cargo test --workspace` job builds trusty-common with the
    /// union of its dependents' features, and trusty-code / trusty-memory /
    /// trusty-mpm / trusty-search all request `crate-config`.
    /// Test: this test.
    #[cfg(feature = "crate-config")]
    #[test]
    fn config_file_drives_both_zero_argument_resolvers() {
        let _guard = env_lock();
        clear_env();
        let fake_home = tempfile::tempdir().expect("tempdir");
        let cfg_path = crate::crate_config::crate_config_path_at(fake_home.path(), MPM_CRATE_NAME);
        std::fs::create_dir_all(cfg_path.parent().expect("config parent")).expect("mkdir");
        std::fs::write(
            &cfg_path,
            "workspace_root_template: /tmp/from-yaml-root\nworktrees_dirname: .from-yaml-wt\n",
        )
        .expect("write config.yaml");

        let real_home = std::env::var_os("HOME");
        // SAFETY: guarded by `env_lock()`; HOME is restored below.
        unsafe { std::env::set_var("HOME", fake_home.path()) };
        let root = workspace_root();
        let wt = worktrees_dirname();
        // SAFETY: as above — restore before any assertion can unwind.
        unsafe {
            match &real_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }

        assert_eq!(
            root,
            PathBuf::from("/tmp/from-yaml-root"),
            "workspace_root() must read workspace_root_template from {}",
            cfg_path.display()
        );
        assert_eq!(
            wt,
            ".from-yaml-wt",
            "worktrees_dirname() must read worktrees_dirname from {}",
            cfg_path.display()
        );
    }

    /// Why: a dotless `worktrees` would make `.claude/worktrees/<agent>` satisfy
    /// trusty-mpm's `is_session_worktree`, and `worktree_reclaim::tm_provisioned`
    /// applies exactly the remover's predicate — so the classifier AND the
    /// remover would both call a Claude Code agent worktree tm-owned and
    /// deletable (ADR-0020 puts that store out of scope entirely).
    /// What: every reserved name falls back to `.worktrees`, and the built-in
    /// default is never itself rejected.
    /// Test: this test.
    #[test]
    fn reserved_worktrees_dirname_falls_back_to_default() {
        let _guard = env_lock();
        clear_env();
        for reserved in ["worktrees", ".git", ".claude", ".base"] {
            assert_eq!(
                resolve_worktrees_dirname(Some(reserved)),
                DEFAULT_WORKTREES_DIRNAME,
                "{reserved:?} must never become the worktree base"
            );
        }
        // The default must still survive being configured explicitly.
        assert_eq!(
            resolve_worktrees_dirname(Some(DEFAULT_WORKTREES_DIRNAME)),
            DEFAULT_WORKTREES_DIRNAME
        );
    }

    /// Why: the reserved list must hold for the ENV leg too — an operator who
    /// exports `TRUSTY_MPM_WORKTREES_DIRNAME=worktrees` reaches the same
    /// agent-worktree-deletion path as one who writes it into config.
    /// What: the env var carrying a reserved name resolves to the default, and
    /// the detection matcher then refuses to claim a bare `worktrees` segment.
    /// Test: this test.
    #[test]
    fn reserved_name_via_env_cannot_claim_the_claude_agent_store() {
        let _guard = env_lock();
        clear_env();
        // SAFETY: guarded by `env_lock()`.
        unsafe { std::env::set_var(WORKTREES_DIRNAME_ENV, "worktrees") };
        let names = WorktreeDirNames::from_configured(None);
        clear_env();

        assert_eq!(names.creation_name(), DEFAULT_WORKTREES_DIRNAME);
        assert!(
            !names.matches("worktrees"),
            "`.claude/worktrees/<agent>` must never be classified as a tm session worktree"
        );
    }

    /// Why: retargeting the base must not orphan worktrees already on disk —
    /// they stay excluded from indexing and keep resolving workstream tags.
    /// What: `matches` accepts the configured name AND `.worktrees`; creation
    /// reports only the configured one; unrelated names match neither.
    /// Test: this test.
    #[test]
    fn detection_matches_configured_and_builtin() {
        let _guard = env_lock();
        clear_env();
        let names = WorktreeDirNames::from_configured(Some(".sessions"));
        assert_eq!(names.creation_name(), ".sessions");
        assert!(names.matches(".sessions"));
        assert!(names.matches(DEFAULT_WORKTREES_DIRNAME));
        assert!(!names.matches("worktrees"));
        assert!(!names.matches("src"));
        assert_eq!(
            WorktreeDirNames::default().creation_name(),
            DEFAULT_WORKTREES_DIRNAME
        );
    }
}
