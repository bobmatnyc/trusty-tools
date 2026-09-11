//! Unit tests for the default-deny session MCP scope (#7422).
//!
//! Why: the whole point of the module is what it LEAVES OUT, and an omission is
//! invisible unless a test names the server that must not appear. Each fixture
//! therefore plants a shared-only server and asserts on both halves — what the
//! composed map holds and what `excluded` reports. The trust gate needs the
//! same treatment from the other side: a test that only ever passes `true`
//! would still pass against a build with no gate at all.
//! What: scope composition under both trust decisions, the fail arms, the file
//! location and its permissions, the flag renderers, the project-config opt-in
//! reader, and the same trust gate applied to the plugin half of that
//! `[session]` table.
//! Test: this file.

use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::TempDir;

use super::*;

/// Write a tm-managed `.claude.json` holding `names` as stdio servers.
fn shared_config(dir: &Path, names: &[&str]) {
    let mut servers = serde_json::Map::new();
    for name in names {
        servers.insert(
            (*name).to_string(),
            json!({"type": "stdio", "command": name, "args": []}),
        );
    }
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(".claude.json"),
        serde_json::to_string_pretty(&json!({"mcpServers": servers})).unwrap(),
    )
    .unwrap();
}

/// Write a project `.mcp.json` holding `names` as stdio servers.
fn project_mcp_json(dir: &Path, names: &[&str]) {
    let mut servers = serde_json::Map::new();
    for name in names {
        servers.insert(
            (*name).to_string(),
            json!({"type": "stdio", "command": name, "args": []}),
        );
    }
    std::fs::write(
        dir.join(".mcp.json"),
        serde_json::to_string_pretty(&json!({"mcpServers": servers})).unwrap(),
    )
    .unwrap();
}

/// A cwd, a config dir, and the composed file's path, all under one tempdir.
fn fixture(tmp: &TempDir) -> (PathBuf, PathBuf, PathBuf) {
    let cwd = tmp.path().join("repo");
    let cfg = tmp.path().join("cfg");
    std::fs::create_dir_all(&cwd).unwrap();
    let out = session_mcp_path_at(&tmp.path().join("state"), &cwd);
    (cwd, cfg, out)
}

#[test]
fn session_mcp_path_is_per_workspace() {
    let root = Path::new("/state");
    let a = session_mcp_path_at(root, Path::new("/repo/.claude/worktrees/wt-1"));
    let b = session_mcp_path_at(root, Path::new("/repo"));
    assert_ne!(a, b, "two worktrees must not share one composed file");
}

/// #7422: the composed file copies every shared entry verbatim, `env` and
/// `headers` included, so it must never be written inside a working tree.
#[test]
fn session_mcp_path_is_outside_the_repository() {
    let cwd = Path::new("/repo");
    let path = session_mcp_path_at(Path::new("/state"), cwd);
    assert!(
        !path.starts_with(cwd),
        "a credential-bearing file must not land in the repo: {path:?}"
    );
    assert!(
        path.starts_with(Path::new("/state").join(SESSION_MCP_DIR)),
        "{path:?}"
    );
    assert_eq!(path.extension().and_then(|e| e.to_str()), Some("json"));
}

#[test]
fn scoped_for_declines_a_non_relocated_spawn() {
    assert_eq!(scoped_for(Path::new("/repo"), None), None);
}

#[test]
fn strict_mcp_flag_string_quotes_the_path() {
    let rendered = strict_mcp_flag_string(Some(Path::new("/a dir/session-mcp.json")));
    assert_eq!(
        rendered, " --strict-mcp-config --mcp-config '/a dir/session-mcp.json'",
        "the pane shell re-splits this line, so the path must be quoted"
    );
}

#[test]
fn strict_mcp_flag_string_is_empty_without_a_file() {
    assert_eq!(strict_mcp_flag_string(None), "");
}

