//! Unit tests for the native trusty MCP fleet injector (issue #2739) and its
//! secret-routing fix (code-critic BLOCK follow-up).
//!
//! Why: split out of the injector module (`native_mcp.rs`, a 500-SLOC-capped
//! production file) so the coverage can grow under the 1500-SLOC test cap,
//! mirroring the `tests_roster.rs` split.
//! What: drives the hermetic [`super::native_mcp::inject_native_trusty_mcps_from`]
//! core against a tempdir `CLAUDE_CONFIG_DIR` + a REAL git-initialised
//! workspace (secret routing needs an actual `.git` for `git rev-parse
//! --git-path info/exclude` to resolve), plus the trust preseed and the
//! smaller private helpers exposed `pub(super)` for direct unit coverage. The
//! headline regression coverage (mandated by the code-critic BLOCK) proves NO
//! raw token value ever lands in `.mcp.json`, that `.env.local` carries the
//! real values, and that `.env.local` is genuinely git-excluded — including a
//! literal `git add -A` + status check reproducing the exact leak scenario the
//! review described.
//! Test: this file IS the test module.

use super::native_mcp::{
    NATIVE_TRUSTY_MCP_SERVERS, ensure_env_local_git_excluded, inject_native_trusty_mcps_from,
    is_env_local_actually_ignored, merge_env_file, split_public_and_secret_env,
};
use super::settings::preseed_workspace_trust;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use tempfile::tempdir;

/// Token value used by every fixture below — chosen to be unmistakably a raw
/// secret in a grep/contains assertion.
const FAKE_SLACK_TOKEN: &str = "xoxb-verbatim-secret-DO-NOT-COMMIT";

/// `git init -q` a workspace directory so the secret-routing git-exclude path
/// actually executes (it no-ops on a non-repo).
///
/// Why: every test that exercises [`super::native_mcp::route_native_mcp_secrets`]
/// (indirectly, via `inject_native_trusty_mcps_from`) needs a real `.git` for
/// `git rev-parse --git-path info/exclude` to resolve — a plain tempdir is not
/// a repo, so that step would silently skip (tested separately, see
/// `inject_native_secrets_skipped_outside_git_repo`).
/// What: runs `git init -q` in `dir`, with a pinned local git identity so this
/// never depends on the host's global git config.
/// Test: used by every git-backed test below.
fn git_init(dir: &Path) {
    let status = std::process::Command::new("git")
        .arg("init")
        .arg("-q")
        .arg(dir)
        .status()
        .expect("git must be on PATH to run this test");
    assert!(status.success(), "git init failed");
}

/// Write a managed `.claude.json` with a MIX of native + non-native +
/// already-injected servers into `config_dir` and return the two temp roots.
///
/// Why: every injector test needs the same realistic managed registry — the
/// mixed shape is the point of the issue's mandated coverage (only natives get
/// through, and their secrets never land in `.mcp.json`). Centralising it keeps
/// each test focused on one assertion.
/// What: creates `<config_dir>/.claude.json` with `slack-mcp` (native, has
/// secret env), `gworkspace-mcp` (native, no env), `github` (non-native
/// stdio), a Duetto HTTP endpoint (non-native http), and `trusty-memory`
/// (dedicated-injector, must be ignored here). Returns nothing beyond the side
/// effect (the caller owns the dirs).
/// Test: used by every `inject_native_*` test below.
fn seed_managed_registry(config_dir: &std::path::Path) {
    std::fs::create_dir_all(config_dir).unwrap();
    let registry = json!({
        "oauthAccount": { "keep": "me" },
        "mcpServers": {
            "slack-mcp": {
                "type": "stdio",
                "command": "slack-mcp",
                "args": ["serve"],
                "env": { "SLACK_BOT_TOKEN": FAKE_SLACK_TOKEN }
            },
            "gworkspace-mcp": {
                "type": "stdio",
                "command": "gworkspace-mcp",
                "args": []
            },
            "github": {
                "type": "stdio",
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/server-github"]
            },
            "duetto-endpoint": {
                "type": "http",
                "url": "https://duetto.example/mcp"
            },
            "trusty-memory": {
                "type": "stdio",
                "command": "trusty-memory",
                "args": ["serve", "--stdio"]
            }
        }
    });
    std::fs::write(
        config_dir.join(".claude.json"),
        serde_json::to_string_pretty(&registry).unwrap(),
    )
    .unwrap();
}

