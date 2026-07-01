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
//! an attacker-controlled host/owner), falling back to a derived-name match
//! only for genuine local filesystem paths;
//! [`ensure_mcp_spawn_allowed`] is the async orchestration `spawn_managed` calls
//! FIRST — before any session id, workspace, or record is created — so a
//! refusal has zero side effects. It takes the registry and config as plain
//! references (not `Arc<DaemonState>`) so it is testable against a bare
//! `ProjectRegistry` with no daemon state, session manager, or tmux involved.
//! Test: `tests` covers both gate layers offline (env/config precedence, name
//! and URL matching, and the composed disposition); the wiring into
//! `spawn_managed` is covered by `tests/mcp_spawn_gate.rs` (proves a rejected
//! MCP spawn creates nothing and that CLI-origin spawns are never gated).

use trusty_common::github_path::parse_github_path;

use crate::core::trusty_tools_config::TrustyToolsConfig;
use crate::project::{Project, ProjectRegistry, derive_name_from_url};

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
/// this fall back to the looser derived-name match — a URL can never reach
/// that branch, so it cannot be used to impersonate a local checkout.
/// What: for a remote-looking `repo_url`, `true` iff some registered project's
/// `repo_url` is ALSO remote-looking and parses to the same
/// `(owner, repo)`. For a local-path `repo_url`, `true` iff some registered
/// project's `name` equals the path's derived basename.
/// Test: `tests::is_known_repo_matches_by_owner_and_repo_ignoring_git_suffix`,
/// `tests::is_known_repo_matches_ssh_and_https_forms`,
/// `tests::is_known_repo_rejects_same_repo_name_different_owner`,
/// `tests::is_known_repo_matches_by_name`,
/// `tests::is_known_repo_rejects_unregistered`,
/// `tests::is_known_repo_remote_target_ignores_local_registered_url`.
#[must_use]
pub fn is_known_repo(projects: &[Project], repo_url: &str) -> bool {
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

    // Bare local filesystem path — match by derived basename only. This arm
    // is reachable ONLY for inputs `looks_like_remote_url` rejects, so a
    // crafted URL can never ride this fallback to impersonate a project.
    let target_name = derive_name_from_url(repo_url);
    projects
        .iter()
        .any(|p| target_name.as_deref() == Some(p.name.as_str()))
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
    if !is_known_repo(&projects, repo_url) {
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

    /// RAII guard that sets (or removes) [`ALLOW_MCP_SPAWN_ENV`] for the
    /// duration of a `#[serial]` test, restoring the prior value (or absence)
    /// on drop — panic-safe, so a failed assertion mid-test cannot leak the
    /// override into a sibling test (mirrors `session_launch::tests::EnvVarGuard`).
    struct EnvGuard {
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(value: &str) -> Self {
            let prev = std::env::var(ALLOW_MCP_SPAWN_ENV).ok();
            // SAFETY: env-mutating tests using this guard are tagged `#[serial]`.
            unsafe { std::env::set_var(ALLOW_MCP_SPAWN_ENV, value) };
            Self { prev }
        }

        fn unset() -> Self {
            let prev = std::env::var(ALLOW_MCP_SPAWN_ENV).ok();
            // SAFETY: env-mutating tests using this guard are tagged `#[serial]`.
            unsafe { std::env::remove_var(ALLOW_MCP_SPAWN_ENV) };
            Self { prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see `set`/`unset` — serialized by `#[serial]`.
            unsafe {
                match self.prev.take() {
                    Some(v) => std::env::set_var(ALLOW_MCP_SPAWN_ENV, v),
                    None => std::env::remove_var(ALLOW_MCP_SPAWN_ENV),
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

    #[test]
    fn is_known_repo_matches_by_name() {
        let projects = vec![make_project("aria", "https://github.com/duettoresearch/x")];
        // A bare local filesystem path (never a remote URL) matches by
        // derived basename — e.g. a local worktree path ending in `/aria`.
        assert!(is_known_repo(&projects, "/Users/op/checkouts/aria"));
    }

    #[test]
    fn is_known_repo_matches_by_owner_and_repo_ignoring_git_suffix() {
        let projects = vec![make_project(
            "trusty-tools",
            "https://github.com/bobmatnyc/trusty-tools",
        )];
        assert!(is_known_repo(
            &projects,
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
        assert!(!is_known_repo(&projects, "https://github.com/some/repo"));
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
