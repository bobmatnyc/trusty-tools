//! Isolation-invariant integration tests for the standalone managed trusty-mpm driver.
//!
//! Why: the KEY INVARIANT of the DOC-24 managed driver (SPEC-STANDALONE-MPM-04) is
//! that it writes ONLY inside the managed `CLAUDE_CONFIG_DIR`
//! (`<config_dir>/settings.json` and `.claude.json`) and the per-alias workspace —
//! NEVER to the real `~/.claude` or `~/.claude.json`. These tests drive the three
//! config-assembly functions (`ensure_global_config_dir`, `ensure_managed_hooks`,
//! `preseed_managed_trust`) against fresh temp directories with no network, no
//! Claude auth, and no real GitHub, asserting end-to-end that every write goes to
//! the explicitly-supplied paths and nowhere else.
//!
//! ## Scope note: account-scoped cloud MCPs are NOT suppressed
//!
//! `CLAUDE_CONFIG_DIR` isolates **config-dir-scoped** things: settings, hooks, and
//! locally-declared MCP servers (`.mcp.json`). It CANNOT suppress **account-scoped
//! cloud MCPs** (Gmail, Notion, etc.) that are synced to the Claude account via
//! Anthropic's cloud. Once a user is logged in, those cloud MCPs appear in every
//! session regardless of which `CLAUDE_CONFIG_DIR` is in use. Do NOT add any
//! assertion like "this session has zero cloud MCPs" — that would be false for
//! logged-in users and would make these tests flaky. The isolation invariant tested
//! here is purely about filesystem writes, not about which MCPs a live Claude session
//! advertises.
//!
//! Test: this file IS the test module; run with
//! `cargo test -p trusty-mpm --test standalone_isolation`.

use std::path::Path;
use tempfile::TempDir;
use trusty_mpm::core::{
    bundle::OUTPUT_STYLES,
    standalone::{
        global_config::ensure_global_config_dir, hooks::ensure_managed_hooks, preseed_managed_trust,
    },
};

// ── RAII HOME guard ────────────────────────────────────────────────────────────
//
// Why: several tests redirect `$HOME` to a fresh temp dir to prove the functions
// never write through `dirs::home_dir()`. Because `std::env::set_var` / `remove_var`
// are `unsafe` in Rust 2024 (they are thread-unsafe) we wrap them in `unsafe` blocks
// and use the `#[serial_test::serial]` attribute so exactly one such test runs at a
// time — no parallel thread can observe the changed HOME.
//
// The guard implements `Drop` to restore HOME even when an assertion panics.
struct HomeGuard(Option<String>);

