//! MCP-initiated spawn gate: off-by-default + registry allowlist (#1836, #1837).
//!
//! Why: the ARIA incident (#1836) showed that an LLM-driven MCP `session_new`
//! call can mint real infrastructure — a git clone, a tmux session, a running
//! Claude harness — for ANY `repo_url` the caller supplies, with zero operator
//! confirmation. A PM agent working in one repo asked for a session against an
//! entirely unrelated repo and the daemon obliged, creating dozens of orphan
//! worktrees under `~/trusty-mpm-projects/duettoresearch/aria/.worktrees/`. The
//! blast radius has two independent dimensions the tickets separate: (1)
//! MCP-initiated spawning must be OFF by default (#1836) so a careless or
//! misbehaving LLM call can never provision anything without an explicit
//! operator opt-in, and (2) even opted in, the target repo must already be a
//! KNOWN project — registered via `project_register` or `config.yaml`'s
//! `projects:` list — so a session in repo A cannot spin up infrastructure for
//! unrelated repo B (#1837). The `tm` CLI's own `tm launch`/`tm connect`/`tm
//! ticket` paths are NEVER subject to either gate — only the MCP `session_new`
//! tool (via [`super::lifecycle::spawn_managed`]) calls this.
//! What: [`mcp_spawn_enabled`] resolves the #1836 toggle (env > config >
//! default `false`); [`is_known_repo`] is the pure #1837 allowlist predicate —
//! for a remote-looking `repo_url` it requires a STRICT `(owner, repo)`
//! identity match (never a bare repo-name match, which would be spoofable by
//! an attacker-controlled host/owner), and for a local filesystem path it
//! requires the CANONICAL FULL PATH to sit at or under a registered project's
//! own canonical root (#7066 — a basename match let any directory named like a
//! registered project satisfy the allowlist), with a DEGENERATE root (`/`, the
//! operator's home, the workspace root) refused at match time so that an
//! ungated `project_register` call cannot widen the allowlist to the host;
//! [`ensure_mcp_spawn_allowed`] is the async orchestration `spawn_managed` calls
//! FIRST — before any session id, workspace, or record is created — so a
//! refusal has zero side effects. It takes the registry and config as plain
//! references (not `Arc<DaemonState>`) so it is testable against a bare
//! `ProjectRegistry` with no daemon state, session manager, or tmux involved.
//! Test: `tests` covers both gate layers offline (env/config precedence, name
//! and URL matching, and the composed disposition); the wiring into
//! `spawn_managed` is covered by `tests/mcp_spawn_gate.rs` (proves a rejected
//! MCP spawn creates nothing and that CLI-origin spawns are never gated).

use std::path::{Path, PathBuf};

use tracing::warn;
use trusty_common::github_path::parse_github_path;

use crate::core::trusty_tools_config::{TrustyToolsConfig, workspace_root, workspace_subpath};
use crate::project::{Project, ProjectRegistry, derive_name_from_url};

/// Fewest path components a registered root may have and still bound a project
/// (#7066).
///
/// Why: the same "too shallow to be a project" floor
/// `session_manager::workspace_guard::is_safe_to_remove` already applies before
/// it will delete anything — counted from the filesystem root, so `/` is 1 and
/// `/Users` is 2. Reusing that number keeps one notion of a degenerate path in
/// this crate instead of two.
/// What: `3`. Test: `tests::a_degenerate_registered_root_is_never_a_containment_root`.
const MIN_ROOT_COMPONENTS: usize = 3;

/// Env var that force-enables MCP-initiated spawning, overriding config (#1836).
///
/// Why: an explicit, highest-precedence escape hatch mirrors the existing
/// `TRUSTY_MPM_WORKSPACE_ROOT` convention — operators (and tests) can flip the
/// gate without editing `config.yaml`.
/// What: `"TRUSTY_MPM_ALLOW_MCP_SPAWN"`; a truthy value (`"1"`/`"true"`/`"yes"`,
/// case-insensitive) force-enables regardless of config; an explicit falsy
/// value (`"0"`/`"false"`/`"no"`, case-insensitive) force-disables regardless
/// of config; unset or empty defers entirely to config.
/// Test: `tests::env_true_enables_regardless_of_config`,
/// `tests::env_false_string_force_disables_regardless_of_config`,
/// `tests::env_unset_defers_to_config`.
pub const ALLOW_MCP_SPAWN_ENV: &str = "TRUSTY_MPM_ALLOW_MCP_SPAWN";