#[test]
fn strict_mcp_argv_is_three_unquoted_tokens() {
    let argv = strict_mcp_argv(Some(Path::new("/a dir/session-mcp.json")));
    assert_eq!(
        argv,
        vec![
            "--strict-mcp-config".to_owned(),
            "--mcp-config".to_owned(),
            "/a dir/session-mcp.json".to_owned(),
        ],
        "exec takes the path verbatim — quoting would name a file claude cannot open"
    );
    assert!(strict_mcp_argv(None).is_empty());
}

#[test]
fn resolve_scope_includes_builtins_and_project_servers() {
    let tmp = TempDir::new().unwrap();
    let (cwd, cfg, _) = fixture(&tmp);
    shared_config(&cfg, &[]);
    project_mcp_json(&cwd, &["project-only"]);

    let scope = resolve_scope_with_trust(&cwd, &cfg, true);

    for builtin in crate::core::mcp_config::BUILTIN_MANAGED_MCP_SERVERS {
        assert!(
            scope.servers.contains_key(*builtin),
            "builtin {builtin} must always load: {:?}",
            scope.included
        );
    }
    assert!(scope.servers.contains_key("project-only"));
    assert!(scope.excluded.is_empty(), "{:?}", scope.excluded);
    assert_eq!(scope.degraded, None);
}

#[test]
fn resolve_scope_excludes_a_shared_only_server() {
    let tmp = TempDir::new().unwrap();
    let (cwd, cfg, _) = fixture(&tmp);
    shared_config(&cfg, &["slack-mcp", "gworkspace-mcp", "trusty-memory"]);

    let scope = resolve_scope_with_trust(&cwd, &cfg, true);

    assert!(
        !scope.servers.contains_key("slack-mcp"),
        "a shared-only server must not load without an opt-in"
    );
    assert_eq!(
        scope.excluded,
        vec!["gworkspace-mcp".to_owned(), "slack-mcp".to_owned()],
        "both shared-only servers are reported; the builtin is not"
    );
    assert!(scope.servers.contains_key("trusty-memory"));
}

#[test]
fn resolve_scope_includes_an_opted_in_shared_server() {
    let tmp = TempDir::new().unwrap();
    let (cwd, cfg, _) = fixture(&tmp);
    shared_config(&cfg, &["slack-mcp", "heygen"]);
    std::fs::write(
        cwd.join(crate::core::project_config::PROJECT_CONFIG_FILE),
        "[session]\nmcp_servers = [\"slack-mcp\"]\n",
    )
    .unwrap();

    let scope = resolve_scope_with_trust(&cwd, &cfg, true);

    assert!(scope.servers.contains_key("slack-mcp"));
    assert_eq!(scope.excluded, vec!["heygen".to_owned()]);
}

#[test]
fn resolve_scope_degrades_on_a_malformed_project_mcp_json() {
    let tmp = TempDir::new().unwrap();
    let (cwd, cfg, _) = fixture(&tmp);
    shared_config(&cfg, &["slack-mcp"]);
    std::fs::write(cwd.join(".mcp.json"), "{ not json").unwrap();

    let scope = resolve_scope_with_trust(&cwd, &cfg, true);

    assert!(
        scope.degraded.is_some(),
        "a project .mcp.json that will not parse must be reported"
    );
    for builtin in crate::core::mcp_config::BUILTIN_MANAGED_MCP_SERVERS {
        assert!(scope.servers.contains_key(*builtin));
    }
    assert!(
        !scope.servers.contains_key("slack-mcp"),
        "degrading must never widen the set"
    );
}

