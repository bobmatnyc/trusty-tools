//! Unit tests for trusty-mpm's `~/.trusty-tools/trusty-mpm/config.yaml`
//! settings and workspace-root resolution, split out of
//! `trusty_tools_config.rs` to keep that file under the 500-SLOC production
//! cap (#610) — the inline `mod tests` counted as production source.

use super::*;

/// Serialise env mutation so the env-reading tests cannot race each other
/// across the shared test process. Delegates to the crate-wide
/// [`super::env_test_lock`] so env tests in OTHER modules are serialised too.
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    super::env_test_lock()
}

/// Why (DOC-34): the managed `CLAUDE_CONFIG_DIR` MUST resolve under the
/// shared `~/.trusty-tools/trusty-mpm/` base (never `~/.trusty-mpm` or the
/// project). Pinning the exact layout guards the segregation guarantee.
/// Test: itself.
#[test]
fn managed_claude_config_dir_nests_under_trusty_tools() {
    let base = PathBuf::from("/home/bob");
    assert_eq!(
        managed_claude_config_dir_at(&base),
        PathBuf::from("/home/bob/.trusty-tools/trusty-mpm/claude-config")
    );
    // The production accessor, when home resolves, must agree with the
    // hermetic `_at` variant on the trailing `.trusty-tools/trusty-mpm/...`
    // segments regardless of the actual home directory.
    if let Some(dir) = managed_claude_config_dir() {
        assert!(
            dir.ends_with("trusty-mpm/claude-config"),
            "managed config dir must end with trusty-mpm/claude-config, got {}",
            dir.display()
        );
        assert!(
            dir.to_string_lossy().contains(".trusty-tools"),
            "managed config dir must live under .trusty-tools, got {}",
            dir.display()
        );
    }
}

/// Why: the built-in default (no env, no config) must be the #1220 path.
/// Test: itself.
#[test]
fn default_template_is_trusty_mpm_projects() {
    let _g = env_lock();
    // SAFETY: guarded by env_lock; restored below.
    unsafe { std::env::remove_var(WORKSPACE_ROOT_ENV) };
    let root = workspace_root(&TrustyToolsConfig::default());
    assert!(
        root.ends_with(DEFAULT_WORKSPACE_DIR),
        "expected …/{DEFAULT_WORKSPACE_DIR}, got {}",
        root.display()
    );
}

/// Why: the env var must win over both the config template and the default.
/// Test: itself.
#[test]
fn env_overrides_config_and_default() {
    let _g = env_lock();
    let cfg = TrustyToolsConfig {
        workspace_root_template: Some("~/from-config".into()),
        ..Default::default()
    };
    // SAFETY: guarded by env_lock; removed at end.
    unsafe { std::env::set_var(WORKSPACE_ROOT_ENV, "/explicit/env/root") };
    let root = workspace_root(&cfg);
    unsafe { std::env::remove_var(WORKSPACE_ROOT_ENV) };
    assert_eq!(root, PathBuf::from("/explicit/env/root"));
}

/// Why: with no env override, the config template must be used (and `~`
/// expanded), beating the built-in default.
/// Test: itself.
#[test]
fn config_template_used_when_no_env() {
    let _g = env_lock();
    unsafe { std::env::remove_var(WORKSPACE_ROOT_ENV) };
    let cfg = TrustyToolsConfig {
        workspace_root_template: Some("/custom/projects".into()),
        ..Default::default()
    };
    let root = workspace_root(&cfg);
    assert_eq!(root, PathBuf::from("/custom/projects"));
}

/// Why: a leading `~` in a template must expand to the home directory.
/// Test: itself.
#[test]
fn tilde_expansion() {
    let home = PathBuf::from("/home/bob");
    assert_eq!(
        expand_tilde("~/trusty-mpm-projects", &home),
        PathBuf::from("/home/bob/trusty-mpm-projects")
    );
    assert_eq!(expand_tilde("~", &home), home);
    assert_eq!(expand_tilde("/abs/path", &home), PathBuf::from("/abs/path"));
}

/// Why: the `github:` section must round-trip through YAML so an operator's
/// declarative binding survives load/save unchanged (and absent fields stay
/// absent rather than serialising as nulls).
/// Test: itself.
#[test]
fn github_config_yaml_round_trip() {
    let cfg = TrustyToolsConfig {
        github: Some(GithubConfig {
            config_dir: Some(PathBuf::from("/home/bob/.config/gh-work")),
            token_env: Some("WORK_GH_TOKEN".into()),
            account: Some("bob-work".into()),
            host: Some("github.example.com".into()),
        }),
        ..Default::default()
    };
    let yaml = serde_yaml::to_string(&cfg).expect("serialise");
    let back: TrustyToolsConfig = serde_yaml::from_str(&yaml).expect("deserialise");
    assert_eq!(cfg, back);
    // Absent top-level fields must not appear in the YAML.
    assert!(!yaml.contains("workspace_root_template"), "yaml: {yaml}");
    assert!(yaml.contains("github:"), "yaml: {yaml}");
}