/// Read the workspace `.mcp.json` `mcpServers` object.
///
/// Why: shared assertion helper for the post-injection state.
/// What: parses `<workspace>/.mcp.json` and returns its `mcpServers` map; panics
/// (test-only) if the file or key is missing.
/// Test: used by every `inject_native_*` test below.
fn read_injected(workspace: &std::path::Path) -> serde_json::Map<String, Value> {
    let text = std::fs::read_to_string(workspace.join(".mcp.json")).unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    value["mcpServers"].as_object().cloned().unwrap()
}

#[test]
fn inject_native_only_injects_allowlisted() {
    // Why (#2739): the injector must bridge ONLY native trusty servers from the
    // managed registry into the workspace — not the whole registry.
    let cfg = tempdir().unwrap();
    let ws = tempdir().unwrap();
    seed_managed_registry(cfg.path());

    inject_native_trusty_mcps_from(ws.path(), cfg.path()).expect("injection succeeds");

    let servers = read_injected(ws.path());
    assert!(servers.contains_key("slack-mcp"), "native slack injected");
    assert!(
        servers.contains_key("gworkspace-mcp"),
        "native gworkspace injected"
    );
    // trusty-analyze and telegram-mcp are allowlisted but absent from the
    // registry → not injected (only the intersection lands).
    assert!(!servers.contains_key("trusty-analyze"));
    assert!(!servers.contains_key("telegram-mcp"));
}

#[test]
fn inject_native_skips_non_native() {
    // Why (#2739, owner decision): third-party/HTTP managed servers must NEVER be
    // propagated to fleet sessions, and the dedicated-injector servers must be
    // left for their own injectors.
    let cfg = tempdir().unwrap();
    let ws = tempdir().unwrap();
    seed_managed_registry(cfg.path());

    inject_native_trusty_mcps_from(ws.path(), cfg.path()).expect("injection succeeds");

    let servers = read_injected(ws.path());
    assert!(
        !servers.contains_key("github"),
        "non-native github excluded"
    );
    assert!(
        !servers.contains_key("duetto-endpoint"),
        "non-native HTTP endpoint excluded"
    );
    assert!(
        !servers.contains_key("trusty-memory"),
        "trusty-memory owned by its dedicated injector, not this one"
    );
    // Exactly the two present natives, nothing else.
    assert_eq!(servers.len(), 2, "only the two present natives injected");
}

#[test]
fn inject_native_strips_secret_env_from_mcp_json() {
    // Why (code-critic BLOCK, #2739): `.mcp.json` is git-TRACKED — a raw token
    // written there is one `git add -A && commit && push` away from leaking
    // into remote history. The injector must NEVER write a native server's
    // `env` block into `.mcp.json`, regardless of whether the workspace is a
    // git repo (this assertion holds even with no `git init`, proving the
    // guarantee doesn't depend on the git-exclude step succeeding).
    let cfg = tempdir().unwrap();
    let ws = tempdir().unwrap();
    seed_managed_registry(cfg.path());

    inject_native_trusty_mcps_from(ws.path(), cfg.path()).expect("injection succeeds");

    let raw = std::fs::read_to_string(ws.path().join(".mcp.json")).unwrap();
    assert!(
        !raw.contains(FAKE_SLACK_TOKEN),
        ".mcp.json must never contain the raw token value: {raw}"
    );
    assert!(
        !raw.contains("SLACK_BOT_TOKEN"),
        ".mcp.json must not even name the secret env key: {raw}"
    );

    let servers = read_injected(ws.path());
    let slack = &servers["slack-mcp"];
    assert!(
        slack.get("env").is_none(),
        "slack-mcp entry must carry no env block at all in .mcp.json"
    );
    assert_eq!(slack["command"], json!("slack-mcp"));
    assert_eq!(slack["args"], json!(["serve"]));
}