/// Resolve whether MCP-initiated session spawning is currently permitted (#1836).
///
/// Why: the spawn path needs ONE answer, with the same env > config > default
/// precedence pattern [`crate::core::trusty_tools_config::workspace_root`]
/// already establishes, so the gate cannot silently diverge between callers.
/// An operator setting `TRUSTY_MPM_ALLOW_MCP_SPAWN=0` expects that to act as a
/// deliberate, explicit override (force-disable) — not a silent no-op that
/// falls through to whatever config says.
/// What: three-state precedence. `TRUSTY_MPM_ALLOW_MCP_SPAWN` set to a truthy
/// string (`"1"`/`"true"`/`"yes"`) force-enables; set to a falsy string
/// (`"0"`/`"false"`/`"no"`) force-disables; unset, empty, or any other value
/// defers to `config.daemon.allow_mcp_spawn`, defaulting to `false` if config
/// is also silent.
/// Test: `tests::default_is_disabled`, `tests::config_true_enables`,
/// `tests::env_true_enables_regardless_of_config`,
/// `tests::env_false_string_force_disables_regardless_of_config`,
/// `tests::env_unset_defers_to_config`.
#[must_use]
pub fn mcp_spawn_enabled(config: &TrustyToolsConfig) -> bool {
    if let Ok(raw) = std::env::var(ALLOW_MCP_SPAWN_ENV) {
        let raw = raw.trim().to_ascii_lowercase();
        match raw.as_str() {
            "1" | "true" | "yes" => return true,
            "0" | "false" | "no" => return false,
            _ => {} // empty or unrecognised value: defer to config
        }
    }
    config
        .daemon
        .as_ref()
        .and_then(|d| d.allow_mcp_spawn)
        .unwrap_or(false)
}

/// Whether `s` looks like a remote repo reference (URL or SSH shorthand)
/// rather than a bare local filesystem path.
///
/// Why: [`is_known_repo`] must apply STRICT owner/repo identity matching to
/// anything that looks like a remote reference — a bare last-path-segment
/// name match is spoofable (an attacker can name their fork's repo identically
/// to a legitimate one, e.g. `https://evil.example.com/attacker/trusty-tools`
/// vs a registered `owner/trusty-tools`). A genuine local filesystem path
/// (the in-project spawn convenience case, e.g. `/Users/op/checkouts/aria`)
/// carries no such identity and needs the looser name-based fallback instead.
/// What: `true` iff `s` contains `://` (any URL scheme) or a bare `:` (the SSH
/// shorthand `user@host:owner/repo`) — mirrors the exact heuristic
/// [`crate::project::record::derive_name_from_url`] already uses to detect the
/// SSH form. A local absolute path never contains `:` on the supported
/// platforms (macOS/Linux), so this cannot misclassify one.
/// Test: `tests::looks_like_remote_url_*`.
fn looks_like_remote_url(s: &str) -> bool {
    s.contains("://") || s.contains(':')
}