/// Why: the #1265 no-plaintext-token guarantee — the config stores only the
/// NAME of the env var, never the secret value. A reviewer must be able to
/// assert this invariant mechanically.
/// Test: itself.
#[test]
fn github_config_stores_only_env_name() {
    let cfg = GithubConfig {
        token_env: Some("MY_GH_TOKEN".into()),
        ..Default::default()
    };
    // The struct field is named `token_env` and holds the var NAME; there is
    // no field that could hold a token value. Serialised form proves it.
    let yaml = serde_yaml::to_string(&cfg).expect("serialise");
    assert!(yaml.contains("token_env"), "yaml: {yaml}");
    assert!(yaml.contains("MY_GH_TOKEN"), "yaml: {yaml}");
    assert!(
        !yaml.contains("token:"),
        "must not have a bare token field: {yaml}"
    );
}

/// Why (#2081): a `projects:` entry's `gh_user` preference must round-trip
/// through YAML, and an absent `gh_user` must not serialise (matching every
/// other optional field on `ProjectConfig`) so existing configs deserialise
/// unchanged.
/// Test: itself.
#[test]
fn project_config_gh_user_yaml_round_trip() {
    let cfg = TrustyToolsConfig {
        projects: vec![ProjectConfig {
            name: "trusty-tools".into(),
            repo_url: "https://github.com/bobmatnyc/trusty-tools".into(),
            default_branch: Some("main".into()),
            stack_hint: None,
            tags: None,
            description: None,
            gh_user: Some("bobmatnyc".into()),
            gh_account: Some("bobmatnyc".into()),
            github: None,
            commit_name: None,
            commit_email: None,
            untracked_sync: None,
            worktree: None,
        }],
        ..Default::default()
    };
    let yaml = serde_yaml::to_string(&cfg).expect("serialise");
    assert!(yaml.contains("gh_user"), "yaml: {yaml}");
    assert!(yaml.contains("gh_account"), "yaml: {yaml}");
    let back: TrustyToolsConfig = serde_yaml::from_str(&yaml).expect("deserialise");
    assert_eq!(cfg, back);

    // Absent gh_user/gh_account must not serialise.
    let no_pref = TrustyToolsConfig {
        projects: vec![ProjectConfig {
            name: "other".into(),
            repo_url: "https://github.com/o/other".into(),
            default_branch: None,
            stack_hint: None,
            tags: None,
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            untracked_sync: None,
            worktree: None,
        }],
        ..Default::default()
    };
    let yaml_no_pref = serde_yaml::to_string(&no_pref).expect("serialise");
    assert!(!yaml_no_pref.contains("gh_user"), "yaml: {yaml_no_pref}");
    assert!(!yaml_no_pref.contains("gh_account"), "yaml: {yaml_no_pref}");
}

/// Why (#2184): a project's per-project `github:` binding and commit
/// identity (`commit_name`/`commit_email`) must round-trip through YAML,
/// and absent fields must not serialise — matching every other optional
/// `ProjectConfig` field so existing configs deserialise unchanged.
/// Test: itself.
#[test]
fn project_config_github_and_commit_identity_yaml_round_trip() {
    let cfg = TrustyToolsConfig {
        projects: vec![ProjectConfig {
            name: "work-repo".into(),
            repo_url: "https://github.com/acme/work-repo".into(),
            default_branch: None,
            stack_hint: None,
            tags: None,
            description: None,
            gh_user: None,
            gh_account: None,
            github: Some(GithubConfig {
                config_dir: Some(PathBuf::from("/home/bob/.config/gh-work")),
                token_env: None,
                account: Some("bob-work".into()),
                host: None,
            }),
            commit_name: Some("Bob (work bot)".into()),
            commit_email: Some("bob@acme.example.com".into()),
            untracked_sync: None,
            worktree: None,
        }],
        ..Default::default()
    };
    let yaml = serde_yaml::to_string(&cfg).expect("serialise");
    assert!(yaml.contains("github:"), "yaml: {yaml}");
    assert!(yaml.contains("commit_name"), "yaml: {yaml}");
    assert!(yaml.contains("commit_email"), "yaml: {yaml}");
    let back: TrustyToolsConfig = serde_yaml::from_str(&yaml).expect("deserialise");
    assert_eq!(cfg, back);

    // Absent per-project github/commit fields must not serialise.
    let minimal = TrustyToolsConfig {
        projects: vec![ProjectConfig {
            name: "minimal".into(),
            repo_url: "https://github.com/o/minimal".into(),
            default_branch: None,
            stack_hint: None,
            tags: None,
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            untracked_sync: None,
            worktree: None,
        }],
        ..Default::default()
    };
    let yaml_minimal = serde_yaml::to_string(&minimal).expect("serialise");
    assert!(!yaml_minimal.contains("github:"), "yaml: {yaml_minimal}");
    assert!(
        !yaml_minimal.contains("commit_name"),
        "yaml: {yaml_minimal}"
    );
    assert!(
        !yaml_minimal.contains("commit_email"),
        "yaml: {yaml_minimal}"
    );
}