#[test]
fn inject_native_no_env_key_when_source_has_none() {
    // Why: a native server with no `env` in the managed registry (gworkspace-mcp
    // in the fixture) must round-trip with no `env` key at all — not an empty
    // `{}` — matching the pre-existing `.mcp.json` shape.
    let cfg = tempdir().unwrap();
    let ws = tempdir().unwrap();
    seed_managed_registry(cfg.path());

    inject_native_trusty_mcps_from(ws.path(), cfg.path()).expect("injection succeeds");

    let servers = read_injected(ws.path());
    assert!(servers["gworkspace-mcp"].get("env").is_none());
}

#[test]
fn inject_native_delivers_secrets_via_env_local() {
    // Why (code-critic BLOCK, #2739): the token must still reach the native
    // server somehow — this proves the OTHER half of the fix: the real value
    // lands in the workspace-root `.env.local`, which
    // `trusty_common::inference::credentials::resolve_key` reads via its
    // env → .env.local → store precedence.
    let cfg = tempdir().unwrap();
    let ws = tempdir().unwrap();
    git_init(ws.path());
    seed_managed_registry(cfg.path());

    inject_native_trusty_mcps_from(ws.path(), cfg.path()).expect("injection succeeds");

    let env_local = std::fs::read_to_string(ws.path().join(".env.local"))
        .expect(".env.local must be written when a native server carries secret env");
    assert!(
        env_local.contains("SLACK_BOT_TOKEN"),
        ".env.local must name the key: {env_local}"
    );
    assert!(
        env_local.contains(FAKE_SLACK_TOKEN),
        ".env.local must carry the real value: {env_local}"
    );

    // And still never in .mcp.json.
    let mcp_json = std::fs::read_to_string(ws.path().join(".mcp.json")).unwrap();
    assert!(!mcp_json.contains(FAKE_SLACK_TOKEN));
}

#[test]
fn inject_native_env_local_is_git_excluded_and_unstaged() {
    // Why (code-critic BLOCK, #2739): the literal leak scenario the review
    // described — a fleet agent running `git add -A && commit && push`. This
    // test reproduces exactly that sequence against the real `.env.local` the
    // injector wrote and proves the token can never be staged, let alone
    // committed.
    let cfg = tempdir().unwrap();
    let ws = tempdir().unwrap();
    git_init(ws.path());
    seed_managed_registry(cfg.path());

    inject_native_trusty_mcps_from(ws.path(), cfg.path()).expect("injection succeeds");

    // `git check-ignore` is git's own authority on whether a path is excluded.
    let check_ignore = std::process::Command::new("git")
        .arg("-C")
        .arg(ws.path())
        .args(["check-ignore", "-q", ".env.local"])
        .status()
        .expect("git check-ignore must run");
    assert!(
        check_ignore.success(),
        ".env.local must be recognised as git-excluded"
    );

    // The literal reproduction: stage everything, then prove .env.local never
    // made it into the index.
    let add = std::process::Command::new("git")
        .arg("-C")
        .arg(ws.path())
        .args(["add", "-A"])
        .status()
        .expect("git add must run");
    assert!(add.success());

    let staged = std::process::Command::new("git")
        .arg("-C")
        .arg(ws.path())
        .args(["diff", "--cached", "--name-only"])
        .output()
        .expect("git diff --cached must run");
    let staged_files = String::from_utf8_lossy(&staged.stdout);
    assert!(
        !staged_files.contains(".env.local"),
        "`git add -A` must never stage .env.local: staged files were: {staged_files}"
    );

    // .mcp.json, by contrast, IS staged (it's the whole point of the feature —
    // native servers' non-secret shape is meant to be committed).
    assert!(
        staged_files.contains(".mcp.json"),
        "sanity: .mcp.json should still be a normal staged file"
    );
}