/// The canonical local root a registered project occupies on this host (#7066).
///
/// Why: [`is_known_repo`]'s local arm compares full paths, so it needs each
/// registered project's OWN path, canonicalized exactly as the target is. A
/// project reaches the registry in one of two shapes: its `repo_url` is the
/// local checkout itself — ADR-0055 decision B, "the error must tell the
/// operator to clone the repository first and pass the local path", enforced by
/// [`crate::core::local_repo_url::require_local_repo_url`] — or it is a remote
/// URL, in which case the daemon's provisioning home for that project is
/// `<workspace_root>/<owner>/<repo>` ([`workspace_subpath`], #1220).
/// Both are full paths; neither is a basename.
/// What: `Some(root)` with symlinks resolved and `..` normalised, or `None`
/// when the `repo_url` is a remote URL that does not parse as `(owner, repo)`,
/// or when the resulting path cannot be canonicalized (missing, permission
/// denied). A project with no resolvable root on this host contributes NOTHING
/// to the allowlist — every failure arm refuses.
/// Test: `tests::local_path_allowlist_decides_on_the_canonical_full_path`,
/// `tests::a_remote_registered_project_is_known_at_its_workspace_checkout`,
/// `tests::a_degenerate_registered_root_is_never_a_containment_root`.
fn registered_local_root(project: &Project, config: &TrustyToolsConfig) -> Option<PathBuf> {
    let declared = if looks_like_remote_url(&project.repo_url) {
        workspace_subpath(config, &parse_github_path(&project.repo_url)?)
    } else {
        PathBuf::from(&project.repo_url)
    };
    // `canonicalize` resolves symlinks and `..` AND fails on a path that does
    // not exist, so an unresolvable root simply yields no match.
    let root = std::fs::canonicalize(declared).ok()?;
    // #7066: a degenerate root contains every path on the host, so refuse it
    // here rather than trusting the registration that produced it.
    if let Some(reason) = degenerate_root_reason(&root, config) {
        warn!(
            project = %project.name,
            root = %root.display(),
            "MCP spawn gate: ignoring registered project root — {reason}"
        );
        return None;
    }
    Some(root)
}

/// Why the canonical `root` is too broad to bound the spawn allowlist (#7066).
///
/// Why: `project_register` is an ungated MCP tool that takes any string as
/// `repo_url`, so a caller can register a project rooted at `/`, at the
/// operator's home directory, or at the daemon's own workspace root.
/// [`is_known_repo`]'s local arm admits any target CONTAINED by a registered
/// root, and containment under such a root holds for every existing directory
/// on the host — a single degenerate registration would turn the allowlist into
/// an allow-everything, which is the ARIA-incident shape the gate exists to
/// stop. The floor is enforced HERE, at match time, and NOT in
/// `project_register`, so a bad registration already sitting in the registry —
/// written by an older build, by hand, or by a caller that never passed a
/// registration-time check — still cannot widen the allowlist.
/// What: `Some(reason)` when `root` has fewer than [`MIN_ROOT_COMPONENTS`]
/// components (`/`, `/Users`), or IS the operator's home directory, or IS the
/// workspace root the daemon provisions every project under
/// ([`workspace_root`]). The last two need naming explicitly because the
/// component floor does not reach them — `/Users/op` is already three
/// components. Comparison canonicalizes both sides and falls back to a literal
/// comparison when a side cannot be canonicalized, so a home or workspace root
/// reached through a symlink still matches. `None` means the root bounds a real
/// project directory.
/// Test: `tests::a_degenerate_registered_root_is_never_a_containment_root`.
fn degenerate_root_reason(root: &Path, config: &TrustyToolsConfig) -> Option<&'static str> {
    if root.components().count() < MIN_ROOT_COMPONENTS {
        return Some("too few path components to be a project checkout");
    }
    if dirs::home_dir().is_some_and(|home| same_path(&home, root)) {
        return Some("it is the operator's home directory");
    }
    if same_path(&workspace_root(config), root) {
        return Some("it is the daemon's workspace root");
    }
    None
}