/// Why: the `daemon:` section (#1836) must round-trip through YAML, and an
/// absent section must not serialise at all (matching every other optional
/// top-level section).
/// Test: itself.
#[test]
fn daemon_config_yaml_round_trip() {
    let cfg = TrustyToolsConfig {
        daemon: Some(DaemonConfig {
            allow_mcp_spawn: Some(true),
        }),
        ..Default::default()
    };
    let yaml = serde_yaml::to_string(&cfg).expect("serialise");
    let back: TrustyToolsConfig = serde_yaml::from_str(&yaml).expect("deserialise");
    assert_eq!(cfg, back);
    assert!(yaml.contains("allow_mcp_spawn"), "yaml: {yaml}");

    // Absent daemon section must not serialise.
    let empty = TrustyToolsConfig::default();
    let yaml_empty = serde_yaml::to_string(&empty).expect("serialise");
    assert!(!yaml_empty.contains("daemon"), "yaml: {yaml_empty}");
}

/// Why: the `tmux:` section (#2398) must round-trip through YAML, and an
/// absent section must not serialise at all (matching every other
/// optional top-level section).
/// Test: itself.
#[test]
fn tmux_config_yaml_round_trip() {
    let cfg = TrustyToolsConfig {
        tmux: Some(TmuxConfig {
            history_limit: Some(50_000),
            mouse: Some(false),
            alternate_screen: Some(false),
        }),
        ..Default::default()
    };
    let yaml = serde_yaml::to_string(&cfg).expect("serialise");
    let back: TrustyToolsConfig = serde_yaml::from_str(&yaml).expect("deserialise");
    assert_eq!(cfg, back);
    assert!(yaml.contains("history_limit"), "yaml: {yaml}");
    assert!(yaml.contains("mouse"), "yaml: {yaml}");
    assert!(yaml.contains("alternate_screen"), "yaml: {yaml}");

    // Absent tmux section must not serialise.
    let empty = TrustyToolsConfig::default();
    let yaml_empty = serde_yaml::to_string(&empty).expect("serialise");
    assert!(!yaml_empty.contains("tmux"), "yaml: {yaml_empty}");
}

/// Why: with no `tmux:` section at all, resolution must fall back to the
/// built-in defaults (100,000-line history-limit, mouse on,
/// alternate-screen on).
/// Test: itself.
#[test]
fn tmux_options_default_when_no_config() {
    let opts = resolve_tmux_options(&TrustyToolsConfig::default());
    assert_eq!(opts.history_limit, DEFAULT_TMUX_HISTORY_LIMIT);
    assert_eq!(opts.mouse, DEFAULT_TMUX_MOUSE);
    assert_eq!(opts.alternate_screen, DEFAULT_TMUX_ALTERNATE_SCREEN);
}

/// Why (#5151): `alternate_screen` is an OPT-IN knob. `on` is tmux's
/// factory default and today's behaviour, so an absent section — and an
/// absent field within a present section — must both resolve to `true`.
/// Defaulting the other way would silently change how every full-screen
/// program in every pane on the shared server behaves.
/// Test: itself.
#[test]
fn tmux_options_alternate_screen_defaults_on() {
    // Compile-time tripwire: flipping the default is a deliberate act.
    const { assert!(DEFAULT_TMUX_ALTERNATE_SCREEN) };
    assert!(resolve_tmux_options(&TrustyToolsConfig::default()).alternate_screen);

    // A `tmux:` section that sets other fields but not this one still
    // falls back per-field.
    let partial = TrustyToolsConfig {
        tmux: Some(TmuxConfig {
            history_limit: Some(10_000),
            mouse: Some(false),
            alternate_screen: None,
        }),
        ..Default::default()
    };
    assert!(resolve_tmux_options(&partial).alternate_screen);
}