#[test]
fn inject_native_env_local_routing_is_idempotent() {
    // Why (#2739): prep runs on every spawn; a second run against an unchanged
    // registry must leave BOTH `.mcp.json` and `.env.local` byte-identical.
    let cfg = tempdir().unwrap();
    let ws = tempdir().unwrap();
    git_init(ws.path());
    seed_managed_registry(cfg.path());

    inject_native_trusty_mcps_from(ws.path(), cfg.path()).expect("first injection");
    let mcp_json_1 = std::fs::read_to_string(ws.path().join(".mcp.json")).unwrap();
    let env_local_1 = std::fs::read_to_string(ws.path().join(".env.local")).unwrap();

    inject_native_trusty_mcps_from(ws.path(), cfg.path()).expect("second injection");
    let mcp_json_2 = std::fs::read_to_string(ws.path().join(".mcp.json")).unwrap();
    let env_local_2 = std::fs::read_to_string(ws.path().join(".env.local")).unwrap();

    assert_eq!(mcp_json_1, mcp_json_2, ".mcp.json is a no-op on rerun");
    assert_eq!(env_local_1, env_local_2, ".env.local is a no-op on rerun");
}

#[test]
fn inject_native_secrets_skipped_outside_git_repo() {
    // Why (#2739): when the workspace is NOT a git repo, the injector must not
    // write `.env.local` at all (there is no exclusion guarantee it could
    // confirm) — it degrades to "native servers launch without credentials"
    // rather than ever risking an un-excluded secret file. `.mcp.json` still
    // gets written normally.
    let cfg = tempdir().unwrap();
    let ws = tempdir().unwrap();
    // Deliberately no git_init(ws.path()).
    seed_managed_registry(cfg.path());

    inject_native_trusty_mcps_from(ws.path(), cfg.path()).expect("injection still succeeds");

    assert!(
        !ws.path().join(".env.local").exists(),
        ".env.local must not be created when git-exclusion cannot be confirmed"
    );
    assert!(
        ws.path().join(".mcp.json").exists(),
        ".mcp.json injection must still proceed independently"
    );
}

/// `git add -f <path> && git commit` a file, so it becomes genuinely TRACKED.
///
/// Why: the residual-HIGH regression fixture needs a `.env.local` that git
/// considers tracked, not merely present on disk — `info/exclude` is a no-op
/// for a tracked path, which is exactly the gap this test proves is now
/// closed. `-f` is required here (empirically verified against real git,
/// version 2.54): a PLAIN `git add` on a path already matching an
/// `info/exclude` pattern refuses with "paths are ignored ... Use -f if you
/// really want to add them" and adds nothing — some call sites in this file
/// add the exclude entry BEFORE constructing the tracked fixture, so this
/// helper must force past that regardless of ordering.
/// What: force-stages and commits `path` (relative to `dir`) with a pinned
/// local identity, matching [`git_init`]'s isolation.
/// Test: used by `inject_native_skips_secrets_when_env_local_is_tracked`,
/// `is_env_local_actually_ignored_false_when_tracked`.
fn git_commit_file(dir: &Path, path: &str) {
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "trusty-tools-test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "trusty-tools-test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .status()
            .expect("git must be on PATH to run this test");
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["add", "-f", path]);
    run(&["commit", "-q", "-m", "seed a tracked file"]);
}