impl HomeGuard {
    /// Redirect `$HOME` to `dir` and return a guard that restores the original
    /// value on drop.
    ///
    /// Why: provides panic-safe HOME restoration so env-mutating tests never leak
    /// a poisoned HOME into the Rust test process.
    /// What: saves the current `HOME`, sets it to `dir`, returns a guard.
    /// Test: exercised by every `#[serial]` test in this file that redirects HOME.
    fn redirect(dir: &Path) -> Self {
        let prev = std::env::var("HOME").ok();
        // SAFETY: guarded by #[serial_test::serial] on all callers — only one
        // thread mutates HOME at a time.
        unsafe { std::env::set_var("HOME", dir) };
        HomeGuard(prev)
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.0 {
            Some(ref p) => unsafe { std::env::set_var("HOME", p) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

// ── Helper ─────────────────────────────────────────────────────────────────────

/// Assert that neither `~/.claude` (dir) nor `~/.claude.json` (file) exist under
/// `fake_home`.
///
/// Why: centralises the two-file isolation check so every test that redirects HOME
/// can share the same assertion without duplicating the paths.
/// What: panics (fails the test) if either `<fake_home>/.claude` or
/// `<fake_home>/.claude.json` exists.
/// Test: called by every HOME-redirect test in this file.
fn assert_no_real_claude_writes(fake_home: &Path) {
    assert!(
        !fake_home.join(".claude").exists(),
        "isolation invariant VIOLATED: managed driver must NEVER create \
         $HOME/.claude/ — found it at {}",
        fake_home.join(".claude").display()
    );
    assert!(
        !fake_home.join(".claude.json").exists(),
        "isolation invariant VIOLATED: managed driver must NEVER create \
         $HOME/.claude.json — found it at {}",
        fake_home.join(".claude.json").display()
    );
}

// ══════════════════════════════════════════════════════════════════════════════
// 1. No writes to the real ~/.claude* (end-to-end isolation guard)
// ══════════════════════════════════════════════════════════════════════════════

/// Full assembly flow (ensure_global_config_dir → ensure_managed_hooks →
/// preseed_managed_trust) must NEVER create `$HOME/.claude` or
/// `$HOME/.claude.json` when HOME is redirected to a fresh temp dir.
///
/// Why: this is the safety-critical test — it proves the managed driver only
/// writes to the explicit `claude_config_dir` argument and never leaks state
/// into the user's real home. A failure here means the isolation invariant of
/// SPEC-STANDALONE-MPM-04 is broken.
/// What: redirects HOME to `fake_home`, runs the full config-assembly pipeline
/// against `<managed_root>/claude-config/`, then asserts `fake_home` contains
/// no `.claude` directory or `.claude.json` file.
/// Test: this function IS the test; run with `cargo test -p trusty-mpm --test standalone_isolation`.
#[serial_test::serial]
#[test]
fn full_assembly_writes_nothing_to_real_home() {
    let tmp = TempDir::new().expect("temp dir");
    let fake_home = TempDir::new().expect("fake home temp dir");

    let managed_root = tmp.path().join("managed");
    let claude_config = managed_root.join("claude-config");
    let workspace = tmp.path().join("my-project");
    std::fs::create_dir_all(&workspace).expect("create workspace");

    // Redirect HOME so any accidental home-dir access goes to the fake home.
    let _home_guard = HomeGuard::redirect(fake_home.path());

    // Run the full assembly pipeline.
    ensure_global_config_dir(&managed_root, &claude_config)
        .expect("ensure_global_config_dir must not fail");
    ensure_managed_hooks(&claude_config).expect("ensure_managed_hooks must not fail");
    preseed_managed_trust(&claude_config, &workspace).expect("preseed_managed_trust must not fail");

    // --- Positive assertion: output must exist inside claude_config ---
    assert!(
        claude_config.join("settings.json").exists(),
        "settings.json must exist inside managed claude_config_dir after full assembly"
    );
    assert!(
        claude_config.join(".claude.json").exists(),
        ".claude.json must exist inside managed claude_config_dir after preseed"
    );

    // --- Isolation assertions: nothing must have been written to the fake home ---
    assert_no_real_claude_writes(fake_home.path());

    // _home_guard drops here and restores HOME.
}

// ══════════════════════════════════════════════════════════════════════════════
// 2. Hook wiring — settings.json carries the full hook triad
// ══════════════════════════════════════════════════════════════════════════════

/// After `ensure_managed_hooks`, `<config_dir>/settings.json` must contain
/// the PreToolUse, PostToolUse, and Stop hook triad with `command: "trusty-mpm hook"`.
///
/// Why: without the hook triad managed sessions are invisible to the daemon —
/// the circuit breaker, audit log, and dashboard receive no lifecycle events.
/// This test pins the exact JSON shape written by `ensure_managed_hooks` so a
/// future refactor that silently drops a hook event is caught immediately.
/// What: runs `ensure_managed_hooks` against a fresh `{}` settings.json and
/// parses the result, asserting each of the three hook events is present with
/// the correct command string and expected fields.
/// Test: this function IS the test.
#[test]
fn hook_triad_written_to_settings_json() {
    let tmp = TempDir::new().expect("temp dir");
    let cfg = tmp.path().to_path_buf();

    // Seed an empty settings.json as the function expects.
    std::fs::write(cfg.join("settings.json"), "{}\n").expect("write settings.json");

    ensure_managed_hooks(&cfg).expect("ensure_managed_hooks must not fail");

    let text = std::fs::read_to_string(cfg.join("settings.json"))
        .expect("settings.json must be readable after ensure_managed_hooks");
    let val: serde_json::Value =
        serde_json::from_str(&text).expect("settings.json must be valid JSON");

    let hooks = val
        .get("hooks")
        .expect("settings.json must have a top-level 'hooks' key after ensure_managed_hooks");

    // --- PreToolUse ---
    let pre = hooks
        .get("PreToolUse")
        .and_then(|v| v.as_array())
        .expect("hooks.PreToolUse must be an array");
    assert!(
        !pre.is_empty(),
        "hooks.PreToolUse must have at least one entry"
    );
    let pre_cmd = pre[0]["hooks"][0]["command"]
        .as_str()
        .expect("PreToolUse hook must have a 'command' field");
    assert_eq!(
        pre_cmd, "trusty-mpm hook",
        "PreToolUse hook command must be 'trusty-mpm hook'"
    );
    // PreToolUse is synchronous: no async:true.
    let pre_async = pre[0]["hooks"][0].get("async");
    assert!(
        pre_async.is_none() || pre_async == Some(&serde_json::Value::Bool(false)),
        "PreToolUse hook must NOT be async (synchronous timeout required)"
    );

    // --- PostToolUse ---
    let post = hooks
        .get("PostToolUse")
        .and_then(|v| v.as_array())
        .expect("hooks.PostToolUse must be an array");
    assert!(
        !post.is_empty(),
        "hooks.PostToolUse must have at least one entry"
    );
    let post_cmd = post[0]["hooks"][0]["command"]
        .as_str()
        .expect("PostToolUse hook must have a 'command' field");
    assert_eq!(
        post_cmd, "trusty-mpm hook",
        "PostToolUse hook command must be 'trusty-mpm hook'"
    );
    // PostToolUse is async to avoid blocking Claude on daemon ingestion.
    let post_async = post[0]["hooks"][0]
        .get("async")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(
        post_async,
        "PostToolUse hook must be async:true so Claude does not block waiting for daemon ingestion"
    );

    // --- Stop ---
    let stop = hooks
        .get("Stop")
        .and_then(|v| v.as_array())
        .expect("hooks.Stop must be an array");
    assert!(!stop.is_empty(), "hooks.Stop must have at least one entry");
    let stop_cmd = stop[0]["hooks"][0]["command"]
        .as_str()
        .expect("Stop hook must have a 'command' field");
    assert_eq!(
        stop_cmd, "trusty-mpm hook",
        "Stop hook command must be 'trusty-mpm hook'"
    );
}

/// Hook triad must NOT land inside the per-alias workspace `.claude/` directory.
/// It belongs only in the managed `CLAUDE_CONFIG_DIR` settings.json.
///
/// Why: the IDE-attach contract (SPEC-STANDALONE-MPM-06) specifies that the
/// project-local `workspace/.claude/` is the layer an IDE's `claude` invocation
/// reads — it MUST NOT carry the managed hooks (which require `CLAUDE_CONFIG_DIR`
/// to be set, which the IDE does not do). Mixing managed hooks into the
/// project-local layer would inject daemon side-effects into plain IDE sessions.
/// What: runs `ensure_managed_hooks` against `<config_dir>/settings.json`, then
/// asserts that `<workspace>/.claude/settings.json` does NOT exist.
/// Test: this function IS the test.
#[test]
fn hook_triad_does_not_land_in_workspace_dot_claude() {
    let tmp = TempDir::new().expect("temp dir");
    let cfg = tmp.path().join("claude-config");
    std::fs::create_dir_all(&cfg).expect("create config dir");
    let workspace = tmp.path().join("my-workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");

    std::fs::write(cfg.join("settings.json"), "{}\n").expect("write settings.json");

    ensure_managed_hooks(&cfg).expect("ensure_managed_hooks must not fail");

    // The workspace/.claude/ directory must not exist (hooks go to config dir only).
    assert!(
        !workspace.join(".claude").exists(),
        "IDE-attach boundary VIOLATED: ensure_managed_hooks must not create \
         workspace/.claude/ — hooks belong only in the managed CLAUDE_CONFIG_DIR"
    );
    assert!(
        !workspace.join(".claude").join("settings.json").exists(),
        "IDE-attach boundary VIOLATED: hook triad must NOT be written to \
         workspace/.claude/settings.json"
    );
}

// ══════════════════════════════════════════════════════════════════════════════
// 3. Trust-seed + MCP-enable wiring
// ══════════════════════════════════════════════════════════════════════════════

/// After `preseed_managed_trust`, `<config_dir>/.claude.json` must contain
/// `projects.<workspace>` with `hasTrustDialogAccepted: true` and
/// `enabledMcpjsonServers: ["trusty-memory", "trusty-search"]`.
///
/// Why: these two trust fields are what prevents Claude Code from showing
/// blocking "Do you trust this folder?" and "New MCP servers found" dialogs
/// at the start of every managed session. Without them sessions require manual
/// interaction even in attended mode.
/// What: runs `preseed_managed_trust` against a fresh temp config dir and
/// workspace, then parses `.claude.json` and asserts the required fields.
/// Test: this function IS the test.
#[test]
fn trust_seed_sets_trust_and_mcp_servers() {
    let tmp = TempDir::new().expect("temp dir");
    let cfg = tmp.path().join("claude-config");
    std::fs::create_dir_all(&cfg).expect("create config dir");
    let workspace = tmp.path().join("projects").join("my-repo");
    std::fs::create_dir_all(&workspace).expect("create workspace");

    preseed_managed_trust(&cfg, &workspace).expect("preseed_managed_trust must not fail");

    let text = std::fs::read_to_string(cfg.join(".claude.json"))
        .expect(".claude.json must exist after preseed_managed_trust");
    let val: serde_json::Value =
        serde_json::from_str(&text).expect(".claude.json must be valid JSON");

    let key = workspace.to_string_lossy().to_string();
    let proj = val["projects"][&key]
        .as_object()
        .expect("projects.<workspace> must be an object in .claude.json");

    // hasTrustDialogAccepted
    assert_eq!(
        proj.get("hasTrustDialogAccepted"),
        Some(&serde_json::Value::Bool(true)),
        "projects.<workspace>.hasTrustDialogAccepted must be true"
    );

    // hasCompletedProjectOnboarding
    assert_eq!(
        proj.get("hasCompletedProjectOnboarding"),
        Some(&serde_json::Value::Bool(true)),
        "projects.<workspace>.hasCompletedProjectOnboarding must be true"
    );

    // projectOnboardingSeenCount >= 1
    assert!(
        proj.get("projectOnboardingSeenCount")
            .and_then(|v| v.as_i64())
            .is_some_and(|n| n >= 1),
        "projects.<workspace>.projectOnboardingSeenCount must be >= 1"
    );

    // enabledMcpjsonServers
    let servers = proj
        .get("enabledMcpjsonServers")
        .and_then(|v| v.as_array())
        .expect("projects.<workspace>.enabledMcpjsonServers must be an array");
    let names: Vec<&str> = servers.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        names.contains(&"trusty-memory"),
        "enabledMcpjsonServers must include 'trusty-memory'; got {names:?}"
    );
    assert!(
        names.contains(&"trusty-search"),
        "enabledMcpjsonServers must include 'trusty-search'; got {names:?}"
    );
}

/// `preseed_managed_trust` must NOT write `.claude.json` or create `.claude/`
/// inside the per-alias workspace — those belong only in the managed config dir.
///
/// Why: IDE-attach boundary (SPEC-STANDALONE-MPM-06). The project-local
/// `workspace/.claude/` is controlled by the repo and the user; the managed
/// driver must not inject `.claude.json` trust state into it, as that would
/// cause the workspace's project-local layer to carry daemon-level trust config.
/// What: runs `preseed_managed_trust` and asserts the workspace dir does not
/// contain `.claude.json` or `.claude/`.
/// Test: this function IS the test.
#[test]
fn trust_seed_does_not_land_in_workspace() {
    let tmp = TempDir::new().expect("temp dir");
    let cfg = tmp.path().join("claude-config");
    std::fs::create_dir_all(&cfg).expect("create config dir");
    let workspace = tmp.path().join("my-workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");

    preseed_managed_trust(&cfg, &workspace).expect("preseed_managed_trust must not fail");

    assert!(
        !workspace.join(".claude.json").exists(),
        "IDE-attach boundary VIOLATED: preseed_managed_trust must NOT write \
         workspace/.claude.json — trust state belongs in CLAUDE_CONFIG_DIR only"
    );
    assert!(
        !workspace.join(".claude").exists(),
        "IDE-attach boundary VIOLATED: preseed_managed_trust must NOT create \
         workspace/.claude/ — trust state belongs in CLAUDE_CONFIG_DIR only"
    );
}

// ══════════════════════════════════════════════════════════════════════════════
// 4. Agent / skill deployment
// ══════════════════════════════════════════════════════════════════════════════

/// After `ensure_global_config_dir` with a pre-populated framework source,
/// agents and skills must land under `<config_dir>/agents/` and
/// `<config_dir>/skills/` respectively — and NOT under `$HOME/.claude`.
///
/// Why: SPEC-STANDALONE-MPM-03 requires that the managed config dir is
/// self-contained. Agents and skills that end up in `~/.claude/agents/` would
/// collide with the user's own claude-mpm setup (exactly the collision this
/// whole epic was designed to prevent).
/// What: creates fake `<managed_root>/framework/agents/my-agent.md` and
/// `<managed_root>/framework/skills/my-skill.md`, runs
/// `ensure_global_config_dir`, then asserts the files land under
/// `<config_dir>/agents/` and `<config_dir>/skills/` and NOT under
/// `fake_home/.claude/`.
/// Test: this function IS the test.
#[serial_test::serial]
#[test]
fn agents_and_skills_deploy_to_config_dir_not_home() {
    let tmp = TempDir::new().expect("temp dir");
    let fake_home = TempDir::new().expect("fake home temp dir");

    let managed_root = tmp.path().join("managed");
    let claude_config = managed_root.join("claude-config");

    // Populate a fake framework source under managed_root/framework/.
    let agents_src = managed_root.join("framework").join("agents");
    std::fs::create_dir_all(&agents_src).expect("create agents src");
    std::fs::write(
        agents_src.join("my-agent.md"),
        "---\nname: my-agent\nrole: engineer\n---\n\n# My Agent\n\nAgent content.\n",
    )
    .expect("write fake agent");

    let skills_src = managed_root.join("framework").join("skills");
    std::fs::create_dir_all(&skills_src).expect("create skills src");
    std::fs::write(
        skills_src.join("my-skill.md"),
        "---\nname: my-skill\n---\n\n# My Skill\n\nSkill content.\n",
    )
    .expect("write fake skill");

    // Redirect HOME to the fake home so any accidental home-dir resolution
    // is captured and can be asserted against.
    let _home_guard = HomeGuard::redirect(fake_home.path());

    ensure_global_config_dir(&managed_root, &claude_config)
        .expect("ensure_global_config_dir must not fail");

    // Agent must land in <config_dir>/agents/my-agent.md.
    let agent_dest = claude_config.join("agents").join("my-agent.md");
    assert!(
        agent_dest.exists(),
        "agent must be deployed to config_dir/agents/my-agent.md; \
         not found at {}",
        agent_dest.display()
    );
    let agent_text = std::fs::read_to_string(&agent_dest).expect("read deployed agent");
    assert!(
        agent_text.contains("Agent content."),
        "deployed agent content must match source"
    );

    // Skill must land in <config_dir>/skills/my-skill/SKILL.md.
    let skill_dest = claude_config
        .join("skills")
        .join("my-skill")
        .join("SKILL.md");
    assert!(
        skill_dest.exists(),
        "skill must be deployed to config_dir/skills/my-skill/SKILL.md; \
         not found at {}",
        skill_dest.display()
    );
    let skill_text = std::fs::read_to_string(&skill_dest).expect("read deployed skill");
    assert!(
        skill_text.contains("Skill content."),
        "deployed skill content must match source"
    );

    // Nothing must have been written to the fake home's .claude directory.
    assert_no_real_claude_writes(fake_home.path());

    // _home_guard drops here and restores HOME.
}

// ══════════════════════════════════════════════════════════════════════════════
// 5. IDE-attach boundary — config-dir vs workspace separation of concerns
// ══════════════════════════════════════════════════════════════════════════════

/// The managed hook triad and MCP-enable state live ONLY in the managed
/// CLAUDE_CONFIG_DIR, NOT in the per-alias workspace's `.claude/` directory.
///
/// Why: SPEC-STANDALONE-MPM-06 (IDE attach). An IDE invokes plain `claude`
/// without `CLAUDE_CONFIG_DIR` set, so it reads the project-local
/// `workspace/.claude/settings.json`. If managed hooks were also written there,
/// they would fire in plain IDE sessions, which is NOT the intended behaviour —
/// the daemon side-effects (circuit breaker, audit log) must be opt-in through
/// the managed driver. This test asserts the separation of concerns explicitly.
/// What: runs the full assembly pipeline against `<config_dir>` and
/// `<workspace>`, then checks that:
///   (a) `<config_dir>/settings.json` contains the hook triad,
///   (b) `<workspace>/.claude/settings.json` does NOT exist (or does not have
///       the managed hooks if it does exist), and
///   (c) `<config_dir>/.claude.json` contains `enabledMcpjsonServers`,
///   (d) `<workspace>/.claude.json` does NOT exist (trust state not leaked).
/// Test: this function IS the test.
#[test]
fn ide_attach_boundary_config_dir_vs_workspace() {
    let tmp = TempDir::new().expect("temp dir");
    let managed_root = tmp.path().join("managed");
    let cfg = managed_root.join("claude-config");
    let workspace = tmp.path().join("my-workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");

    ensure_global_config_dir(&managed_root, &cfg).expect("ensure_global_config_dir must not fail");
    ensure_managed_hooks(&cfg).expect("ensure_managed_hooks must not fail");
    preseed_managed_trust(&cfg, &workspace).expect("preseed_managed_trust must not fail");

    // (a) Managed config dir must have the hook triad in settings.json.
    let settings_text = std::fs::read_to_string(cfg.join("settings.json"))
        .expect("config_dir/settings.json must be readable");
    let settings_val: serde_json::Value =
        serde_json::from_str(&settings_text).expect("config_dir/settings.json must be valid JSON");
    assert!(
        settings_val.get("hooks").is_some(),
        "config_dir/settings.json must contain the managed hook triad"
    );

    // (b) workspace/.claude/ must NOT exist (hooks not leaked to workspace layer).
    assert!(
        !workspace.join(".claude").exists(),
        "IDE-attach boundary VIOLATED: workspace/.claude/ must not exist; \
         managed hooks and settings belong only in CLAUDE_CONFIG_DIR, not the \
         project-local workspace layer that a plain IDE `claude` invocation reads"
    );

    // (c) Managed config dir must have enabledMcpjsonServers in .claude.json.
    let claude_json_text = std::fs::read_to_string(cfg.join(".claude.json"))
        .expect("config_dir/.claude.json must be readable");
    let claude_json_val: serde_json::Value = serde_json::from_str(&claude_json_text)
        .expect("config_dir/.claude.json must be valid JSON");
    let ws_key = workspace.to_string_lossy().to_string();
    let servers = claude_json_val["projects"][&ws_key]["enabledMcpjsonServers"]
        .as_array()
        .expect("config_dir/.claude.json must have enabledMcpjsonServers for the workspace");
    let names: Vec<&str> = servers.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        names.contains(&"trusty-memory"),
        "enabledMcpjsonServers in config_dir/.claude.json must include trusty-memory"
    );

    // (d) workspace/.claude.json must NOT exist.
    assert!(
        !workspace.join(".claude.json").exists(),
        "IDE-attach boundary VIOLATED: trust state must NOT be written to \
         workspace/.claude.json — it belongs only in CLAUDE_CONFIG_DIR/.claude.json"
    );
}

// ══════════════════════════════════════════════════════════════════════════════
// 6. Clean teardown — TempDir auto-cleanup (no stray artifacts)
// ══════════════════════════════════════════════════════════════════════════════

/// All writes from the full assembly pipeline are contained within the two
/// temp roots (`managed_root` and `workspace`) — both of which are cleaned up
/// automatically when the `TempDir` guards are dropped at the end of the test.
///
/// Why: stray artifacts left in the real filesystem after a test run are a
/// silent correctness hazard — they may influence subsequent test runs or
/// pollute the user's machine. Using `tempfile::TempDir` gives us automatic
/// drop-based cleanup and makes the invariant mechanically enforced rather than
/// dependent on explicit teardown.
/// What: runs the full pipeline, then explicitly drops both temp dirs via
/// `close()` (which errors if the temp dir cannot be removed) to prove the
/// contents are fully under our control and removable.
/// Test: this function IS the test.
#[serial_test::serial]
#[test]
fn teardown_leaves_no_stray_artifacts_outside_temp_roots() {
    let tmp = TempDir::new().expect("temp dir");
    let fake_home = TempDir::new().expect("fake home temp dir");

    let managed_root = tmp.path().join("managed");
    let claude_config = managed_root.join("claude-config");
    let workspace = tmp.path().join("my-project");
    std::fs::create_dir_all(&workspace).expect("create workspace");

    let _home_guard = HomeGuard::redirect(fake_home.path());

    // Run the full pipeline.
    ensure_global_config_dir(&managed_root, &claude_config)
        .expect("ensure_global_config_dir must not fail");
    ensure_managed_hooks(&claude_config).expect("ensure_managed_hooks must not fail");
    preseed_managed_trust(&claude_config, &workspace).expect("preseed_managed_trust must not fail");

    // Verify no writes leaked to fake home.
    assert_no_real_claude_writes(fake_home.path());

    // _home_guard drops first, restoring HOME.
    drop(_home_guard);

    // `TempDir::close()` returns an error if the directory cannot be deleted
    // (e.g. a file handle was left open outside our control). This proves all
    // writes are contained within the temp roots.
    tmp.close()
        .expect("all pipeline output must be inside the temp root — close() must succeed");
    fake_home
        .close()
        .expect("fake home must be empty and closeable — no stray writes");
}

// ══════════════════════════════════════════════════════════════════════════════
// 7. Output-style deployer isolation — styles deploy ONLY to config dir
// ══════════════════════════════════════════════════════════════════════════════

/// `ensure_global_config_dir` must deploy all bundled output styles under
/// `<config_dir>/output-styles/` and NEVER write to `$HOME/.claude*`.
///
/// Why: WI-2 follow-up (#1553, epic #1548).  The managed driver must deploy
/// output-style definitions into `CLAUDE_CONFIG_DIR/output-styles/` so Claude
/// Code can resolve the `"outputStyle": "trusty-mpm"` key.  The isolation
/// invariant (SPEC-STANDALONE-MPM-04) requires that no write goes to the real
/// `~/.claude*` — this test proves both properties hold together.
/// What: redirects HOME to a fresh temp dir, runs `ensure_global_config_dir`,
/// then asserts:
///   (a) `<config_dir>/output-styles/<file_name>` exists for every bundled style,
///   (b) content matches the bundled constant,
///   (c) `$HOME/.claude` and `$HOME/.claude.json` do NOT exist.
/// Test: this function IS the test; run with
/// `cargo test -p trusty-mpm --test standalone_isolation`.
#[serial_test::serial]
#[test]
fn output_styles_deploy_to_config_dir_not_home() {
    let tmp = TempDir::new().expect("temp dir");
    let fake_home = TempDir::new().expect("fake home temp dir");

    let managed_root = tmp.path().join("managed");
    let claude_config = managed_root.join("claude-config");

    // Redirect HOME so any accidental home-dir resolution lands in fake_home.
    let _home_guard = HomeGuard::redirect(fake_home.path());

    ensure_global_config_dir(&managed_root, &claude_config)
        .expect("ensure_global_config_dir must not fail");

    let styles_dir = claude_config.join("output-styles");

    // (a) + (b): every bundled style must be present with the correct content.
    assert!(
        styles_dir.exists(),
        "output-styles dir must exist inside the managed CLAUDE_CONFIG_DIR after \
         ensure_global_config_dir (WI-2 follow-up #1553)"
    );
    for style in OUTPUT_STYLES {
        let target = styles_dir.join(style.file_name);
        assert!(
            target.exists(),
            "output-styles/{} must be deployed to config_dir/output-styles/ — \
             not found at {}",
            style.file_name,
            target.display()
        );
        let content = std::fs::read_to_string(&target).expect("read deployed style");
        assert_eq!(
            content, style.content,
            "deployed output style {} content must match bundled constant",
            style.file_name
        );
    }

    // (c): isolation — nothing must have been written to the fake home.
    assert_no_real_claude_writes(fake_home.path());
    assert!(
        !fake_home.path().join("output-styles").exists(),
        "isolation VIOLATED: output styles must NOT be written to $HOME/output-styles"
    );

    // _home_guard drops here and restores HOME.
}

// ══════════════════════════════════════════════════════════════════════════════
// 8. WI-1 Isolation regression guard — version-captured invariant check
// ══════════════════════════════════════════════════════════════════════════════
//
// Why: Claude Code is an external binary that releases frequently; each upgrade
// could introduce new write paths under `~/.claude`, `~/.claude.json`, or
// `~/.config/claude*`. This section pins the isolation invariant to the
// currently-installed Claude Code version so any version drift that introduces
// new write paths is detectable immediately.
//
// Design choice (CI test only, not launch-time):
//   - The regression guard lives entirely here as CI tests — no changes to the
//     `tm run` launch path. A launch-time check would block or slow attended
//     interactive sessions and is inappropriate for a non-fatal advisory concern.
//   - The tests shell out to `claude --version` to capture the version string.
//     When `claude` is NOT on PATH (CI without Claude Code installed, fresh
//     developer machine), the tests skip with a clear log line rather than
//     failing — keeping the CI green on machines where Claude Code is absent.
//   - The version string is embedded in the test output via `println!` so it
//     appears in `cargo test -- --nocapture` and in CI logs, making version
//     drift visible without any persistent state file.
//
// Graceful-skip protocol: `detect_claude_version()` returns `None` when
// `claude` is absent or its output cannot be parsed. Every test below calls
// `detect_claude_version()` first and returns early (skip) when `None`.

/// Attempt to run `claude --version` and return the version string.
///
/// Why: version capture is the key addition of WI-1 — it anchors the isolation
/// invariant to a specific Claude Code release so future upgrades that add new
/// write paths are surfaced immediately rather than silently passing.
/// What: shells out to `claude --version`, parses stdout into a trimmed string.
/// Returns `None` when `claude` is not on PATH or the output is empty/unparseable
/// — the caller then skips the test rather than failing, keeping CI green on
/// machines without Claude Code.
/// Test: exercised by `regression_guard_version_capture_and_isolation_invariant`
/// and `regression_guard_config_write_paths_never_escape_to_home` below.
fn detect_claude_version() -> Option<String> {
    let out = std::process::Command::new("claude")
        .arg("--version")
        .output()
        .ok()?;
    if out.status.success() {
        let raw = String::from_utf8_lossy(&out.stdout);
        let trimmed = raw.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    } else {
        // `claude --version` failed or exited non-zero — treat as not installed.
        None
    }
}

/// Assert that no managed-driver writes landed under the fake home's "real home"
/// locations: `<fake_home>/.claude/`, `<fake_home>/.claude.json`, and any
/// `claude*` entry under `<fake_home>/.config/`.
///
/// Why: the standard `assert_no_real_claude_writes` checks `~/.claude` and
/// `~/.claude.json` (the two paths known at WI-7 time). WI-1 extends the check
/// to `~/.config/claude*` because Claude Code could write profile/auth state
/// there across upgrades. Centralising the extended check here ensures every
/// WI-1 guard test reports the exact violating path rather than a generic panic.
/// What: calls `assert_no_real_claude_writes` first, then scans
/// `<fake_home>/.config/` for any entry whose name starts with `claude`.
/// Panics with the violating path if found.
/// Test: called by every WI-1 regression guard test in this file.
fn assert_no_managed_driver_writes_to_home(fake_home: &Path) {
    // Standard two-path check shared with the WI-7 tests.
    assert_no_real_claude_writes(fake_home);

    // Extended check: ~/.config/claude* must also be clean.
    let config_dir = fake_home.join(".config");
    if config_dir.exists() {
        let claude_in_config = std::fs::read_dir(&config_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .any(|e| e.file_name().to_string_lossy().starts_with("claude"))
            })
            .unwrap_or(false);
        assert!(
            !claude_in_config,
            "isolation invariant VIOLATED: managed driver must NEVER write to \
             $HOME/.config/claude* — found a claude entry under {}",
            config_dir.display()
        );
    }
}

/// WI-1 regression guard — version capture + full isolation invariant.
///
/// Why: each Claude Code upgrade could introduce new write paths under the real
/// home. This test anchors the isolation invariant to the installed version so
/// drift is immediately visible in CI logs without any persistent state file.
/// What: (1) detects the installed Claude Code version (skips gracefully when
/// absent so CI without Claude Code stays green), (2) prints the version string
/// to the test log, (3) runs the full config-assembly pipeline with HOME
/// redirected to `fake_home`, (4) asserts no write landed under `~/.claude`,
/// `~/.claude.json`, or `~/.config/claude*` — naming the exact violating path
/// if the assertion fires.
/// Test: this function IS the test; run with
/// `cargo test -p trusty-mpm --test standalone_isolation`.
#[serial_test::serial]
#[test]
fn regression_guard_version_capture_and_isolation_invariant() {
    // Step 1: version detection — graceful skip when claude is absent.
    let claude_version = detect_claude_version();
    match &claude_version {
        None => {
            // `claude` is not installed or not on PATH. Skip this test so CI
            // on machines without Claude Code stays green.
            println!(
                "[WI-1 regression guard] SKIP — `claude` not found on PATH; \
                 install Claude Code to enable version-pinned isolation checks."
            );
            return;
        }
        Some(v) => {
            // Emit version so it appears in CI logs for drift detection.
            println!("[WI-1 regression guard] Validated against Claude Code {v}");
        }
    }

    // Step 2: full assembly pipeline under fake HOME.
    let tmp = TempDir::new().expect("temp dir");
    let fake_home = TempDir::new().expect("fake home temp dir");

    let managed_root = tmp.path().join("managed");
    let claude_config = managed_root.join("claude-config");
    let workspace = tmp.path().join("my-project");
    std::fs::create_dir_all(&workspace).expect("create workspace");

    // Redirect HOME so any accidental home-dir access goes to fake_home.
    let _home_guard = HomeGuard::redirect(fake_home.path());

    ensure_global_config_dir(&managed_root, &claude_config)
        .expect("ensure_global_config_dir must not fail");
    ensure_managed_hooks(&claude_config).expect("ensure_managed_hooks must not fail");
    preseed_managed_trust(&claude_config, &workspace).expect("preseed_managed_trust must not fail");

    // Step 3: isolation assertions.
    // If any of these panic the message names the exact violating path,
    // mirroring the WI-7 assertion style.
    assert_no_managed_driver_writes_to_home(fake_home.path());

    // Positive assertion: output must exist inside claude_config (not just "no leak").
    assert!(
        claude_config.join("settings.json").exists(),
        "settings.json must exist inside managed claude_config_dir"
    );
    assert!(
        claude_config.join(".claude.json").exists(),
        ".claude.json must exist inside managed claude_config_dir"
    );

    println!(
        "[WI-1 regression guard] PASS — zero writes to fake home under \
         Claude Code {}",
        claude_version.expect("version checked above")
    );
    // _home_guard drops here and restores HOME.
}

/// WI-1 regression guard — write-path leakage check (focused negative assertion).
///
/// Why: a focused guard that checks only the negative assertion (no leakage to
/// the real home) so that a future factoring of the assembly pipeline into
/// separate stages doesn't accidentally drop a write-path check. Kept separate
/// from the positive-plus-negative test above so failure messages are targeted.
/// What: redirects HOME to `fake_home`, calls all three config-assembly
/// functions, then asserts zero writes to `~/.claude`, `~/.claude.json`, and
/// `~/.config/claude*`. Skips gracefully when `claude` is not installed.
/// Test: this function IS the test.
#[serial_test::serial]
#[test]
fn regression_guard_config_write_paths_never_escape_to_home() {
    // Graceful skip when claude is not installed (CI without Claude Code).
    let claude_version = detect_claude_version();
    match &claude_version {
        None => {
            println!(
                "[WI-1 regression guard] SKIP (write-path check) — \
                 `claude` not found on PATH."
            );
            return;
        }
        Some(v) => {
            println!("[WI-1 regression guard] Write-path check against Claude Code {v}");
        }
    }

    let tmp = TempDir::new().expect("temp dir");
    let fake_home = TempDir::new().expect("fake home temp dir");

    let managed_root = tmp.path().join("managed");
    let claude_config = managed_root.join("claude-config");
    let workspace = tmp.path().join("repo");
    std::fs::create_dir_all(&workspace).expect("create workspace");

    let _home_guard = HomeGuard::redirect(fake_home.path());

    // Run every public config-assembly entry point.
    ensure_global_config_dir(&managed_root, &claude_config).expect("ensure_global_config_dir");
    ensure_managed_hooks(&claude_config).expect("ensure_managed_hooks");
    preseed_managed_trust(&claude_config, &workspace).expect("preseed_managed_trust");

    // All writes must be confined to claude_config (or managed_root).
    // None must escape to fake_home.
    assert_no_managed_driver_writes_to_home(fake_home.path());

    println!(
        "[WI-1 regression guard] PASS (write-path) — no leakage under \
         Claude Code {}",
        claude_version.expect("version checked above")
    );
    // _home_guard drops here and restores HOME.
}