/// #7422: a `.mcp.json` ships with the clone, so it cannot be its own
/// permission — a hostile repo's `{"command": "sh", "args": ["-c", …]}` must
/// not reach a pane running `--dangerously-skip-permissions`.
#[test]
fn resolve_scope_drops_project_mcp_json_for_an_untrusted_project() {
    let tmp = TempDir::new().unwrap();
    let (cwd, cfg, _) = fixture(&tmp);
    shared_config(&cfg, &[]);
    project_mcp_json(&cwd, &["smuggled"]);

    let scope = resolve_scope_with_trust(&cwd, &cfg, false);

    assert!(
        !scope.servers.contains_key("smuggled"),
        "an untrusted project's own .mcp.json must not load: {:?}",
        scope.included
    );
    for builtin in crate::core::mcp_config::BUILTIN_MANAGED_MCP_SERVERS {
        assert!(scope.servers.contains_key(*builtin));
    }
    let degraded = scope.degraded.expect("the drop must be reported");
    assert!(
        degraded.contains("tm project trust"),
        "the message must name the grant: {degraded}"
    );
}

/// #7422: the committed `[session] mcp_servers` list decides which of the
/// OPERATOR's credentialed servers load, so it needs the same grant.
#[test]
fn resolve_scope_ignores_opt_ins_for_an_untrusted_project() {
    let tmp = TempDir::new().unwrap();
    let (cwd, cfg, _) = fixture(&tmp);
    shared_config(&cfg, &["slack-mcp"]);
    std::fs::write(
        cwd.join(crate::core::project_config::PROJECT_CONFIG_FILE),
        "[session]\nmcp_servers = [\"slack-mcp\"]\n",
    )
    .unwrap();

    let scope = resolve_scope_with_trust(&cwd, &cfg, false);

    assert!(
        !scope.servers.contains_key("slack-mcp"),
        "a committed opt-in cannot grant itself the operator's credentials"
    );
    assert_eq!(scope.excluded, vec!["slack-mcp".to_owned()]);
    assert!(scope.degraded.is_some());
}

/// #7422: a project that declared nothing lost nothing, so there is no warning
/// to emit — untrusted is the ordinary state, not an error.
#[test]
fn resolve_scope_is_silent_for_an_untrusted_project_that_declares_nothing() {
    let tmp = TempDir::new().unwrap();
    let (cwd, cfg, _) = fixture(&tmp);
    shared_config(&cfg, &["slack-mcp"]);

    let scope = resolve_scope_with_trust(&cwd, &cfg, false);

    assert_eq!(
        scope.degraded, None,
        "nothing was declared, so nothing dropped"
    );
    assert_eq!(scope.excluded, vec!["slack-mcp".to_owned()]);
}

/// #7422: the production entry point must actually read the trust store, not
/// just accept a boolean a caller invented.
#[test]
#[serial_test::serial]
fn resolve_scope_consults_the_real_trust_store() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _guard = HomeOverride::set(&home);

    let (cwd, cfg, _) = fixture(&tmp);
    shared_config(&cfg, &[]);
    project_mcp_json(&cwd, &["project-only"]);

    assert!(
        !resolve_scope(&cwd, &cfg)
            .servers
            .contains_key("project-only"),
        "an untrusted project is the default"
    );

    let root = crate::core::project_trust::trust_store_root().expect("HOME is set");
    let mut store = crate::core::project_trust::ProjectTrustStore::load(&root).unwrap();
    assert!(store.trust(&cwd));
    store.save().unwrap();

    assert!(
        resolve_scope(&cwd, &cfg)
            .servers
            .contains_key("project-only"),
        "`tm project trust` must be what flips it"
    );
}

#[test]
fn shared_servers_tolerates_a_malformed_config() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join(".claude.json"), "{ not json").unwrap();

    assert!(shared_servers(&cfg).is_empty());
    assert!(
        cfg.join(".claude.json").exists(),
        "the launch path must never quarantine the file that holds OAuth state"
    );
}

#[test]
fn opt_in_servers_is_empty_without_a_config() {
    let tmp = TempDir::new().unwrap();
    assert!(opt_in_servers(tmp.path()).is_empty());
    assert!(opt_in_plugins(tmp.path()).is_empty());
}

#[test]
fn opt_in_servers_reads_the_project_config() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path()
            .join(crate::core::project_config::PROJECT_CONFIG_FILE),
        "[session]\nmcp_servers = [\"apex\", \"slack-mcp\"]\n",
    )
    .unwrap();
    assert_eq!(
        opt_in_servers(tmp.path()),
        vec!["apex".to_owned(), "slack-mcp".to_owned()]
    );
}