#[test]
fn inject_native_skips_secrets_when_env_local_is_tracked() {
    // Why (code-critic re-review, residual HIGH on #2739): `info/exclude`
    // cannot un-track an already-tracked file. If the TARGET repo (tm ships to
    // arbitrary repos, including Duetto ones) already commits a file literally
    // named `.env.local`, `ensure_env_local_git_excluded` still "succeeds" (the
    // append itself never errors), so without the `git check-ignore` gate the
    // injector would merge the real token into that tracked file and a routine
    // `git add -A` would stage it — the exact same leak class, relocated. This
    // reproduces that scenario end-to-end and asserts secret delivery is
    // correctly SKIPPED.
    let cfg = tempdir().unwrap();
    let ws = tempdir().unwrap();
    git_init(ws.path());
    let pre_existing_content = "PRE_EXISTING=\"operator committed this\"\n";
    std::fs::write(ws.path().join(".env.local"), pre_existing_content).unwrap();
    git_commit_file(ws.path(), ".env.local");
    seed_managed_registry(cfg.path());

    inject_native_trusty_mcps_from(ws.path(), cfg.path()).expect("injection still succeeds");

    // (a) The tracked file's committed content is unchanged — no token was
    // merged into it.
    let content_after = std::fs::read_to_string(ws.path().join(".env.local")).unwrap();
    assert_eq!(
        content_after, pre_existing_content,
        ".env.local content must be untouched when it is tracked"
    );
    assert!(!content_after.contains(FAKE_SLACK_TOKEN));
    assert!(!content_after.contains("SLACK_BOT_TOKEN"));

    // (b) `git add -A` must not stage any token-bearing change — i.e. there is
    // no diff to stage at all, since the file was never written to.
    let add = std::process::Command::new("git")
        .arg("-C")
        .arg(ws.path())
        .args(["add", "-A"])
        .status()
        .expect("git add must run");
    assert!(add.success());
    let staged = std::process::Command::new("git")
        .arg("-C")
        .arg(ws.path())
        .args(["diff", "--cached", "--name-only"])
        .output()
        .expect("git diff --cached must run");
    let staged_files = String::from_utf8_lossy(&staged.stdout);
    assert!(
        !staged_files.contains(".env.local"),
        ".env.local must have no pending changes to stage: {staged_files}"
    );

    // Sanity: `.mcp.json` injection still proceeded (only secret delivery was
    // skipped), and still carries no raw token either.
    let mcp_json = std::fs::read_to_string(ws.path().join(".mcp.json")).unwrap();
    assert!(!mcp_json.contains(FAKE_SLACK_TOKEN));
}

#[test]
fn is_env_local_actually_ignored_true_when_excluded() {
    let ws = tempdir().unwrap();
    git_init(ws.path());
    ensure_env_local_git_excluded(ws.path()).expect("exclude write succeeds");

    assert!(is_env_local_actually_ignored(ws.path()));
}

#[test]
fn is_env_local_actually_ignored_false_when_tracked() {
    let ws = tempdir().unwrap();
    git_init(ws.path());
    // Excluded via info/exclude AND tracked at the same time — git's own
    // ls-files/staging behaviour wins: a tracked path is never "ignored" in
    // the sense that matters (it will still be staged by `git add -A`).
    ensure_env_local_git_excluded(ws.path()).expect("exclude write succeeds");
    std::fs::write(ws.path().join(".env.local"), "X=1\n").unwrap();
    git_commit_file(ws.path(), ".env.local");

    assert!(!is_env_local_actually_ignored(ws.path()));
}

#[test]
fn is_env_local_actually_ignored_false_when_not_excluded_at_all() {
    let ws = tempdir().unwrap();
    git_init(ws.path());
    // No ensure_env_local_git_excluded call — nothing lists .env.local in
    // info/exclude, and it isn't tracked either.

    assert!(!is_env_local_actually_ignored(ws.path()));
}