/// Why (#5151): a configured `false` is the ONLY thing that turns the
/// alternate screen off, and it must be honoured verbatim — unlike
/// `history_limit`, neither value is pathological, so nothing is clamped
/// or corrected.
/// Test: itself.
#[test]
fn tmux_options_alternate_screen_config_override() {
    let off = TrustyToolsConfig {
        tmux: Some(TmuxConfig {
            alternate_screen: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(!resolve_tmux_options(&off).alternate_screen);

    let on = TrustyToolsConfig {
        tmux: Some(TmuxConfig {
            alternate_screen: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(resolve_tmux_options(&on).alternate_screen);
}

/// Why (#5151): the YAML key an operator actually types must deserialise.
/// A rename or serde-attribute slip would leave `alternate_screen: false`
/// silently ignored, which is indistinguishable from the feature not
/// working.
/// Test: itself.
#[test]
fn tmux_alternate_screen_parses_from_yaml() {
    let cfg: TrustyToolsConfig =
        serde_yaml::from_str("tmux:\n  alternate_screen: false\n").expect("deserialise");
    assert_eq!(
        cfg.tmux.as_ref().and_then(|t| t.alternate_screen),
        Some(false)
    );
    assert!(!resolve_tmux_options(&cfg).alternate_screen);
}

/// Why: an explicit `tmux:` section must override the built-in defaults,
/// field-by-field (a partial section still falls back per-field).
/// Test: itself.
#[test]
fn tmux_options_config_override() {
    let cfg = TrustyToolsConfig {
        tmux: Some(TmuxConfig {
            history_limit: Some(250_000),
            mouse: Some(false),
            alternate_screen: Some(false),
        }),
        ..Default::default()
    };
    let opts = resolve_tmux_options(&cfg);
    assert_eq!(opts.history_limit, 250_000);
    assert!(!opts.mouse);

    // Partial override: only history_limit set, mouse falls back to default.
    let partial = TrustyToolsConfig {
        tmux: Some(TmuxConfig {
            history_limit: Some(10_000),
            mouse: None,
            alternate_screen: None,
        }),
        ..Default::default()
    };
    let opts = resolve_tmux_options(&partial);
    assert_eq!(opts.history_limit, 10_000);
    assert_eq!(opts.mouse, DEFAULT_TMUX_MOUSE);
}

/// Why (#2398 QA finding): `history-limit 0` means "no scrollback" in
/// tmux, not "unlimited" — a configured `0` must be clamped up to
/// [`MIN_TMUX_HISTORY_LIMIT`] rather than honoured verbatim, which would
/// silently reintroduce the unscrollable-session problem this feature
/// exists to fix.
/// Test: itself.
#[test]
fn tmux_options_clamps_zero_history_limit() {
    let cfg = TrustyToolsConfig {
        tmux: Some(TmuxConfig {
            history_limit: Some(0),
            mouse: None,
            alternate_screen: None,
        }),
        ..Default::default()
    };
    let opts = resolve_tmux_options(&cfg);
    assert_eq!(opts.history_limit, MIN_TMUX_HISTORY_LIMIT);
}

/// Why: any near-zero configured value below the floor is clamped, not
/// just the literal `0` case.
/// Test: itself.
#[test]
fn tmux_options_clamps_below_minimum() {
    let cfg = TrustyToolsConfig {
        tmux: Some(TmuxConfig {
            history_limit: Some(42),
            mouse: None,
            alternate_screen: None,
        }),
        ..Default::default()
    };
    let opts = resolve_tmux_options(&cfg);
    assert_eq!(opts.history_limit, MIN_TMUX_HISTORY_LIMIT);

    // A value AT the floor is left untouched (not bumped further).
    let at_floor = TrustyToolsConfig {
        tmux: Some(TmuxConfig {
            history_limit: Some(MIN_TMUX_HISTORY_LIMIT),
            mouse: None,
            alternate_screen: None,
        }),
        ..Default::default()
    };
    assert_eq!(
        resolve_tmux_options(&at_floor).history_limit,
        MIN_TMUX_HISTORY_LIMIT
    );
}

/// Why: the project subpath must nest `<owner>/<repo>` under the root in that
/// order (the #1220 layout the migration scan also relies on).
/// Test: itself.
#[test]
fn subpath_nests_owner_repo() {
    let _g = env_lock();
    unsafe { std::env::remove_var(WORKSPACE_ROOT_ENV) };
    let cfg = TrustyToolsConfig {
        workspace_root_template: Some("/projects".into()),
        ..Default::default()
    };
    let gh = GithubPath {
        owner: "bobmatnyc".into(),
        repo: "trusty-tools".into(),
    };
    assert_eq!(
        workspace_subpath(&cfg, &gh),
        PathBuf::from("/projects/bobmatnyc/trusty-tools")
    );
}

// untracked_sync (#2196) resolution + YAML round-trip tests live in
// `untracked_sync::tests` (split out to keep this file under the
// 500-SLOC cap; see that module's doc).