/// Whether two paths name the same directory, resolving symlinks when possible.
///
/// Why: [`degenerate_root_reason`] compares an already-canonical root against
/// `$HOME` and the workspace root, neither of which is canonical and either of
/// which may not exist (an unprovisioned workspace root is the normal case).
/// What: canonicalizes both and compares; when either side cannot be
/// canonicalized, compares the paths literally.
/// Test: `tests::a_degenerate_registered_root_is_never_a_containment_root`.
fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Whether `repo_url` matches an already-registered project (#1837).
///
/// Why: the second gate layer — even with MCP spawning enabled, a repo the
/// operator has never registered must be refused, and the match must NOT be
/// spoofable by an attacker-controlled URL. When `repo_url` looks like a
/// remote reference, identity is decided STRICTLY by the parsed
/// `(owner, repo)` pair (case-insensitive) against each registered project's
/// OWN `repo_url` parsed the same way — matching only the trailing repo name
/// (as an earlier revision did) would let `https://evil.example.com/attacker/
/// trusty-tools` impersonate a registered `owner/trusty-tools`, reproducing
/// the exact ARIA-incident shape through the allowlist itself. Only when
/// `repo_url` carries no such identity (a bare local filesystem path) does
/// this reach the local arm — and since #7066 that arm decides on the
/// CANONICAL FULL PATH, not the basename. Before #7066 it compared the path's
/// last segment against each registered project's `name`, so ANY directory
/// named like a registered project — `/tmp/evil/trusty-tools` against a
/// registered `trusty-tools` — satisfied the allowlist. ADR-0055 made that the
/// SOLE rule an MCP-initiated spawn hits, because `session_new` now refuses
/// every non-local `repo_url` before the gate runs.
/// What: for a remote-looking `repo_url`, `true` iff some registered project's
/// `repo_url` is ALSO remote-looking and parses to the same `(owner, repo)`.
/// For a local-path `repo_url`, `true` iff its canonical path IS, or is
/// contained by, some registered project's canonical root
/// ([`registered_local_root`]). Containment — not equality alone — is what
/// admits a session against a worktree of a registered checkout (`.worktrees/`,
/// `.claude/worktrees/<x>`); neither ADR-0055 nor #7066 fixes a worktree rule,
/// and a path inside a registered project's own directory is exactly what the
/// operator registered. Comparison is by path COMPONENT, so a sibling whose
/// name merely starts with a registered root's name is not contained by it.
/// A DEGENERATE registered root — `/`, the operator's home, the workspace root,
/// anything shallower than [`MIN_ROOT_COMPONENTS`] — bounds nothing and is
/// dropped by [`degenerate_root_reason`] before containment is tested, because
/// `project_register` accepts any string as a `repo_url` and containment under
/// such a root holds for the whole host. That floor is applied at MATCH time,
/// not at registration, so a bad registration cannot widen the allowlist.
/// Every error arm refuses: a target that cannot be canonicalized (missing,
/// permission denied) is NOT known.
/// Test: `tests::is_known_repo_matches_by_owner_and_repo_ignoring_git_suffix`,
/// `tests::is_known_repo_matches_ssh_and_https_forms`,
/// `tests::is_known_repo_rejects_same_repo_name_different_owner`,
/// `tests::local_path_allowlist_decides_on_the_canonical_full_path`,
/// `tests::a_remote_registered_project_is_known_at_its_workspace_checkout`,
/// `tests::a_degenerate_registered_root_is_never_a_containment_root`,
/// `tests::is_known_repo_rejects_unregistered`,
/// `tests::is_known_repo_remote_target_ignores_local_registered_url`.
#[must_use]
pub fn is_known_repo(projects: &[Project], config: &TrustyToolsConfig, repo_url: &str) -> bool {
    if looks_like_remote_url(repo_url) {
        let Some(target) = parse_github_path(repo_url) else {
            return false;
        };
        return projects.iter().any(|p| {
            looks_like_remote_url(&p.repo_url)
                && parse_github_path(&p.repo_url).is_some_and(|gh| {
                    gh.owner.eq_ignore_ascii_case(&target.owner)
                        && gh.repo.eq_ignore_ascii_case(&target.repo)
                })
        });
    }

    // #7066: a local path is decided on its canonical full path, never its
    // basename. A failed canonicalization refuses rather than falling through.
    let Ok(target) = std::fs::canonicalize(repo_url) else {
        return false;
    };
    projects
        .iter()
        .any(|p| registered_local_root(p, config).is_some_and(|root| target.starts_with(root)))
}

/// Enforce the two-layer MCP spawn gate before any provisioning begins
/// (#1836, #1837).
///
/// Why: [`super::lifecycle::spawn_managed`] calls this FIRST — before
/// `ManagedSessionId::new()` — so a refusal is a pure, side-effect-free `Err`:
/// no id minted, no workspace touched, no tmux session created. This is the
/// single seam both tickets share, and taking `&ProjectRegistry` /
/// `&TrustyToolsConfig` directly (rather than `&Arc<DaemonState>`) keeps it
/// testable without a daemon.
/// What: (1) if MCP spawning is disabled (the default), returns an actionable
/// `Err` naming both the config key and the env var; (2) otherwise consults
/// the project registry — an unregistered `repo_url` is refused with a message
/// naming the exact `project_register` call to run. A registered project
/// passes silently (`Ok(())`).
/// Test: `tests::ensure_mcp_spawn_allowed_disabled_by_default`,
/// `tests::ensure_mcp_spawn_allowed_enabled_but_unregistered`,
/// `tests::ensure_mcp_spawn_allowed_enabled_and_registered`.
pub async fn ensure_mcp_spawn_allowed(
    registry: &ProjectRegistry,
    config: &TrustyToolsConfig,
    repo_url: &str,
) -> Result<(), String> {
    if !mcp_spawn_enabled(config) {
        return Err(format!(
            "managed spawning via MCP is disabled; enable it with `allow_mcp_spawn: true` \
             under `daemon:` in ~/.trusty-tools/trusty-mpm/config.yaml, or set \
             {ALLOW_MCP_SPAWN_ENV}=1 — or run `tm launch` directly from the CLI, which is \
             never gated"
        ));
    }

    let projects = registry
        .list()
        .await
        .map_err(|e| format!("failed to read project registry: {e}"))?;
    if !is_known_repo(&projects, config, repo_url) {
        let name_hint = derive_name_from_url(repo_url).unwrap_or_else(|| repo_url.to_string());
        return Err(format!(
            "refusing MCP-initiated spawn for unregistered repo `{repo_url}`; register it \
             first with the `project_register` MCP tool (name=\"{name_hint}\", \
             repo_url=\"{repo_url}\") or add it under `projects:` in \
             ~/.trusty-tools/trusty-mpm/config.yaml, then retry"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::trusty_tools_config::DaemonConfig;
    use serial_test::serial;
    use tempfile::TempDir;

    /// RAII guard that sets (or removes) one environment variable for the
    /// duration of a `#[serial]` test, restoring the prior value (or absence)
    /// on drop — panic-safe, so a failed assertion mid-test cannot leak the
    /// override into a sibling test (mirrors `session_launch::tests::EnvVarGuard`).
    /// The no-key constructors act on [`ALLOW_MCP_SPAWN_ENV`], this module's
    /// most-overridden variable; `set_key`/`unset_key` take any other.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(value: &str) -> Self {
            Self::set_key(ALLOW_MCP_SPAWN_ENV, value)
        }

        fn unset() -> Self {
            Self::unset_key(ALLOW_MCP_SPAWN_ENV)
        }

        fn set_key(key: &'static str, value: &str) -> Self {
            let prev = std::env::var(key).ok();
            // SAFETY: env-mutating tests using this guard are tagged `#[serial]`.
            unsafe { std::env::set_var(key, value) };
            Self { key, prev }
        }

        fn unset_key(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            // SAFETY: env-mutating tests using this guard are tagged `#[serial]`.
            unsafe { std::env::remove_var(key) };
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see `set_key`/`unset_key` — serialized by `#[serial]`.
            unsafe {
                match self.prev.take() {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    fn make_project(name: &str, repo_url: &str) -> Project {
        Project {
            name: name.to_string(),
            repo_url: repo_url.to_string(),
            default_branch: "main".into(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            worktree: None,
        }
    }

    // ── mcp_spawn_enabled precedence ───────────────────────────────────────

    #[test]
    #[serial]
    fn default_is_disabled() {
        let _env = EnvGuard::unset();
        assert!(!mcp_spawn_enabled(&TrustyToolsConfig::default()));
    }

    #[test]
    #[serial]
    fn config_true_enables() {
        let _env = EnvGuard::unset();
        let cfg = TrustyToolsConfig {
            daemon: Some(DaemonConfig {
                allow_mcp_spawn: Some(true),
            }),
            ..Default::default()
        };
        assert!(mcp_spawn_enabled(&cfg));
    }

    #[test]
    #[serial]
    fn env_true_enables_regardless_of_config() {
        let _env = EnvGuard::set("1");
        assert!(
            mcp_spawn_enabled(&TrustyToolsConfig::default()),
            "env=1 must enable even with no config set"
        );
    }

    #[test]
    #[serial]
    fn env_false_string_force_disables_regardless_of_config() {
        // An explicit falsy env value is a deliberate override, not a
        // "no-op" — it must force-disable even when config says `true`.
        let _env = EnvGuard::set("0");
        let cfg = TrustyToolsConfig {
            daemon: Some(DaemonConfig {
                allow_mcp_spawn: Some(true),
            }),
            ..Default::default()
        };
        assert!(
            !mcp_spawn_enabled(&cfg),
            "an explicit env=0 must force-disable even when config=true"
        );
    }

    #[test]
    #[serial]
    fn env_unset_defers_to_config() {
        // With the env var absent entirely, config is authoritative — this is
        // genuine deferral, distinct from the env=0 force-disable case above.
        let _env = EnvGuard::unset();
        let cfg = TrustyToolsConfig {
            daemon: Some(DaemonConfig {
                allow_mcp_spawn: Some(true),
            }),
            ..Default::default()
        };
        assert!(
            mcp_spawn_enabled(&cfg),
            "env unset must defer to config=true"
        );
    }

    #[test]
    #[serial]
    fn env_empty_string_defers_to_config() {
        // An empty (but set) env value is treated the same as unset: defer.
        let _env = EnvGuard::set("");
        let cfg = TrustyToolsConfig {
            daemon: Some(DaemonConfig {
                allow_mcp_spawn: Some(true),
            }),
            ..Default::default()
        };
        assert!(mcp_spawn_enabled(&cfg), "env='' must defer to config=true");
    }

    // ── looks_like_remote_url classification ────────────────────────────────

    #[test]
    fn looks_like_remote_url_detects_https_and_ssh() {
        assert!(looks_like_remote_url("https://github.com/owner/repo"));
        assert!(looks_like_remote_url("git@github.com:owner/repo.git"));
    }

    #[test]
    fn looks_like_remote_url_rejects_local_absolute_path() {
        assert!(!looks_like_remote_url("/Users/op/checkouts/aria"));
    }

    // ── is_known_repo matching ──────────────────────────────────────────────

    /// #7066: the local-path arm decides on the CANONICAL FULL PATH of a
    /// registered project, never the basename.
    ///
    /// Before this fix the arm compared the target's last path segment against
    /// each registered project's `name`, so `/tmp/evil/trusty-tools` was
    /// "known" merely for being named `trusty-tools` — and ADR-0055 left that
    /// as the ONLY allowlist rule an MCP-initiated spawn reaches. The table
    /// covers the shapes the basename rule got wrong alongside the ones the
    /// path rule must keep admitting.
    #[test]
    fn local_path_allowlist_decides_on_the_canonical_full_path() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        let registered = root.join("checkouts").join("trusty-tools");
        let worktree = registered.join(".claude").join("worktrees").join("agent-x");
        let sibling = root.join("checkouts").join("trusty-tools-evil");
        let elsewhere = root.join("evil").join("trusty-tools");
        for dir in [&worktree, &sibling, &elsewhere] {
            std::fs::create_dir_all(dir).expect("create fixture dir");
        }
        let link = root.join("link-to-checkout");
        std::os::unix::fs::symlink(&registered, &link).expect("symlink the registered checkout");

        let projects = vec![make_project("trusty-tools", &registered.to_string_lossy())];
        let cfg = TrustyToolsConfig::default();

        let cases: Vec<(&str, PathBuf, bool)> = vec![
            ("the registered checkout itself", registered.clone(), true),
            ("a symlink to the registered checkout", link, true),
            ("a worktree inside the registered checkout", worktree, true),
            (
                "a same-basename directory elsewhere on disk",
                elsewhere,
                false,
            ),
            (
                "a sibling whose name merely starts with the root's",
                sibling,
                false,
            ),
            (
                "a path that does not exist",
                registered.join("missing"),
                false,
            ),
            (
                "a `..` escape out of the registered checkout",
                registered.join("..").join("trusty-tools-evil"),
                false,
            ),
        ];

        for (what, path, expected) in cases {
            assert_eq!(
                is_known_repo(&projects, &cfg, &path.to_string_lossy()),
                expected,
                "{what}: {}",
                path.display()
            );
        }
    }

    /// #7066: a project registered by its REMOTE url is known at the daemon's
    /// own provisioning home for it — `<workspace_root>/<owner>/<repo>` (#1220)
    /// — and at no other directory of the same name.
    #[test]
    fn a_remote_registered_project_is_known_at_its_workspace_checkout() {
        let _g = crate::core::trusty_tools_config::env_test_lock();
        // This test exercises the CONFIG template, which the env var would
        // otherwise override. The guard restores whatever the caller had, so a
        // developer running with `TRUSTY_MPM_WORKSPACE_ROOT` set does not lose
        // it for every later test in this binary.
        let _env = EnvGuard::unset_key(trusty_common::workspace_layout::WORKSPACE_ROOT_ENV);

        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().join("trusty-mpm-projects");
        let checkout = workspace.join("bobmatnyc").join("trusty-tools");
        let impostor = tmp.path().join("elsewhere").join("trusty-tools");
        for dir in [&checkout, &impostor] {
            std::fs::create_dir_all(dir).expect("create fixture dir");
        }

        let projects = vec![make_project(
            "trusty-tools",
            "https://github.com/bobmatnyc/trusty-tools",
        )];
        let cfg = TrustyToolsConfig {
            workspace_root_template: Some(workspace.to_string_lossy().into_owned()),
            ..Default::default()
        };

        assert!(
            is_known_repo(&projects, &cfg, &checkout.to_string_lossy()),
            "the project's own workspace checkout must be known"
        );
        assert!(
            !is_known_repo(&projects, &cfg, &impostor.to_string_lossy()),
            "a same-name directory outside the workspace checkout must not be"
        );
    }

    /// #7066 (review round 2): a registered root broad enough to contain the
    /// whole host must never bound the allowlist.
    ///
    /// `project_register` is ungated and takes any string as `repo_url`, so one
    /// registration at `/` or at the operator's home would make
    /// `target.starts_with(root)` true for every existing directory — the
    /// containment rule's match arm, not a failure arm. Both degenerate roots
    /// are registered alongside a real one, so the same run also proves the
    /// floor did not cost a genuine project its worktrees.
    #[test]
    #[serial]
    fn a_degenerate_registered_root_is_never_a_containment_root() {
        let tmp = TempDir::new().expect("tempdir");
        let home = tmp.path().join("home").join("op");
        let unrelated = home.join("checkouts").join("aria");
        let real = tmp.path().join("checkouts").join("trusty-tools");
        let real_worktree = real.join(".worktrees").join("agent-x");
        for dir in [&unrelated, &real_worktree] {
            std::fs::create_dir_all(dir).expect("create fixture dir");
        }
        // `degenerate_root_reason` reads `$HOME` through `dirs::home_dir`, so
        // the simulated home must be the process's for the duration.
        let _home = EnvGuard::set_key("HOME", &home.to_string_lossy());

        let projects = vec![
            make_project("filesystem-root", "/"),
            make_project("home-dir", &home.to_string_lossy()),
            make_project("trusty-tools", &real.to_string_lossy()),
        ];
        let cfg = TrustyToolsConfig::default();

        assert!(
            !is_known_repo(&projects, &cfg, &unrelated.to_string_lossy()),
            "a path outside every real project must not be known via `/` or $HOME"
        );
        assert!(
            is_known_repo(&projects, &cfg, &real_worktree.to_string_lossy()),
            "a worktree of a genuinely registered checkout must stay known"
        );
    }

    #[test]
    fn is_known_repo_matches_by_owner_and_repo_ignoring_git_suffix() {
        let projects = vec![make_project(
            "trusty-tools",
            "https://github.com/bobmatnyc/trusty-tools",
        )];
        assert!(is_known_repo(
            &projects,
            &TrustyToolsConfig::default(),
            "https://github.com/bobmatnyc/trusty-tools.git"
        ));
    }

    /// SSH and HTTPS forms of the SAME owner/repo must match (a bonus of the
    /// owner/repo-identity matcher over a raw string comparison).
    #[test]
    fn is_known_repo_matches_ssh_and_https_forms() {
        let projects = vec![make_project(
            "trusty-tools",
            "https://github.com/bobmatnyc/trusty-tools",
        )];
        assert!(is_known_repo(
            &projects,
            &TrustyToolsConfig::default(),
            "git@github.com:bobmatnyc/trusty-tools.git"
        ));
    }

    #[test]
    fn is_known_repo_rejects_unregistered() {
        let projects = vec![make_project(
            "trusty-tools",
            "https://github.com/bobmatnyc/trusty-tools",
        )];
        assert!(!is_known_repo(
            &projects,
            &TrustyToolsConfig::default(),
            "https://github.com/duettoresearch/aria"
        ));
    }

    /// CRITICAL regression (found in review): a bare repo-NAME match would let
    /// an attacker-controlled URL impersonate a registered project merely by
    /// sharing the last path segment, on a completely different host/owner —
    /// reproducing the exact ARIA-incident shape (an arbitrary, LLM-supplied
    /// `repo_url`) through the allowlist itself. Owner+repo identity matching
    /// must reject this.
    #[test]
    fn is_known_repo_rejects_same_repo_name_different_owner() {
        let projects = vec![make_project(
            "trusty-tools",
            "https://github.com/bobmatnyc/trusty-tools",
        )];
        assert!(!is_known_repo(
            &projects,
            &TrustyToolsConfig::default(),
            "https://evil.example.com/attacker/trusty-tools"
        ));
    }

    /// A registered project whose OWN `repo_url` is (unusually) a local path
    /// must never be used to satisfy a remote-looking target via the owner/repo
    /// matcher — the `looks_like_remote_url` guard on the registry side must
    /// hold.
    #[test]
    fn is_known_repo_remote_target_ignores_local_registered_url() {
        let projects = vec![make_project("weird", "/some/local/path/repo")];
        assert!(!is_known_repo(
            &projects,
            &TrustyToolsConfig::default(),
            "https://github.com/some/repo"
        ));
    }

    // ── ensure_mcp_spawn_allowed composed behaviour ─────────────────────────

    #[tokio::test]
    #[serial]
    async fn ensure_mcp_spawn_allowed_disabled_by_default() {
        let _env = EnvGuard::unset();
        let dir = TempDir::new().expect("tempdir");
        let registry = ProjectRegistry::load(dir.path()).await.expect("load");
        let cfg = TrustyToolsConfig::default();

        let err =
            ensure_mcp_spawn_allowed(&registry, &cfg, "https://github.com/duettoresearch/aria")
                .await
                .expect_err("must refuse when MCP spawning is disabled by default");
        assert!(err.contains("disabled"), "{err}");
        assert!(err.contains("allow_mcp_spawn"), "{err}");
    }

    #[tokio::test]
    #[serial]
    async fn ensure_mcp_spawn_allowed_enabled_but_unregistered() {
        let _env = EnvGuard::unset();
        let dir = TempDir::new().expect("tempdir");
        let registry = ProjectRegistry::load(dir.path()).await.expect("load");
        let cfg = TrustyToolsConfig {
            daemon: Some(DaemonConfig {
                allow_mcp_spawn: Some(true),
            }),
            ..Default::default()
        };

        let err =
            ensure_mcp_spawn_allowed(&registry, &cfg, "https://github.com/duettoresearch/aria")
                .await
                .expect_err("must refuse an unregistered repo even when spawning is enabled");
        assert!(err.contains("unregistered"), "{err}");
        assert!(err.contains("project_register"), "{err}");
    }

    #[tokio::test]
    #[serial]
    async fn ensure_mcp_spawn_allowed_enabled_and_registered() {
        let _env = EnvGuard::unset();
        let dir = TempDir::new().expect("tempdir");
        let registry = ProjectRegistry::load(dir.path()).await.expect("load");
        registry
            .register(make_project(
                "trusty-tools",
                "https://github.com/bobmatnyc/trusty-tools",
            ))
            .await
            .expect("register");
        let cfg = TrustyToolsConfig {
            daemon: Some(DaemonConfig {
                allow_mcp_spawn: Some(true),
            }),
            ..Default::default()
        };

        ensure_mcp_spawn_allowed(
            &registry,
            &cfg,
            "https://github.com/bobmatnyc/trusty-tools.git",
        )
        .await
        .expect("an already-registered project must spawn without extra ceremony");
    }
}