#[test]
fn opt_in_plugins_reads_the_project_config() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path()
            .join(crate::core::project_config::PROJECT_CONFIG_FILE),
        "[session]\nplugins = [\"aws-core\"]\n",
    )
    .unwrap();
    assert_eq!(opt_in_plugins(tmp.path()), vec!["aws-core".to_owned()]);
}

/// A project whose committed config opts `aws-core` in.
fn project_declaring_a_plugin(tmp: &TempDir) -> PathBuf {
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(crate::core::project_config::PROJECT_CONFIG_FILE),
        "[session]\nplugins = [\"aws-core\"]\n",
    )
    .unwrap();
    dir
}

/// A managed config dir with `aws-core@m` installed.
fn config_with_installed_plugin(tmp: &TempDir) -> PathBuf {
    let cfg = tmp.path().join("cfg");
    std::fs::create_dir_all(cfg.join("plugins")).unwrap();
    std::fs::write(
        cfg.join("plugins").join("installed_plugins.json"),
        serde_json::to_string_pretty(&json!({"plugins": {"aws-core@m": []}})).unwrap(),
    )
    .unwrap();
    cfg
}

#[test]
fn granted_plugins_are_empty_for_an_untrusted_project() {
    let tmp = TempDir::new().unwrap();
    let project = project_declaring_a_plugin(&tmp);

    assert!(
        granted_plugins_with_trust(&project, false).is_empty(),
        "a `[session] plugins` list ships with the clone, so it grants nothing \
         until the operator trusts the directory"
    );
}

#[test]
fn granted_plugins_pass_through_for_a_trusted_project() {
    let tmp = TempDir::new().unwrap();
    let project = project_declaring_a_plugin(&tmp);

    assert_eq!(
        granted_plugins_with_trust(&project, true),
        vec!["aws-core".to_owned()]
    );
}

#[test]
fn plugin_scope_denies_an_opt_in_from_an_untrusted_project() {
    use crate::core::session_plugin_scope::plugin_scope;

    let tmp = TempDir::new().unwrap();
    let project = project_declaring_a_plugin(&tmp);
    let cfg = config_with_installed_plugin(&tmp);

    let untrusted = plugin_scope(&cfg, &granted_plugins_with_trust(&project, false));
    let trusted = plugin_scope(&cfg, &granted_plugins_with_trust(&project, true));

    assert_eq!(
        untrusted.get("aws-core@m"),
        Some(&false),
        "an untrusted clone must not turn an operator-installed plugin on in a \
         pane running --dangerously-skip-permissions"
    );
    assert_eq!(trusted.get("aws-core@m"), Some(&true));
}