#[test]
fn inject_native_no_secrets_writes_no_env_local() {
    // Why (#2739): a native server with no secret env (only gworkspace-mcp
    // present) must never create a `.env.local` at all — nothing to route.
    let cfg = tempdir().unwrap();
    let ws = tempdir().unwrap();
    git_init(ws.path());
    std::fs::create_dir_all(cfg.path()).unwrap();
    std::fs::write(
        cfg.path().join(".claude.json"),
        serde_json::to_string_pretty(&json!({
            "mcpServers": {
                "gworkspace-mcp": { "type": "stdio", "command": "gworkspace-mcp", "args": [] }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    inject_native_trusty_mcps_from(ws.path(), cfg.path()).expect("injection succeeds");

    assert!(
        !ws.path().join(".env.local").exists(),
        "no secrets collected → no .env.local written"
    );
    assert!(read_injected(ws.path()).contains_key("gworkspace-mcp"));
}

#[test]
fn inject_native_preserves_existing_injectors() {
    // Why (#2739): the native injector runs alongside the memory/search
    // injectors; it must merge into (never clobber) an existing `.mcp.json`.
    let cfg = tempdir().unwrap();
    let ws = tempdir().unwrap();
    seed_managed_registry(cfg.path());
    // Pretend the memory/search injectors already ran.
    std::fs::write(
        ws.path().join(".mcp.json"),
        r#"{"mcpServers":{"trusty-memory":{"command":"trusty-memory"},"trusty-search":{"command":"trusty-search"}}}"#,
    )
    .unwrap();

    inject_native_trusty_mcps_from(ws.path(), cfg.path()).expect("injection succeeds");

    let servers = read_injected(ws.path());
    assert!(servers.contains_key("trusty-memory"), "memory survives");
    assert!(servers.contains_key("trusty-search"), "search survives");
    assert!(servers.contains_key("slack-mcp"), "native added");
}

#[test]
fn inject_native_trust_preseed_covers_injected() {
    // Why (#2739 + #1296): the injected native names must flow into the workspace
    // trust preseed's `enabledMcpjsonServers` so Claude Code does not block the
    // fleet session on the "new MCP servers found" dialog. This mirrors the real
    // ordering in `prepare_session`: inject first, THEN preseed trust.
    let cfg = tempdir().unwrap();
    let ws_root = tempdir().unwrap();
    let workspace = ws_root.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let claude_json = ws_root.path().join(".claude.json");
    seed_managed_registry(cfg.path());

    inject_native_trusty_mcps_from(&workspace, cfg.path()).expect("injection succeeds");
    preseed_workspace_trust(&claude_json, &workspace).expect("trust preseed succeeds");

    let value: Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
    let key = workspace.to_string_lossy().to_string();
    let enabled: Vec<&str> = value["projects"][&key]["enabledMcpjsonServers"]
        .as_array()
        .expect("enabledMcpjsonServers is an array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        enabled.contains(&"slack-mcp"),
        "injected native flows into trust preseed"
    );
    assert!(enabled.contains(&"gworkspace-mcp"));
    assert!(
        !enabled.contains(&"github"),
        "non-native never reaches the workspace, so never trusted here"
    );
}

#[test]
fn inject_native_absent_config_is_noop() {
    // Why (#2739): a config dir with no `.claude.json` (fresh install, no
    // `tm mcp add` yet) must not error and must not create a workspace `.mcp.json`.
    let cfg = tempdir().unwrap();
    let ws = tempdir().unwrap();
    // Note: no seed — the managed dir has no `.claude.json`.

    inject_native_trusty_mcps_from(ws.path(), cfg.path()).expect("no-op succeeds");

    assert!(
        !ws.path().join(".mcp.json").exists(),
        "nothing to inject → no workspace .mcp.json written"
    );
}

#[test]
fn native_allowlist_excludes_dedicated_injector_servers() {
    // Why (#2739): trusty-memory / trusty-search have dedicated pinning injectors
    // and MUST NOT appear in this allowlist, or this injector would clobber their
    // palace/index pinning.
    assert!(!NATIVE_TRUSTY_MCP_SERVERS.contains(&"trusty-memory"));
    assert!(!NATIVE_TRUSTY_MCP_SERVERS.contains(&"trusty-search"));
    assert!(NATIVE_TRUSTY_MCP_SERVERS.contains(&"slack-mcp"));
    assert!(NATIVE_TRUSTY_MCP_SERVERS.contains(&"trusty-analyze"));
}

// ── split_public_and_secret_env: direct unit coverage ──────────────────────

#[test]
fn split_public_and_secret_env_separates_env_from_shape() {
    let entry = json!({
        "type": "stdio",
        "command": "slack-mcp",
        "args": ["serve"],
        "env": { "SLACK_BOT_TOKEN": FAKE_SLACK_TOKEN, "SLACK_USER_TOKEN": "xoxp-also-secret" }
    });

    let (public, secrets) = split_public_and_secret_env(&entry, "slack-mcp");

    assert!(
        public.get("env").is_none(),
        "env stripped from public shape"
    );
    assert_eq!(public["command"], json!("slack-mcp"));
    assert_eq!(public["args"], json!(["serve"]));
    assert_eq!(
        secrets.get("SLACK_BOT_TOKEN").map(String::as_str),
        Some(FAKE_SLACK_TOKEN)
    );
    assert_eq!(
        secrets.get("SLACK_USER_TOKEN").map(String::as_str),
        Some("xoxp-also-secret")
    );
}

#[test]
fn split_public_and_secret_env_skips_non_string_values() {
    // Why: the `env` schema is always Record<string,string>; a malformed
    // registry entry with a non-string value must never be silently coerced
    // into either output — it is dropped (with a warning) on both sides.
    let entry = json!({
        "type": "stdio",
        "command": "weird-mcp",
        "args": [],
        "env": { "GOOD": "value", "BAD": 42 }
    });

    let (public, secrets) = split_public_and_secret_env(&entry, "weird-mcp");

    assert!(public.get("env").is_none());
    assert_eq!(secrets.get("GOOD").map(String::as_str), Some("value"));
    assert!(!secrets.contains_key("BAD"), "non-string value dropped");
}

// ── ensure_env_local_git_excluded: direct unit coverage ─────────────────────

#[test]
fn ensure_env_local_git_excluded_is_idempotent() {
    let ws = tempdir().unwrap();
    git_init(ws.path());

    ensure_env_local_git_excluded(ws.path()).expect("first call succeeds");
    let exclude_path_1 = std::process::Command::new("git")
        .arg("-C")
        .arg(ws.path())
        .args(["rev-parse", "--git-path", "info/exclude"])
        .output()
        .unwrap();
    let path_1 = String::from_utf8_lossy(&exclude_path_1.stdout)
        .trim()
        .to_string();
    let content_1 = std::fs::read_to_string(ws.path().join(&path_1)).unwrap();

    ensure_env_local_git_excluded(ws.path()).expect("second call succeeds");
    let content_2 = std::fs::read_to_string(ws.path().join(&path_1)).unwrap();

    assert_eq!(content_1, content_2, "second call is a no-op");
    assert_eq!(
        content_1
            .lines()
            .filter(|l| l.trim() == ".env.local")
            .count(),
        1,
        ".env.local listed exactly once, not duplicated"
    );
}

#[test]
fn ensure_env_local_git_excluded_fails_outside_git_repo() {
    let ws = tempdir().unwrap();
    // No git_init: this must fail, not panic.
    let result = ensure_env_local_git_excluded(ws.path());
    assert!(result.is_err(), "a non-repo directory must return Err");
}

// ── merge_env_file: direct unit coverage ────────────────────────────────────

#[test]
fn merge_env_file_adds_new_keys() {
    let mut updates = BTreeMap::new();
    updates.insert("SLACK_BOT_TOKEN".to_string(), FAKE_SLACK_TOKEN.to_string());

    let merged = merge_env_file("", &updates);

    assert_eq!(merged, format!("SLACK_BOT_TOKEN=\"{FAKE_SLACK_TOKEN}\"\n"));
}

#[test]
fn merge_env_file_replaces_existing_key() {
    let mut updates = BTreeMap::new();
    updates.insert("SLACK_BOT_TOKEN".to_string(), "new-value".to_string());

    let merged = merge_env_file("SLACK_BOT_TOKEN=\"old-value\"\n", &updates);

    assert_eq!(merged, "SLACK_BOT_TOKEN=\"new-value\"\n");
}

#[test]
fn merge_env_file_preserves_unrelated_lines() {
    let mut updates = BTreeMap::new();
    updates.insert("SLACK_BOT_TOKEN".to_string(), FAKE_SLACK_TOKEN.to_string());

    let existing = "# operator's own comment\nOPENAI_API_KEY=\"sk-operator-owns-this\"\n";
    let merged = merge_env_file(existing, &updates);

    assert!(merged.contains("# operator's own comment"));
    assert!(merged.contains("OPENAI_API_KEY=\"sk-operator-owns-this\""));
    assert!(merged.contains(&format!("SLACK_BOT_TOKEN=\"{FAKE_SLACK_TOKEN}\"")));
}

#[test]
fn merge_env_file_is_idempotent_when_unchanged() {
    let mut updates = BTreeMap::new();
    updates.insert("SLACK_BOT_TOKEN".to_string(), FAKE_SLACK_TOKEN.to_string());

    let first = merge_env_file("", &updates);
    let second = merge_env_file(&first, &updates);

    assert_eq!(
        first, second,
        "re-merging already-merged content is a no-op"
    );
}