#[test]
#[serial_test::serial]
fn granted_plugins_reads_the_real_trust_store_and_denies_by_default() {
    let tmp = TempDir::new().unwrap();
    let project = project_declaring_a_plugin(&tmp);
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let prev = std::env::var_os("HOME");
    // SAFETY: this test is `#[serial]`, so no other test thread races the
    // set/restore, and `$HOME` is put back before it returns.
    unsafe { std::env::set_var("HOME", &home) };

    let granted = granted_plugins(&project);

    match prev {
        Some(v) => unsafe { std::env::set_var("HOME", v) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    assert!(
        granted.is_empty(),
        "the production accessor must resolve the trust store itself and \
         fail closed on a home that records no grant"
    );
}

#[test]
fn provision_writes_the_composed_map() {
    let tmp = TempDir::new().unwrap();
    let (cwd, cfg, out) = fixture(&tmp);
    shared_config(&cfg, &["slack-mcp"]);

    let path = provision_at(&out, &cwd, &cfg).expect("provision succeeds");

    assert_eq!(path, out);
    assert!(
        !path.starts_with(&cwd),
        "the composed file must never land in the working tree: {path:?}"
    );
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let servers = written["mcpServers"].as_object().unwrap();
    assert!(servers.contains_key("trusty-mpm"));
    assert!(
        !servers.contains_key("slack-mcp"),
        "the written file is the default-deny set, not the shared map"
    );
}

/// #7422: a stdio server's `env` and a remote server's `headers` are copied
/// verbatim, so the file and its directory are owner-only.
#[cfg(unix)]
#[test]
fn provision_writes_an_owner_only_file() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = TempDir::new().unwrap();
    let (cwd, cfg, out) = fixture(&tmp);
    shared_config(&cfg, &[]);

    // Twice: `OpenOptions::mode` applies at CREATION only, so the rewrite arm
    // needs its own proof.
    provision_at(&out, &cwd, &cfg).unwrap();
    let path = provision_at(&out, &cwd, &cfg).unwrap();

    let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(file_mode, OWNER_ONLY_FILE, "file mode {file_mode:o}");
    let dir_mode = std::fs::metadata(path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, OWNER_ONLY_DIR, "dir mode {dir_mode:o}");
}

#[test]
fn provision_rewrites_on_every_call() {
    let tmp = TempDir::new().unwrap();
    let (cwd, cfg, out) = fixture(&tmp);
    shared_config(&cfg, &["slack-mcp"]);

    let path = provision_at(&out, &cwd, &cfg).unwrap();
    std::fs::write(&path, "{\"mcpServers\":{\"stale\":{}}}").unwrap();
    provision_at(&out, &cwd, &cfg).unwrap();

    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        written["mcpServers"].get("stale").is_none(),
        "a stale set from a previous launch must not survive"
    );
}

#[test]
fn provision_for_spawn_declines_a_non_relocated_spawn() {
    let tmp = TempDir::new().unwrap();
    assert_eq!(provision_for_spawn(tmp.path(), None).unwrap(), None);
}

/// The fail-CLOSED arm: an unwritable state dir aborts rather than degrading.
///
/// Why: the only alternative to a composed file is a spawn with no
/// `--mcp-config`, which is the unscoped shared map this module exists to stop.
/// What: plants a regular FILE where `session-mcp/` must be a directory, so
/// `create_dir_all` fails, and asserts the error rather than a silent `Ok`.
/// Test: this test.
#[test]
fn provision_errors_when_the_state_dir_is_unwritable() {
    let tmp = TempDir::new().unwrap();
    let (cwd, cfg, out) = fixture(&tmp);
    shared_config(&cfg, &[]);
    let dir = out.parent().unwrap();
    std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
    // A file where the state directory has to be: `create_dir_all` cannot win.
    std::fs::write(dir, "not a directory").unwrap();

    let err =
        provision_at(&out, &cwd, &cfg).expect_err("an unwritable state dir must fail the launch");

    assert!(
        matches!(err, ScopeError::Write { .. }),
        "expected a Write error, got {err:?}"
    );
    assert!(
        err.to_string().contains("refusing to launch"),
        "the message must say why it did not fall back: {err}"
    );
}

/// RAII `$HOME` override for the one test that must read the real trust store.
///
/// Why: `dirs::home_dir` is the only way `trust_store_root` resolves, and a
/// test that pointed at the developer's own `$HOME` would either read their
/// real grants or write one. Callers MUST be `#[serial_test::serial]`.
/// What: sets `HOME`, restores the prior value on drop (panics included).
/// Test: used by `resolve_scope_consults_the_real_trust_store`.
struct HomeOverride {
    prev: Option<std::ffi::OsString>,
}

impl HomeOverride {
    fn set(value: &Path) -> Self {
        let prev = std::env::var_os("HOME");
        // SAFETY: every caller is `#[serial]`, so no other test thread races
        // this set/restore.
        unsafe { std::env::set_var("HOME", value) };
        Self { prev }
    }
}

impl Drop for HomeOverride {
    fn drop(&mut self) {
        // SAFETY: see `set`.
        match self.prev.take() {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}
